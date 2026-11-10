//! ICCP/TASE.2 server (docs/field-device-protocols-plan.md section 2 template). The first byte picks the layer: odd, arbitrary bytes
//! into the OSI stack and the server through `fd::mms::step` (cut into chunks at 0xFF so PDUs split across reads); even, the rest is
//! one MMS PDU handed straight to the handler (no OSI framing in the way), twice, with a refresh after each. Invariant: no panic, whatever arrives; a violation closes the association, never the server.
#![no_main]
use ferrofire::fd::config::{Opts, Tags};
use ferrofire::fd::mms::{step, Handler};
use ferrofire::fd::proto::iccp::{Device, Server};
use ferrofire::fd::proto::Module;
use ferrofire::fd::store::{Layout, Store};
use ferrofire::model::Kind;
use iec61850_rs::common::{monotonic_now, Instant};
use iec61850_rs::proto::mms::association::{Association, AssociationConfig};
use libfuzzer_sys::fuzz_target;

fn device() -> Device {
    let xml = "<iccp-server><endpoint>tcp://127.0.0.1:1</endpoint><event-logging>/dev/null</event-logging>\
        <data-state><tag>bi</tag><name>BI</name></data-state><data-real><tag>an</tag><name>AN</name><variant>qtimetag</variant></data-real>\
        <data-discrete><tag>ao</tag><name>AO</name></data-discrete>\
        <peer><name>EMS</name><write>BI AO</write></peer></iccp-server>";
    let doc = roxmltree::Document::parse(xml).unwrap();
    let (mut tags, mut layout) = (Tags::default(), Layout::default());
    for (n, k) in [("bi", Kind::Binary), ("an", Kind::Analog), ("ao", Kind::Analog)] {
        tags.add_internal(n, k).unwrap();
        layout.points.push((n.into(), k, None));
        layout.external.push((n.into(), n.into()));
    }
    let server = Server::parse(doc.root_element(), &mut tags, &Opts::default()).unwrap();
    let mut d = server.device(&Store::new(layout));
    d.opened(1, vec![10, 0, 0, 1]);
    d
}

fuzz_target!(|data: &[u8]| {
    let Some((mode, rest)) = data.split_first() else { return };
    let mut dev = device();
    let now = monotonic_now();
    if mode & 1 == 1 {
        let mut assoc = Association::server(AssociationConfig::default());
        for chunk in rest.split(|&b| b == 0xFF) {
            assoc.on_bytes(now, chunk);
            let (_, closed) = step(&mut dev, 1, &mut assoc, now, &mut false);
            while assoc.poll_transmit().is_some() {}
            if closed {
                break;
            }
        }
    } else {
        for _ in 0..2 {
            let _ = dev.request(1, Instant(0), 1, rest);
            let _ = dev.refresh(10_000);
        }
    }
});
