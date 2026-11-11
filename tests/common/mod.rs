//! Shared by the protocol integration tests: load a module's element of a sceptre config.xml the way the field device does.
#![allow(dead_code)]

pub mod modbus;
pub mod scpi;

use ferrofire::fd::config::{Opts, Tags};
use ferrofire::fd::proto::dnp3::Outstation;
use ferrofire::fd::proto::Module;
use ferrofire::fd::store::{Layout, Store};
use ferrofire::model::Kind;
use std::collections::HashMap;

pub fn kind(t: &str) -> Kind {
    if t == "binary" {
        Kind::Binary
    } else {
        Kind::Analog
    }
}

/// Points and tags typed first (as `Config::parse` does), then the module parses its own element.
pub fn load(xml: &str) -> (Outstation, Store) {
    load_as::<Outstation>(xml, "dnp3-server")
}

/// The same for any module: its `<comms>` child is the element called `tag`.
pub fn load_as<M: Module>(xml: &str, tag: &str) -> (M, Store) {
    let doc = roxmltree::Document::parse(xml).unwrap();
    let fd = doc
        .descendants()
        .find(|n| n.has_tag_name("field-device"))
        .unwrap();
    let text = |n: roxmltree::Node, k: &str| {
        n.children()
            .find(|c| c.has_tag_name(k))
            .and_then(|c| c.text())
            .unwrap()
            .trim()
            .to_string()
    };
    let (mut layout, mut tags, mut ids) = (Layout::default(), Tags::default(), HashMap::new());
    for (section, owned) in [("input", false), ("output", true)] {
        for (i, sec) in fd
            .children()
            .filter(|c| c.has_tag_name(section))
            .enumerate()
        {
            for p in sec
                .children()
                .filter(|c| c.is_element() && matches!(c.tag_name().name(), "binary" | "analog"))
            {
                let (name, k) = (text(p, "name"), kind(p.tag_name().name()));
                layout.points.push((name.clone(), k, owned.then_some(i)));
                ids.insert(text(p, "id"), name);
            }
        }
    }
    for t in fd.descendants().filter(|c| c.has_tag_name("external-tag")) {
        let (name, io, k) = (text(t, "name"), text(t, "io"), kind(&text(t, "type")));
        layout.external.push((name.clone(), ids[&io].clone()));
        tags.add_internal(&name, k).unwrap();
    }
    let node = fd.descendants().find(|c| c.has_tag_name(tag)).unwrap();
    let out = M::parse(node, &mut tags, &Opts::default()).unwrap();
    (out, Store::new(layout))
}

pub fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}
