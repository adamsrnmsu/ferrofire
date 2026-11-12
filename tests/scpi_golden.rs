//! Transcripts of an SCPI session, with no sockets: every `>` line is sent to the instrument and the `<` lines after it are exactly
//! what it must answer (none: nothing). `@ publish POINT VALUE` is the provider, `@ connect` a new connection (fresh registers and
//! error queue), `@ session N` talks as the Nth connection of the same instrument (session 0 is the first; the others share its
//! registers and error queue), `@ drain` is the provider acknowledging every queued write, `@ queued TAG` / `@ idle TAG` check the
//! Store write queue. `{version}` is this crate's version.
//! The expected texts follow SCPI-99 and IEEE 488.2 as written; docs/field-device-scpi.md lists the choices where those leave room.

mod common;

use ferrofire::fd::proto::scpi::device::Session;

const TRANSCRIPT: &str = include_str!("golden/scpi.transcript");

#[test]
fn the_transcript_is_what_the_instrument_answers() {
    let (server, store) = common::scpi::load("");
    let ins = server.instrument();
    let mut sessions = vec![Session::new()];
    let mut who = 0;
    let version = env!("CARGO_PKG_VERSION");
    let mut lines = TRANSCRIPT
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty() && !l.starts_with('#'))
        .peekable();
    while let Some((n, line)) = lines.next() {
        let at = format!("tests/golden/scpi.transcript:{}", n + 1);
        if let Some(cmd) = line.strip_prefix("@ ") {
            let w: Vec<_> = cmd.split(' ').collect();
            match w[..] {
                ["connect"] => (sessions, who) = (vec![Session::new()], 0),
                ["session", n] => {
                    who = n.parse().unwrap();
                    while sessions.len() <= who {
                        sessions.push(Session::sharing(sessions[0].status().clone()));
                    }
                }
                ["drain"] => store
                    .pending(0)
                    .into_iter()
                    .for_each(|(tag, _, v)| store.ack(&tag, v)),
                ["publish", point, v] => {
                    assert!(store.publish(point, v.parse().unwrap(), 2), "{at}")
                }
                ["queued", tag] => assert!(store.is_queued(tag), "{at}: {tag} is not queued"),
                ["idle", tag] => assert!(!store.is_queued(tag), "{at}: {tag} is queued"),
                _ => panic!("{at}: unknown directive"),
            }
            continue;
        }
        let sent = line
            .strip_prefix("> ")
            .unwrap_or_else(|| panic!("{at}: expected `> `"));
        let got = sessions[who].feed(ins, &store, format!("{sent}\n").as_bytes());
        let mut want = String::new();
        while let Some((_, l)) = lines.next_if(|(_, l)| l.starts_with('<')) {
            want.push_str(&l[1..].trim_start_matches(' ').replace("{version}", version));
            want.push('\n');
        }
        assert_eq!(
            String::from_utf8(got).unwrap(),
            want,
            "{at}: after `{sent}`"
        );
    }
}
