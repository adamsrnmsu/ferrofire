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
