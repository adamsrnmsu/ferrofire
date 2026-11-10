//! IEC 61850 MMS server (docs/field-device-protocols-plan.md section 2 template): arbitrary bytes into the OSI stack
//! (TPKT, COTP, session, presentation, ACSE, MMS) and the ACSI server through the same `step` the socket loop uses.
//! Invariant: no panic, whatever arrives; a violation closes the association, never the server. The input is cut into chunks at
//! 0xFF bytes so that PDUs split across reads too.
#![no_main]
use ferrofire::fd::config::{Opts, Tags};
use ferrofire::fd::mms::step;
use ferrofire::fd::proto::iec61850::{Device, Server};
use ferrofire::fd::proto::Module;
use ferrofire::fd::store::{Layout, Store};
use ferrofire::model::Kind;
use iec61850_rs::common::monotonic_now;
use iec61850_rs::proto::mms::association::{Association, AssociationConfig};
use libfuzzer_sys::fuzz_target;

fn device() -> Device {
    let xml = "<iec61850-server><endpoint>tcp://127.0.0.1:1</endpoint><event-logging>/dev/null</event-logging>\
        <binary-input><tag>bi</tag></binary-input><analog-input><tag>an</tag></analog-input>\
        <binary-output><tag>bo</tag></binary-output><analog-output><tag>ao</tag></analog-output></iec61850-server>";
    let doc = roxmltree::Document::parse(xml).unwrap();
    let (mut tags, mut layout) = (Tags::default(), Layout::default());
    for (n, k) in [("bi", Kind::Binary), ("an", Kind::Analog), ("bo", Kind::Binary), ("ao", Kind::Analog)] {
        tags.add_internal(n, k).unwrap();
        layout.points.push((n.into(), k, None));
        layout.external.push((n.into(), n.into()));
    }
    let server = Server::parse(doc.root_element(), &mut tags, &Opts::default()).unwrap();
    server.device(&Store::new(layout), None)
}

fuzz_target!(|data: &[u8]| {
    let mut dev = device();
    let mut assoc = Association::server(AssociationConfig::default());
    let now = monotonic_now();
    for chunk in data.split(|&b| b == 0xFF) {
        assoc.on_bytes(now, chunk);
        let (_, closed) = step(&mut dev, 1, &mut assoc, now, &mut false);
        while assoc.poll_transmit().is_some() {}
        if closed {
            break;
        }
    }
});
