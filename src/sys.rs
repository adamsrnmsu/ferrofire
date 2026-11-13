//! The only unsafe outside `simulink::ipc`: small safe wrappers over the libc calls the CLI needs (signals,
//! kill, fork/dup2, setsockopt, getuid). Every other module is `#![forbid]`-style `deny(unsafe_code)`.
#![allow(unsafe_code)]
#![deny(unsafe_op_in_unsafe_fn)]

use std::os::fd::AsRawFd;

/// Route SIGINT and SIGTERM to `handler`. Invariant: `handler` is async-signal-safe (only stores an atomic).
pub fn on_stop_signals(handler: extern "C" fn(libc::c_int)) {
    // SAFETY: signal(2) with a valid extern "C" fn pointer; no memory is touched.
    unsafe {
        libc::signal(libc::SIGINT, handler as *const () as usize);
        libc::signal(libc::SIGTERM, handler as *const () as usize);
    }
}

/// kill(2); true when it succeeded (pid exists / signal delivered).
pub fn kill(pid: i32, sig: i32) -> bool {
    // SAFETY: plain syscall on integers.
    unsafe { libc::kill(pid, sig) == 0 }
}

pub fn getuid() -> u32 {
    // SAFETY: getuid cannot fail and takes no pointers.
    unsafe { libc::getuid() }
}

/// Classic double fork: returns in the grandchild only (parents `exit(0)`, not `_exit`, so coverage flushes).
/// Invariant: call before any threads exist, since fork copies only the calling thread.
pub fn double_fork_detach() {
    // SAFETY: fork/setsid/chdir/umask take no pointers beyond a static C string.
    unsafe {
        if libc::fork() > 0 {
            std::process::exit(0);
        }
        libc::setsid();
        if libc::fork() > 0 {
            std::process::exit(0);
        }
        libc::chdir(c"/".as_ptr());
        libc::umask(0);
    }
}

/// dup2(from, to): make `to` refer to `from`'s file.
pub fn dup2(from: &impl AsRawFd, to: i32) {
    // SAFETY: both descriptors are plain ints; `from` is open for the duration of the borrow.
    unsafe {
        libc::dup2(from.as_raw_fd(), to);
    }
}

/// IP_MULTICAST_IF: send multicast datagrams out of the interface that owns `a`.
pub fn set_multicast_if(s: &std::net::UdpSocket, a: std::net::Ipv4Addr) -> Result<(), String> {
    let ia = libc::in_addr {
        s_addr: u32::from(a).to_be(),
    };
    // SAFETY: `ia` outlives the call and the length passed is exactly its size; the fd is live (borrowed).
    let rc = unsafe {
        libc::setsockopt(
            s.as_raw_fd(),
            libc::IPPROTO_IP,
            libc::IP_MULTICAST_IF,
            &ia as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::in_addr>() as u32,
        )
    };
    match rc {
        0 => Ok(()),
        _ => Err(format!(
            "IP_MULTICAST_IF {a}: {}",
            std::io::Error::last_os_error()
        )),
    }
}

/// The first address in `ifs` (interface flags, IPv4 address) of an interface that is up and not loopback.
pub fn pick_up_ipv4(
    ifs: impl IntoIterator<Item = (u32, std::net::Ipv4Addr)>,
) -> Option<std::net::Ipv4Addr> {
    let (up, lo) = (libc::IFF_UP as u32, libc::IFF_LOOPBACK as u32);
    ifs.into_iter()
        .find(|(f, _)| f & up != 0 && f & lo == 0)
        .map(|(_, a)| a)
}

/// What libzmq's udp_engine binds a `*` interface to: the first non-loopback IPv4 address that is up.
pub fn first_up_ipv4() -> Option<std::net::Ipv4Addr> {
    let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills `head` with a list we only read until freeifaddrs; every pointer is checked for null
    // and an AF_INET entry's ifa_addr points at a sockaddr_in.
    unsafe {
        if libc::getifaddrs(&mut head) != 0 {
            return None;
        }
        let mut v = Vec::new();
        let mut p = head;
        while let Some(i) = p.as_ref() {
            if let Some(a) = i
                .ifa_addr
                .as_ref()
                .filter(|a| a.sa_family as i32 == libc::AF_INET)
            {
                let sin = &*(a as *const libc::sockaddr as *const libc::sockaddr_in);
                v.push((
                    i.ifa_flags,
                    std::net::Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr)),
                ));
            }
            p = i.ifa_next;
        }
        libc::freeifaddrs(head);
        pick_up_ipv4(v)
    }
}

/// Wait up to `d` for `s` to accept a send (poll POLLOUT); false on timeout or error.
pub fn wait_writable(s: &std::net::UdpSocket, d: std::time::Duration) -> bool {
    let mut p = libc::pollfd {
        fd: s.as_raw_fd(),
        events: libc::POLLOUT,
        revents: 0,
    };
    let ms = d.as_micros().div_ceil(1000).min(i32::MAX as u128) as i32;
    // SAFETY: one live pollfd and nfds = 1.
    unsafe { libc::poll(&mut p, 1, ms) == 1 && p.revents & libc::POLLOUT != 0 }
}

/// Seconds east of UTC the local clock is at `unix`.
pub fn utc_offset(unix: i64) -> i64 {
    let t = unix as libc::time_t;
    // SAFETY: localtime_r writes only into the zeroed struct we pass, from a valid time_t.
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        tm.tm_gmtoff
    }
}
