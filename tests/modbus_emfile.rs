//! A server that runs out of descriptors keeps listening (bennu #79: one failed accept ended the accept loop for good). This test
//! lowers the process's descriptor limit, so it is a binary of its own: no other test shares the process.

mod common;

use common::modbus::*;
use socket2::{Domain, Socket, Type};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

/// Lower the soft limit (the hard one stays, so it can be raised again).
fn set_nofile(limit: u64) {
    let mut r = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    assert_eq!(unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut r) }, 0);
    r.rlim_cur = limit as libc::rlim_t;
    assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &r) }, 0);
}

/// Run out of descriptors with a client waiting to be accepted, hold that for a while, then free them: the client is served.
async fn episode(port: u16, tid: u16) {
    // let the server finish closing the connection before: its descriptor must not be the one that frees up later
    tokio::time::sleep(Duration::from_millis(300)).await;
    let open = open_fds().expect("/proc") as u64;
    // the client's socket exists before descriptors run out, so that connecting needs none
    let sock = Socket::new(Domain::IPV4, Type::STREAM, None).unwrap();
    set_nofile(open + 6);
    let mut hog = vec![];
    while let Ok(f) = std::fs::File::open("/dev/null") {
        hog.push(f);
    }
    // the kernel queues the connection; the server cannot accept it
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    sock.connect(&addr.into()).unwrap();
    let std_client = std::net::TcpStream::from(sock);
    std_client.set_nonblocking(true).unwrap();
    let mut client = TcpStream::from_std(std_client).unwrap();
    client.write_all(&good(tid, &READ_IR)).await.unwrap();
    // it retries, pausing longer each time, and the connection waits
    let (got, closed) = drain(&mut client, Duration::from_millis(1500)).await;
    assert_eq!(
        (got, closed),
        (vec![], false),
        "an unaccepted connection waits"
    );
    drop(hog);
    set_nofile(open + 100);
    let mut reply = [0u8; 11];
    timeout(Duration::from_secs(3), client.read_exact(&mut reply))
        .await
        .expect("served once descriptors are free")
        .unwrap();
    assert_eq!(reply, [0, tid as u8, 0, 0, 0, 5, 1, 4, 2, 0, 0]);
}

#[tokio::test]
async fn accept_runs_out_of_descriptors_says_so_once_per_episode_and_serves_again_when_some_are_free(
) {
    let d = start();
    assert_eq!(ask(d.port, &good(1, &READ_IR)).await.len(), 11);
    if open_fds().is_none() {
        return; // /proc is Linux's
    }
    episode(d.port, 7).await;
    assert_eq!(
        ask(d.port, &good(2, &READ_IR)).await.len(),
        11,
        "served in between"
    );
    episode(d.port, 8).await;
    let text = std::fs::read_to_string(&d.log).unwrap();
    let said = text.matches("accept failed").count();
    assert_eq!(
        said, 2,
        "one line per run of failures, not one per retry:\n{text}"
    );
    d.stop.cancel();
}
