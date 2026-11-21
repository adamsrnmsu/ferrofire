//! The bennu command interface (`fd::cmdif`): arbitrary bytes as a request, read as the socket reads it (up to the first NUL,
//! lossily as text). Invariants: no panic, whatever arrives; every request gets exactly one reply, it starts with `ACK=` or
//! `ERR=` and ends in one NUL.
#![no_main]
use ferrofire::fd::cmdif::{reply, Points};
use ferrofire::model::Value;
use libfuzzer_sys::fuzz_target;
use std::future::Future;
use std::task::{Context, Poll, Waker};

struct Tags;

impl Points for Tags {
    fn names(&self) -> Vec<String> {
        vec!["a".into(), "b".into(), "é".into()]
    }
    async fn read(&self, tag: &str) -> Result<Value, String> {
        match tag {
            "a" => Ok(Value::Analog(1.5)),
            "b" => Ok(Value::Binary(true)),
            _ => Err("no".into()),
        }
    }
    async fn write(&self, tag: &str, _v: Value) -> Result<(), String> {
        match tag {
            "b" => Err("no".into()),
            _ => Ok(()),
        }
    }
}

fuzz_target!(|data: &[u8]| {
    let line = String::from_utf8_lossy(data.split(|b| *b == 0).next().unwrap_or(&[])).into_owned();
    // the fake's futures never wait, so one poll finishes the reply
    let mut fut = std::pin::pin!(reply(&line, &Tags));
    let Poll::Ready(out) = fut.as_mut().poll(&mut Context::from_waker(Waker::noop())) else {
        panic!("the reply waited");
    };
    assert!(out.starts_with(b"ACK=") || out.starts_with(b"ERR="));
    assert_eq!(out.iter().filter(|b| **b == 0).count(), 1);
    assert_eq!(out.last(), Some(&0));
});
