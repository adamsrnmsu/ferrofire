//! Shared by the SCPI tests: a device config with a read-only analog and binary, two writable ones and a tag whose name is not an SCPI
//! mnemonic, and the extra `<scpi-server>` keys a test wants.
#![allow(dead_code)]

use super::load_as;
use ferrofire::fd::proto::scpi::Server;
use ferrofire::fd::store::Store;

/// A port nothing listens on, that no other test of this process was given, and that the kernel will not hand to a client as its source
/// port before the server has bound it (what asking the kernel for port 0 and closing it again risked: the next `connect` of another
/// test could take the same port, and the server's bind failed). The ports are below the kernel's ephemeral range (32768 up); the
/// process id picks a block of 100, so that test binaries running at the same time (mutation workers) do not meet.
pub fn free_port() -> u16 {
    static NEXT: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(0);
    let base = 12_000 + (std::process::id() % 200) as u16 * 100;
    loop {
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let port = base + n % 100;
        if std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
            && std::net::UdpSocket::bind(("127.0.0.1", port)).is_ok()
        {
            return port;
        }
    }
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
