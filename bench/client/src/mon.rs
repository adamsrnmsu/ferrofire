//! `ffbench mon`: run inside the provider's network + PID namespaces. Captures every frame on all interfaces with an
//! AF_PACKET socket (frame length = L2 header + IP + TCP/UDP + payload; no preamble/FCS) and counts it by class,
//! and samples the provider process (PID 1) CPU ticks and RSS. One CSV row of cumulative counters per interval.
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

static STOP: AtomicBool = AtomicBool::new(false);
extern "C" fn on_term(_: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

/// 0 = request/reply (tcp 5555), 1 = publish (tcp 5556 nng, udp 40000..41999 datagram v1), 2 = other (ARP, IGMP, ...).
pub fn classify(f: &[u8]) -> usize {
    if f.len() < 34 || f[12..14] != [8, 0] {
        return 2;
    }
    let ihl = (f[14] & 15) as usize * 4;
    let l4 = 14 + ihl;
    if f.len() < l4 + 4 {
        return 2;
    }
    let (sp, dp) = (u16::from_be_bytes([f[l4], f[l4 + 1]]), u16::from_be_bytes([f[l4 + 2], f[l4 + 3]]));
    match f[23] {
        6 if sp == 5555 || dp == 5555 => 0,
        6 if sp == 5556 || dp == 5556 => 1,
        17 if (40000..42000).contains(&dp) || (40000..42000).contains(&sp) => 1, // datagram v1: whole-model 40000, per-RTU 41000+
        _ => 2,
    }
}

fn proc_sample() -> (u64, u64) {
    let stat = std::fs::read_to_string("/proc/1/stat").unwrap_or_default();
    let rest = stat.rsplit_once(')').map(|x| x.1).unwrap_or("");
    let f: Vec<&str> = rest.split_whitespace().collect();
    let ticks = f.get(11).and_then(|x| x.parse::<u64>().ok()).unwrap_or(0) + f.get(12).and_then(|x| x.parse::<u64>().ok()).unwrap_or(0);
    let status = std::fs::read_to_string("/proc/1/status").unwrap_or_default();
    let rss = status.lines().find(|l| l.starts_with("VmRSS:")).and_then(|l| l.split_whitespace().nth(1)?.parse().ok()).unwrap_or(0);
    (ticks, rss)
}

pub fn run(args: &[String]) {
    let out = crate::arg(args, "--out", "mon.csv");
    let every: u64 = crate::arg(args, "--ms", "250").parse().unwrap();
    unsafe {
        libc::signal(libc::SIGTERM, on_term as *const () as usize);
        libc::signal(libc::SIGINT, on_term as *const () as usize);
        let fd = libc::socket(libc::AF_PACKET, libc::SOCK_RAW, (libc::ETH_P_ALL as u16).to_be() as i32);
        assert!(fd >= 0, "AF_PACKET socket: {}", std::io::Error::last_os_error());
        let tv = libc::timeval { tv_sec: 0, tv_usec: 20_000 };
        libc::setsockopt(fd, libc::SOL_SOCKET, libc::SO_RCVTIMEO, &tv as *const _ as *const libc::c_void, std::mem::size_of::<libc::timeval>() as u32);
        let big: libc::c_int = 16 << 20;
        libc::setsockopt(fd, libc::SOL_SOCKET, libc::SO_RCVBUF, &big as *const _ as *const libc::c_void, 4);
        let mut f = std::fs::File::create(&out).unwrap();
        writeln!(f, "t_ms,cpu_ticks,rss_kb,req_bytes,req_pkts,pub_bytes,pub_pkts,oth_bytes,oth_pkts,max_frame,drops").unwrap();
        let (mut b, mut p, mut maxf, mut drops) = ([0u64; 3], [0u64; 3], 0usize, 0u64);
        let mut buf = vec![0u8; 65536];
        let mut next = 0u64;
        loop {
            let mut sa: libc::sockaddr_ll = std::mem::zeroed();
            let mut sl = std::mem::size_of::<libc::sockaddr_ll>() as u32;
            let n = libc::recvfrom(fd, buf.as_mut_ptr() as *mut _, buf.len(), 0, &mut sa as *mut _ as *mut libc::sockaddr, &mut sl);
            // The provider only sends multicast, so an IPv4 multicast frame that is not outgoing is the kernel's own
            // IP_MULTICAST_LOOP copy of a datagram it just sent (a first run counted every datagram twice). Count it once, on transmit.
            let mcast_dst = n > 30 && buf[12..14] == [8, 0] && buf[30] >= 224;
            let dup = sa.sll_pkttype == libc::PACKET_LOOPBACK || (mcast_dst && sa.sll_pkttype != libc::PACKET_OUTGOING);
            if n > 0 && !dup {
                let c = classify(&buf[..n as usize]);
                b[c] += n as u64;
                p[c] += 1;
                maxf = maxf.max(n as usize);
            }
            let t = crate::now_ns() / 1_000_000;
            let stop = STOP.load(Ordering::SeqCst);
            if t >= next || stop {
                let mut st: libc::tpacket_stats = std::mem::zeroed();
                let mut l = std::mem::size_of::<libc::tpacket_stats>() as u32;
                libc::getsockopt(fd, libc::SOL_PACKET, libc::PACKET_STATISTICS, &mut st as *mut _ as *mut libc::c_void, &mut l);
                drops += st.tp_drops as u64;
                let (ticks, rss) = proc_sample();
                writeln!(f, "{t},{ticks},{rss},{},{},{},{},{},{},{maxf},{drops}", b[0], p[0], b[1], p[1], b[2], p[2]).unwrap();
                f.flush().unwrap();
                next = t + every;
            }
            if stop {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::classify;
    fn frame(proto: u8, sp: u16, dp: u16) -> Vec<u8> {
        let mut f = vec![0u8; 60];
        f[12..14].copy_from_slice(&[8, 0]);
        f[14] = 0x45;
        f[23] = proto;
        f[34..36].copy_from_slice(&sp.to_be_bytes());
        f[36..38].copy_from_slice(&dp.to_be_bytes());
        f
    }
    #[test]
    fn classes() {
        assert_eq!(classify(&frame(6, 40000, 5555)), 0);
        assert_eq!(classify(&frame(6, 5556, 33000)), 1);
        assert_eq!(classify(&frame(17, 50000, 40000)), 1);
        assert_eq!(classify(&frame(17, 50000, 41007)), 1);
        assert_eq!(classify(&frame(17, 50000, 53)), 2);
        assert_eq!(classify(&[0u8; 20]), 2);
    }
}
