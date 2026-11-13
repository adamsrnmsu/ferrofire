//! ww-1df.27: a flat VLAN host has an interface address and nothing else (no 224/4 route, no default route).
//! The RADIO publisher on `udp://*;...` must still get its datagrams out, as libzmq does, by picking the first
//! non-loopback IPv4 address for IP_MULTICAST_IF. Needs CAP_SYS_ADMIN (a private network namespace) and /dev/net/tun:
//! `make netns` runs it in a privileged container; anywhere else it prints a skip line and passes.
#![cfg(target_os = "linux")]
#![allow(unsafe_code)]
use ferrofire::transport::publisher;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

fn ifreq(name: &str) -> libc::ifreq {
    // SAFETY: ifreq is plain old data; all-zero is a valid value.
    let mut r: libc::ifreq = unsafe { std::mem::zeroed() };
    for (d, s) in r.ifr_name.iter_mut().zip(name.bytes()) {
        *d = s as libc::c_char;
    }
    r
}

fn sin(a: [u8; 4]) -> libc::sockaddr {
    let s = libc::sockaddr_in {
        sin_family: libc::AF_INET as libc::sa_family_t,
        sin_port: 0,
        sin_addr: libc::in_addr {
            s_addr: u32::from_ne_bytes(a),
        },
        sin_zero: [0; 8],
    };
    // SAFETY: sockaddr and sockaddr_in are both 16 bytes of plain data.
    unsafe { std::mem::transmute(s) }
}

/// A TUN device `ffmc0` holding 10.9.9.1/24 and up: an interface address, a connected /24 route, nothing else.
/// None when the sandbox does not allow it.
fn tun_in_private_netns() -> Option<OwnedFd> {
    // SAFETY: plain syscalls on ints and on structs that live across each call.
    unsafe {
        if libc::unshare(libc::CLONE_NEWNET) != 0 {
            return None;
        }
        let fd = libc::open(c"/dev/net/tun".as_ptr(), libc::O_RDWR | libc::O_NONBLOCK);
        if fd < 0 {
            return None;
        }
        let tun = OwnedFd::from_raw_fd(fd);
        let mut r = ifreq("ffmc0");
        r.ifr_ifru.ifru_flags = (libc::IFF_TUN | libc::IFF_NO_PI) as libc::c_short;
        if libc::ioctl(tun.as_raw_fd(), libc::TUNSETIFF as _, &mut r) != 0 {
            return None;
        }
        let ctl = OwnedFd::from_raw_fd(libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0));
        let mut a = ifreq("ffmc0");
        a.ifr_ifru.ifru_addr = sin([10, 9, 9, 1]);
        let mut m = ifreq("ffmc0");
        m.ifr_ifru.ifru_netmask = sin([255, 255, 255, 0]);
        let mut f = ifreq("ffmc0");
        f.ifr_ifru.ifru_flags = (libc::IFF_UP | libc::IFF_RUNNING) as libc::c_short;
        let ok = libc::ioctl(ctl.as_raw_fd(), libc::SIOCSIFADDR as _, &mut a) == 0
            && libc::ioctl(ctl.as_raw_fd(), libc::SIOCSIFNETMASK as _, &mut m) == 0
            && libc::ioctl(ctl.as_raw_fd(), libc::SIOCSIFFLAGS as _, &mut f) == 0;
        ok.then_some(tun)
    }
}

fn read_packet(tun: &OwnedFd) -> Option<Vec<u8>> {
    let mut p = libc::pollfd {
        fd: tun.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let mut buf = vec![0u8; 2048];
    // SAFETY: `p` and `buf` outlive the calls; the length passed is buf's.
    unsafe {
        if libc::poll(&mut p, 1, 2000) != 1 {
            return None;
        }
        let n = libc::read(tun.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len());
        (n > 0).then(|| buf[..n as usize].to_vec())
    }
}

#[test]
fn wildcard_publish_reaches_the_only_interface_without_a_multicast_or_default_route() {
    let Some(tun) = tun_in_private_netns() else {
        eprintln!("skip: needs CAP_SYS_ADMIN and /dev/net/tun (make netns)");
        return;
    };
    let p = publisher("udp://*;239.0.0.1:40000").unwrap();
    p.publish("a:1.5,");
    assert_eq!(p.dropped(), 0, "the send must not fail with ENETUNREACH");
    let pkt = read_packet(&tun).expect("a datagram left through the interface");
    assert_eq!(pkt[0] >> 4, 4, "IPv4");
    assert_eq!(
        pkt[12..16],
        [10, 9, 9, 1],
        "source is the interface address"
    );
    assert_eq!(pkt[16..20], [239, 0, 0, 1], "destination is the group");
}
