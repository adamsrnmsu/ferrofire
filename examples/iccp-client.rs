//! The thin ICCP test client as a command, for the acceptance harness (conformance/iccp/peer.sh): no open ICCP peer exists.
//!   [ICCP_CALLING_AP_TITLE=1.3.9999.1] iccp-client HOST:PORT DOMAIN READ_ITEM,... WRITE_ITEM [DATA_SET_MEMBER,...]
//! Associates, browses, reads, writes WRITE_ITEM (Data_Real 1.0, Data_State on, Data_Discrete as read), defines a data set of
//! the members (default: the read items), arms a transfer set (interval 1 s + object change) and prints the first reports.
//! Every result is one `key: value` line; exit 1 if anything failed.
#[path = "../tests/common/iccp_client.rs"]
mod iccp_client;

use iccp_client::Iccp;
use iec61850_rs::proto::data::Value;
use std::time::Duration;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 5 {
        eprintln!(
            "usage: iccp-client HOST:PORT DOMAIN READ_ITEM,... WRITE_ITEM [DATA_SET_MEMBER,...]"
        );
        std::process::exit(2);
    }
    // ICCP_STEPS=1: against another vendor's server, run each check on its own and report `step NAME: ok|FAIL detail` instead of
    // stopping at the first failure; conformance/iccp/check.sh decides which of them that peer is expected to pass.
    if std::env::var("ICCP_STEPS").is_ok() {
        if let Err(e) = steps(&a) {
            println!("step associate: FAIL {e}");
        }
        return;
    }
    if let Err(e) = run(&a) {
        println!("FAILED: {e}");
        std::process::exit(1);
    }
}

/// The value just read, with every real set to 1.0 and every state set to on: the same shape, so it is a valid write whatever the
/// object's type (the server ignores the quality and time a client sends).
fn commanded(v: &Value) -> Value {
    match v {
        Value::Float32(_) => Value::Float32(1.0),
        Value::BitString { unused, bytes } if bytes.len() == 1 => Value::BitString {
            unused: *unused,
            bytes: vec![(bytes[0] & 0x3F) | 0x80],
        },
        Value::Structure(m) => Value::Structure(m.iter().map(commanded).collect()),
        other => other.clone(),
    }
}

fn run(a: &[String]) -> Result<(), String> {
    let (domain, items): (&str, Vec<&str>) = (&a[2], a[3].split(',').collect());
    let members: Vec<&str> = a.get(5).map_or(items.clone(), |m| m.split(',').collect());
    // ICCP_CALLING_AP_TITLE=1.3.9999.1: the AP-title a bilateral table keyed by it (igstase2-server) expects
    let ap_title = match std::env::var("ICCP_CALLING_AP_TITLE") {
        Ok(t) => Some(
            t.split('.')
                .map(|n| {
                    n.parse::<u32>()
                        .map_err(|e| format!("ICCP_CALLING_AP_TITLE: {e}"))
                })
                .collect::<Result<Vec<_>, _>>()?,
        ),
        Err(_) => None,
    };
    let mut c = Iccp::connect_as(&a[1], ap_title)?;
    println!("identify: {}", c.identify()?);
    println!("domains: {:?}", c.domains()?);
    println!("variables: {:?}", c.variables(domain)?);
    println!(
        "bilateral-table: {:?}",
        c.read(None, &["Bilateral_Table_ID"])?
    );
    println!("features: {:?}", c.read(None, &["Supported_Features"])?);
    for (i, v) in items.iter().zip(c.read(Some(domain), &items)?) {
        println!("read {i}: {v:?}");
    }
    let w = a[4].as_str();
    let now = c
        .read(Some(domain), &[w])?
        .remove(0)
        .map_err(|e| format!("read {w}: error {e}"))?;
    let v = commanded(&now);
    println!("write {w}: {:?}", c.write(domain, w, &v)?);
    c.define_set(domain, "DS_PROBE", &members)?;
    println!(
        "data-set DS_PROBE: {:?}",
        c.set_members(domain, "DS_PROBE")?
    );
    let ts = c.next_transfer_set(domain)?;
    println!(
        "arm {ts}: {:?}",
        c.arm(domain, &ts, "DS_PROBE", (1, 0xA0, false, true))?
    );
    for n in 1..=3 {
        match c.next_report(Duration::from_secs(4))? {
            Some(r) => println!(
                "report {n}: transfer-set {} conditions {:#04x} values {}",
                r.transfer_set,
                r.conditions,
                r.values.len()
            ),
            None => return Err("no report within 4 s".into()),
        }
    }
    c.arm(domain, &ts, "DS_PROBE", (1, 0xA0, false, false))?
        .map_err(|e| format!("disarm {e}"))?;
    c.release()?;
    println!("released");
    Ok(())
}

/// One check: `step NAME: ok DETAIL` or `step NAME: FAIL DETAIL`.
fn step<T: std::fmt::Debug>(name: &str, r: Result<T, String>) -> Option<T> {
    match r {
        Ok(v) => {
            println!("step {name}: ok {v:?}");
            Some(v)
        }
        Err(e) => {
            println!("step {name}: FAIL {e}");
            None
        }
    }
}

/// A DataAccessError inside a successful service answer is a failed check.
fn access<T>(r: Result<Result<T, i64>, String>) -> Result<T, String> {
    r?.map_err(|code| format!("data access error {code}"))
}

fn all_ok<T>(r: Result<Vec<Result<T, i64>>, String>) -> Result<Vec<T>, String> {
    r?.into_iter()
        .map(|x| x.map_err(|code| format!("data access error {code}")))
        .collect()
}

fn steps(a: &[String]) -> Result<(), String> {
    let (domain, items): (&str, Vec<&str>) = (&a[2], a[3].split(',').collect());
    let members: Vec<&str> = a.get(5).map_or(items.clone(), |m| m.split(',').collect());
    let ap_title = std::env::var("ICCP_CALLING_AP_TITLE")
        .ok()
        .map(|t| t.split('.').filter_map(|n| n.parse::<u32>().ok()).collect());
    let mut c = Iccp::connect_as(&a[1], ap_title)?;
    println!("step associate: ok");
    step("identify", c.identify());
    step("domains", c.domains());
    step("variables", c.variables(domain));
    // Bilateral_Table_ID is domain-scoped in the field (both real captures, igs); TASE2_Version and Supported_Features are VMD-scoped
    for (scope, n) in [
        (Some(domain), "Bilateral_Table_ID"),
        (None, "TASE2_Version"),
        (None, "Supported_Features"),
    ] {
        step(
            &format!("read {n}"),
            access(c.read(scope, &[n]).map(|mut v| v.remove(0))),
        );
    }
    step("read data values", all_ok(c.read(Some(domain), &items)));
    let w = a[4].as_str();
    let write = access(c.read(Some(domain), &[w]).map(|mut v| v.remove(0)))
        .map(|now| commanded(&now))
        .and_then(|v| access(c.write(domain, w, &v)));
    step(&format!("write {w}"), write);
    step(
        "define data set",
        c.define_set(domain, "DS_PROBE", &members),
    );
    step("data set members", c.set_members(domain, "DS_PROBE"));
    step("read data set", all_ok(c.read_set(domain, "DS_PROBE")));
    let ts = step("next transfer set", c.next_transfer_set(domain));
    if let Some(ts) = ts {
        // interval 1 s plus object change; then the same set with report-by-exception
        for (label, rbe) in [("interval", false), ("rbe", true)] {
            while matches!(c.next_report(Duration::from_millis(400)), Ok(Some(_))) {} // reports of the previous arming
            let armed = access(c.arm(domain, &ts, "DS_PROBE", (1, 0xA0, rbe, true)));
            if step(&format!("arm {label}"), armed).is_some() {
                let got = c.next_report(Duration::from_secs(4)).and_then(|r| {
                    r.map(|r| {
                        format!(
                            "{} conditions {:#04x} values {}",
                            r.transfer_set,
                            r.conditions,
                            r.values.len()
                        )
                    })
                    .ok_or_else(|| "no report within 4 s".to_string())
                });
                step(&format!("report {label}"), got);
                step(
                    &format!("disarm {label}"),
                    access(c.arm(domain, &ts, "DS_PROBE", (1, 0xA0, rbe, false))),
                );
            }
        }
    }
    step("delete data set", c.delete_set(domain, "DS_PROBE"));
    step("conclude", c.release());
    Ok(())
}
