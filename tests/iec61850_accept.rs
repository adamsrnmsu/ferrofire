//! The MMS accept loop survives `accept()` failing (out of descriptors, as bennu #81 for DNP3). This test lowers the descriptor
//! limit, so it lives alone in its own test binary (the limit is process-wide).
#![cfg(feature = "proto-iec61850")]

mod common;

use ferrofire::fd::proto::iec61850::Server;
use ferrofire::fd::proto::Module;
use iec61850_rs::client::Client;
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
        .join(format!("iec61850-accept-{port}.log"))
        .display()
        .to_string();
    let xml = format!(
        r#"<SCEPTRE><field-device><name>t</name><tags><external-tag><name>bi</name><io>I0</io><type>binary</type></external-tag></tags>
<comms><iec61850-server><endpoint>tcp://127.0.0.1:{port}</endpoint><event-logging>{log}</event-logging>
<binary-input><tag>bi</tag></binary-input></iec61850-server></comms>
<input><endpoint>udp://239.0.0.1:40000</endpoint><binary><id>I0</id><name>p</name></binary></input></field-device></SCEPTRE>"#
    );
    let (srv, store) = common::load_as::<Server>(&xml, "iec61850-server");
    let stop = CancellationToken::new();
    tokio::spawn(srv.run(store, stop.clone()));
    tokio::time::sleep(Duration::from_millis(300)).await;

    let used = std::fs::read_dir("/proc/self/fd").unwrap().count() as u64;
    set_limit(used + 3);
    let hoard: Vec<_> = (0..2)
        .map_while(|_| std::fs::File::open("/dev/null").ok())
        .collect();
    let s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap(); // completed by the kernel, never accepted
    tokio::time::sleep(Duration::from_millis(600)).await; // accept() fails with EMFILE a few times
    drop(hoard);
    drop(s);
    set_limit(4096);

    let dir = tokio::task::spawn_blocking(move || {
        let mut c = Client::connect(&format!("127.0.0.1:{port}")).unwrap();
        c.server_directory().unwrap()
    })
    .await
    .unwrap();
    assert_eq!(
        dir,
        ["simpleIOGenericIO"],
        "the server answered once descriptors came back"
    );
    stop.cancel();
    let text = std::fs::read_to_string(&log).unwrap();
    assert!(
        text.contains("accept failed"),
        "the failure was logged: {text}"
    );
    assert_eq!(
        text.matches("accept failed").count(),
        1,
        "once per outage: {text}"
    );
}
