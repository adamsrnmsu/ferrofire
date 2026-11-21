//! The command interface over a real ZMTP socket: a REQ client (what bennu-probe is) gets one NUL-terminated reply per request,
//! a request is read up to its NUL, and the socket stops with the device.

use ferrofire::fd::cmdif::{serve, Points};
use ferrofire::model::Value;
use std::time::Duration;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use zeromq::{Socket, SocketRecv, SocketSend};

struct One;

impl Points for One {
    fn names(&self) -> Vec<String> {
        vec!["a".into()]
    }
    async fn read(&self, _tag: &str) -> Result<Value, String> {
        Ok(Value::Analog(1.5))
    }
    async fn write(&self, _tag: &str, _v: Value) -> Result<(), String> {
        Ok(())
    }
}

fn free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}

async fn ask(req: &mut zeromq::ReqSocket, text: &[u8]) -> Vec<u8> {
    req.send(text.to_vec().into()).await.unwrap();
    let m = timeout(Duration::from_secs(5), req.recv())
        .await
        .unwrap()
        .unwrap();
    m.get(0).unwrap().to_vec()
}

#[tokio::test]
async fn a_req_client_gets_one_nul_terminated_reply_per_request_until_stop() {
    let ep = format!("tcp://127.0.0.1:{}", free_port());
    let stop = CancellationToken::new();
    let task = tokio::spawn({
        let (ep, stop) = (ep.clone(), stop.clone());
        async move { serve(&ep, One, stop).await }
    });
    let mut req = zeromq::ReqSocket::new();
    for _ in 0..50 {
        if req.connect(&ep).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(ask(&mut req, b"QUERY=\0").await, b"ACK=a,\0");
    assert_eq!(ask(&mut req, b"READ=a\0").await, b"ACK=a:1.500000\0");
    // the request is a C string: what follows the NUL is not part of it, and a missing NUL is tolerated
    assert_eq!(ask(&mut req, b"READ=a\0=junk").await, b"ACK=a:1.500000\0");
    assert_eq!(ask(&mut req, b"WRITE=a:2").await, b"ACK=Wrote tag a -- 2\0");
    stop.cancel();
    timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn a_bind_failure_returns() {
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let ep = format!("tcp://{}", taken.local_addr().unwrap());
    timeout(
        Duration::from_secs(5),
        serve(&ep, One, CancellationToken::new()),
    )
    .await
    .expect("serve returns when it cannot bind");
}
