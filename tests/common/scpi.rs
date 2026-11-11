//! Shared by the SCPI tests: a device config with a read-only analog and binary, two writable ones and a tag whose name is not an SCPI
//! mnemonic, and the extra `<scpi-server>` keys a test wants.
#![allow(dead_code)]

use super::load_as;
use ferrofire::fd::proto::scpi::Server;
use ferrofire::fd::store::Store;

pub fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// `inner`: keys of `<scpi-server>` other than the points, which are always the same five.
pub fn config(inner: &str) -> String {
    format!(
        r#"<SCEPTRE><field-device><name>rtu-1</name><cycle-time>500</cycle-time>
<tags>
 <external-tag><name>level</name><io>I1</io><type>analog</type></external-tag>
 <external-tag><name>alarm</name><io>I2</io><type>binary</type></external-tag>
 <external-tag><name>load-1_bus-1.mw</name><io>I3</io><type>analog</type></external-tag>
 <external-tag><name>setpoint</name><io>O1</io><type>analog</type></external-tag>
 <external-tag><name>breaker</name><io>O2</io><type>binary</type></external-tag>
</tags>
<comms><scpi-server>{inner}
 <point><tag>level</tag></point>
 <point><tag>alarm</tag></point>
 <point><tag>load-1_bus-1.mw</tag></point>
 <point><tag>setpoint</tag><writable>true</writable><min-value>0</min-value><max-value>1000</max-value><default-value>500</default-value></point>
 <point><tag>breaker</tag><writable>true</writable></point>
</scpi-server></comms>
<input><endpoint>udp://239.0.0.1:40000</endpoint><analog><id>I1</id><name>plevel</name></analog><binary><id>I2</id><name>palarm</name></binary><analog><id>I3</id><name>pload</name></analog></input>
<output><endpoint>tcp://127.0.0.1:5555</endpoint><analog><id>O1</id><name>psetpoint</name></analog><binary><id>O2</id><name>pbreaker</name></binary></output>
</field-device></SCEPTRE>"#
    )
}

/// The server parsed from `config(inner)` and its store, with the provider's values published.
pub fn load(inner: &str) -> (Server, Store) {
    let (server, store) = load_as::<Server>(&config(inner), "scpi-server");
    store.publish("plevel", 12.5, 1);
    store.publish("palarm", 1.0, 1);
    store.publish("pload", -250.0, 1);
    store.publish("psetpoint", 400.0, 1);
    (server, store)
}
