//! A port for a test's server, shared by every test binary (ww-20t, ww-1mu).
#![allow(dead_code)]

/// A port nothing listens on, never one this process was given before, and never one the kernel hands out for `bind(":0")`: it
/// gives a port it has just been given back out again, and a test's port is not bound until its device starts, so two tests
/// running at once (or one with a held listener) could get the same one and talk to each other's servers. These count up from a
/// start that differs per test process, below the kernel's own range (32768 and up), and are free for both TCP and UDP. Two
/// processes' ranges can overlap: fine while cargo test and llvm-cov run one test binary at a time, not under nextest or parallel
/// binaries.
pub fn free_port() -> u16 {
    static NEXT: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(0);
    let start = 12_000 + (std::process::id() % 18_000) as u16;
    loop {
        let port = start + NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
            && std::net::UdpSocket::bind(("127.0.0.1", port)).is_ok()
        {
            return port;
        }
    }
}
