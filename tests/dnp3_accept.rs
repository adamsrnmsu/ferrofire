//! bennu #81: an `accept()` that fails (out of descriptors) must not end the listener. This test lowers the descriptor limit,
//! so it lives alone in its own test binary (the limit is process-wide).

mod common;

use ferrofire::fd::proto::dnp3::link::{self, Parse};
use ferrofire::fd::proto::Module;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

fn set_limit(n: u64) {
    let lim = libc::rlimit {
        rlim_cur: n,
        rlim_max: n.max(4096),
    };
    assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &lim) }, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_listener_survives_running_out_of_descriptors() {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let log = std::env::temp_dir()
        .join(format!("dnp3-accept-{port}.log"))
        .display()
        .to_string();
    let xml = format!(
        r#"<SCEPTRE><field-device><name>t</name><tags><external-tag><name>bi</name><io>I0</io><type>binary</type></external-tag></tags>
<comms><dnp3-server><endpoint>tcp://127.0.0.1:{port}</endpoint><address>10</address><event-logging>{log}</event-logging>
<binary-input><address>0</address><tag>bi</tag></binary-input></dnp3-server></comms>
<input><endpoint>udp://239.0.0.1:40000</endpoint><binary><id>I0</id><name>p</name></binary></input></field-device></SCEPTRE>"#
    );
    let (out, store) = common::load(&xml);
    let stop = CancellationToken::new();
    tokio::spawn(out.run(store, stop.clone()));
    tokio::time::sleep(Duration::from_millis(300)).await;

    // a client whose connection the kernel completes while the process has no descriptor to accept it with
    let used = std::fs::read_dir("/proc/self/fd").unwrap().count() as u64;
    set_limit(used + 3);
    let hoard: Vec<_> = (0..2)
        .map_while(|_| std::fs::File::open("/dev/null").ok())
        .collect();
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap(); // the client's own descriptor: taken before the hoard filled
    tokio::time::sleep(Duration::from_millis(600)).await; // accept() fails with EMFILE a few times
    drop(hoard);
    set_limit(4096);

    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let req = link::encode(0xC4, 10, 1, &[0xC0, 0xC0, 1, 60, 1, 6]);
    std::io::Write::write_all(&mut s, &req).unwrap();
    let mut buf = [0u8; 512];
    let n = std::io::Read::read(&mut s, &mut buf).unwrap();
    assert!(
        n > 0 && matches!(link::parse(&buf[..n]), Parse::Frame(..)),
        "the outstation answered once descriptors came back"
    );
    stop.cancel();
    let text = std::fs::read_to_string(&log).unwrap();
    assert!(
        text.contains("accept failed"),
        "the failure was logged once: {text}"
    );
}
