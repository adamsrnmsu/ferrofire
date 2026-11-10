//! `ferrofire probe`: reply parsing, write requests, the audit line, the watcher on hand-made datagrams, and the real
//! binary against a running `ferrofire run` (query, read, write, watch).
use ferrofire::bennu::{datagrams_msg, group_for};
use ferrofire::probe::*;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_ferrofire");
const MODEL: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/models/waterway.yaml");

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

#[test]
fn replies_and_requests() {
    assert_eq!(tag_list(b"ACK=a,b.c,\0"), Ok(s(&["a", "b.c"])));
    assert!(tag_list(b"ERR=x\0").unwrap_err().contains("ERR=x"));
    let rows = read_rows(b"ACK=a:1.5,b:true,\0").unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!((rows[0].tag.as_str(), rows[0].value.as_str()), ("a", "1.5"));
    assert!(read_rows(b"ACK=a,\0").unwrap_err().contains("malformed"));
    assert!(read_rows(b"ERR=Tag 'x' not found (did you mean `y`?)\0")
        .unwrap_err()
        .contains("did you mean `y`"));
    assert_eq!(
        write_request(&s(&["a=1", "b=true"])),
        Ok(b"WRITE=a:1,b:true,".to_vec())
    );
    for bad in ["a", "=1", "a=", ""] {
        assert!(
            write_request(&s(&[bad]))
                .unwrap_err()
                .contains("expected tag=value"),
            "{bad}"
        );
    }
    assert_eq!(
        audit_line(42, "me@pid7", "tcp://h:1", &s(&["a=1", "b=2"]), "ACK"),
        "audit: write t=42 by=me@pid7 server=tcp://h:1 points=a=1,b=2 result=ACK"
    );
}

#[test]
fn rows_as_text_and_json() {
    let r = |value: &str, live| Row {
        tag: "a\"b".into(),
        value: value.into(),
        live,
    };
    assert_eq!(r("1.5", None).text(), "a\"b=1.5");
    assert_eq!(
        r("1.5", Some((3, true, 2))).text(),
        "a\"b=1.5 age=3 STALE gaps=2"
    );
    assert_eq!(r("1", Some((0, false, 0))).text(), "a\"b=1 age=0 ok gaps=0");
    assert_eq!(r("1.5", None).json(), r#"{"tag":"a\"b","value":1.5}"#);
    assert_eq!(r("true", None).json(), r#"{"tag":"a\"b","value":true}"#);
    assert_eq!(r("x\n", None).json(), r#"{"tag":"a\"b","value":"x\u000a"}"#);
    assert_eq!(r("NaN", None).json(), r#"{"tag":"a\"b","value":"NaN"}"#);
    assert_eq!(
        r("2", Some((1, false, 4))).json(),
        r#"{"tag":"a\"b","value":2,"age":1,"stale":false,"seq_gaps":4}"#
    );
}

#[test]
fn watcher_shows_changes_age_stale_and_gaps() {
    let g = group_for("udp://*;239.0.0.1:40000");
    let dg = |msg: &str, seq: u64, cyc: u64| datagrams_msg(&g, msg, seq, cyc, 1).remove(0);
    let mut w = Watcher::new(&g, s(&["a", "b"]), 3);
    let rows = w.feed(&dg("a:1,b:2,", 1, 1));
    assert_eq!(rows.len(), 2);
    assert_eq!(
        (rows[0].tag.as_str(), rows[0].value.as_str(), rows[0].live),
        ("a", "1", Some((0, false, 0)))
    );
    assert!(w.feed(&dg("a:1,b:2,", 2, 2)).is_empty()); // nothing changed
    assert!(w
        .feed(
            &dg("garbage", 3, 3)
                .iter()
                .rev()
                .copied()
                .collect::<Vec<_>>()
        )
        .is_empty()); // rejected: silent
    let rows = w.feed(&dg("a:5,", 4, 3)); // seq 3 never arrived: one gap; a changed
    assert_eq!(
        (rows.len(), rows[0].value.as_str(), rows[0].live),
        (1, "5", Some((0, false, 1)))
    );
    assert!(w.feed(&dg("a:5,", 5, 4)).is_empty()); // b was last refreshed in cycle 2: age 2, still fresh
    let rows = w.feed(&dg("a:5,", 6, 5)); // age 3: stale, shown once
    assert_eq!(
        (rows.len(), rows[0].tag.as_str(), rows[0].live),
        (1, "b", Some((3, true, 1)))
    );
    assert!(w.feed(&dg("a:5,", 7, 6)).is_empty());
    let rows = w.feed(&dg("a:5,b:9,", 8, 7)); // b is back
    assert_eq!(
        (rows[0].tag.as_str(), rows[0].value.as_str(), rows[0].live),
        ("b", "9", Some((0, false, 1)))
    );
    let late = w.feed(&dg("a:6,", 8, 7)); // a duplicate seq: dropped
    assert!(late.is_empty());
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn server(publish: &str) -> (Server, String) {
    let ep = format!("tcp://127.0.0.1:{}", free_port());
    let child = Command::new(BIN)
        .args([
            "run",
            MODEL,
            "--server",
            &ep,
            "--publish",
            publish,
            "--rate",
            "0.05",
        ])
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    (Server(child), ep)
}

fn probe(server: &str, args: &[&str], stdin: &str) -> (bool, String, String) {
    let mut c = Command::new(BIN)
        .args(["probe", "--server", server, "--timeout-ms", "5000"])
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    c.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    let o = c.wait_with_output().unwrap();
    (
        o.status.success(),
        String::from_utf8_lossy(&o.stdout).into(),
        String::from_utf8_lossy(&o.stderr).into(),
    )
}

#[test]
fn query_read_write_against_a_running_provider() {
    let (_srv, ep) = server("udp://*;127.0.0.1:9");
    let (ok, out, _) = probe(&ep, &["query"], "");
    assert!(
        ok && out.lines().count() == 15 && out.contains("lowervalve.open\n"),
        "{out}"
    );
    let (_, out, _) = probe(&ep, &["--json", "query"], "");
    assert!(out.starts_with("{\"tag\":\""), "{out}");
    let (ok, out, _) = probe(&ep, &["read", "lowerwater.height,lowervalve.open"], "");
    assert!(
        ok && out.starts_with("lowerwater.height=") && out.contains("lowervalve.open=0"),
        "{out}"
    );
    let (_, out, _) = probe(&ep, &["--json", "read", "lowervalve.open"], "");
    assert_eq!(out, "{\"tag\":\"lowervalve.open\",\"value\":0}\n");
    let (ok, _, err) = probe(&ep, &["read", "lowerwater.hieght"], "");
    assert!(
        !ok && err.contains("did you mean `lowerwater.height`"),
        "{err}"
    );

    // a write asks first; "n" (or no answer) writes nothing
    let (ok, _, err) = probe(&ep, &["write", "lowervalve.open=1"], "n\n");
    assert!(
        !ok && err.contains("not confirmed") && !err.contains("audit:"),
        "{err}"
    );
    let (_, out, _) = probe(&ep, &["read", "lowervalve.open"], "");
    assert_eq!(out, "lowervalve.open=0\n");
    let (ok, _, err) = probe(&ep, &["write", "lowervalve.open=1"], "y\n");
    assert!(
        ok && err.contains("audit: write t=")
            && err.contains("points=lowervalve.open=1 result=ACK"),
        "{err}"
    );
    let (ok, _, err) = probe(&ep, &["write", "--confirm", "lowervalve.opn=1"], "");
    assert!(
        !ok && err.contains("result=")
            && err.contains("ERR=")
            && err.contains("did you mean `lowervalve.open`"),
        "{err}"
    );
    let (ok, _, err) = probe(&ep, &["write", "--confirm", "nonsense"], "");
    assert!(
        !ok && err.contains("expected tag=value") && !err.contains("audit:"),
        "{err}"
    );
}

#[test]
fn interface_for_a_group() {
    assert_eq!(nic(None), Ok(std::net::Ipv4Addr::UNSPECIFIED));
    assert_eq!(
        nic(Some("10.1.2.3".into())),
        Ok("10.1.2.3".parse().unwrap())
    );
    assert!(nic(Some("eth0".into())).unwrap_err().contains("eth0"));
}

#[test]
fn watch_prints_value_age_and_stale() {
    let port = free_port();
    let listen = format!("udp://239.0.0.7:{port}");
    let (_srv, ep) = server(&listen);
    let mut w = Command::new(BIN)
        .args([
            "probe",
            "--server",
            &ep,
            "--json",
            "watch",
            "--listen",
            &listen,
            "--tags",
            "lowerwater.height",
        ])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(w.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    unsafe { libc::kill(w.id() as i32, libc::SIGTERM) }; // a clean stop, so coverage is written
    let st = w.wait().unwrap();
    assert!(st.success(), "{st:?}");
    assert!(
        line.starts_with("{\"tag\":\"lowerwater.height\",\"value\":")
            && line.contains("\"age\":0,\"stale\":false,\"seq_gaps\":"),
        "{line}"
    );
}

#[test]
fn globs_match_and_expand() {
    assert!(glob_match("bus-*.voltage", "bus-12.voltage"));
    assert!(glob_match("a?c", "abc") && !glob_match("a?c", "ac"));
    assert!(glob_match("*", "") && !glob_match("a*", "ba"));
    let all = s(&["a.x", "b.x", "b.y"]);
    assert_eq!(
        expand(&s(&["*.x", "b.*", "c"]), &all),
        Ok(s(&["a.x", "b.x", "b.y", "c"]))
    );
    assert!(expand(&s(&["z*"]), &all)
        .unwrap_err()
        .contains("no tag matches `z*`"));
}

#[test]
fn globs_and_completion_cache_against_a_running_provider() {
    let (_srv, ep) = server("udp://*;127.0.0.1:9");
    let (ok, out, _) = probe(&ep, &["read", "lowervalve.*"], "");
    assert!(
        ok && out.starts_with("lowervalve.") && out.contains("lowervalve.open=0"),
        "{out}"
    );
    let (ok, _, err) = probe(&ep, &["read", "nothing.*"], "");
    assert!(!ok && err.contains("no tag matches"), "{err}");
    let (ok, _, err) = probe(&ep, &["write", "--confirm", "lowervalve.op*=1"], "");
    assert!(ok && err.contains("points=lowervalve.open=1 "), "{err}");

    // the QUERY is cached for completion
    let dir = std::env::temp_dir().join(format!("ff-cache-{}", std::process::id()));
    let run = |args: &[&str]| {
        let o = Command::new(BIN)
            .env("XDG_CACHE_HOME", &dir)
            .args(["probe", "--server", &ep])
            .args(args)
            .output()
            .unwrap();
        String::from_utf8_lossy(&o.stdout).into_owned()
    };
    assert_eq!(run(&["cached-tags"]), "");
    run(&["query"]);
    assert!(run(&["cached-tags"]).contains("lowervalve.open\n"));
    let _ = std::fs::remove_dir_all(&dir);
    for (shell, needle) in [
        ("bash", "cached-tags"),
        ("zsh", "compdef"),
        ("fish", "cached-tags"),
    ] {
        let o = Command::new(BIN)
            .args(["completions", shell])
            .output()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&o.stdout).contains(needle),
            "{shell}"
        );
    }
}

#[test]
fn write_checks_against_the_model_and_dry_run_sends_nothing() {
    let (_srv, ep) = server("udp://*;127.0.0.1:9");
    let m = MODEL;
    let (ok, out, err) = probe(
        &ep,
        &["write", "--dry-run", "--model", m, "lowervalve.open=1"],
        "",
    );
    assert!(
        ok && out == "dry run, would send WRITE=lowervalve.open:1,\n" && !err.contains("audit:"),
        "{out}{err}"
    );
    let (_, out, _) = probe(&ep, &["read", "lowervalve.open"], "");
    assert_eq!(out, "lowervalve.open=0\n", "dry run wrote");
    let (ok, _, err) = probe(
        &ep,
        &["write", "--confirm", "--model", m, "lowervalve.open=5"],
        "",
    );
    assert!(
        !ok && err.contains("is binary") && !err.contains("audit:"),
        "{err}"
    );
    let (ok, _, err) = probe(
        &ep,
        &["write", "--dry-run", "--model", m, "lowervalve.open=true"],
        "",
    );
    assert!(ok, "{err}");
}

#[test]
fn scripts_and_conditions_parse() {
    let steps = parse_script(
        "- at: 0.5\n  write: a=1, b=2\n- at: 1\n  write: c=true\n  wait_for: c >= 3\n",
    )
    .unwrap();
    assert_eq!(
        (steps[0].at, steps[0].write.as_str(), steps[1].at),
        (0.5, "a=1, b=2", 1.0)
    );
    for (y, want) in [
        ("- at: -1\n  write: a=1\n", "step 1: `at: -1`"),
        (
            "- at: 2\n  write: a=1\n- at: 1\n  write: a=1\n",
            "step 2: `at: 1`",
        ),
        ("- write: a\n", "step 1: `a`: expected tag=value"),
        (
            "- write: a=1\n  wait_for: a ~ 1\n",
            "step 1: `a ~ 1`: expected",
        ),
        ("- write: a=1\n  wait_for: a > x\n", "`x` is not a number"),
        ("- write: a=1\n  wait_for: '> 1'\n", "no tag before"),
        ("- write: a=1\n  when: 1\n", "when"),
        ("- at: 1\n", "write"),
    ] {
        let e = parse_script(y).unwrap_err();
        assert!(e.contains(want), "{y}: {e}");
    }
    let c = |s: &str, v: f64| Cond::parse(s).unwrap().holds(v);
    assert!(c("t >= 3", 3.0) && !c("t >= 3", 2.9));
    assert!(c("t<=3", 3.0) && !c("t <= 3", 3.1));
    assert!(c("t == 3", 3.0) && !c("t == 3", 4.0));
    assert!(c("t != 3", 4.0) && !c("t != 3", 3.0));
    assert!(c("t > 3", 3.1) && !c("t > 3", 3.0));
    assert!(c("t < 3", 2.9) && !c("t < 3", 3.0));
    assert!(c("x.open == true", 1.0));
    assert_eq!(Cond::parse("a.b>=1"), Cond::parse(" a.b >= 1 "));
}

/// A write lands on the provider's next cycle, so poll the read until it shows `want`.
fn eventually(ep: &str, tag: &str, want: &str) {
    for _ in 0..40 {
        let (_, out, _) = probe(ep, &["read", tag], "");
        if out == format!("{tag}={want}\n") {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    panic!("{tag} never became {want}");
}

fn script(name: &str, body: &str) -> String {
    let p = std::env::temp_dir().join(format!("ff-play-{}-{name}.yaml", std::process::id()));
    std::fs::write(&p, body).unwrap();
    p.to_string_lossy().into_owned()
}

#[test]
fn play_runs_timed_writes_audited_and_dry_run_sends_nothing() {
    let (_srv, ep) = server("udp://*;127.0.0.1:9");
    let ok_script = script(
        "ok",
        "- at: 0\n  write: lowervalve.open=1\n- at: 0.4\n  write: lowervalve.open=0\n  wait_for: lowervalve.open >= 1\n",
    );
    let (ok, out, err) = probe(&ep, &["play", "--dry-run", &ok_script], "");
    assert!(
        ok && out.contains("dry run, at 0s would send WRITE=lowervalve.open:1,")
            && out.contains("at 0.4s once lowervalve.open >= 1 would send")
            && !err.contains("audit:"),
        "{out}{err}"
    );
    let (_, out, _) = probe(&ep, &["read", "lowervalve.open"], "");
    assert_eq!(out, "lowervalve.open=0\n", "dry run wrote");

    let (ok, _, err) = probe(&ep, &["play", &ok_script], "n\n");
    assert!(
        !ok && err.contains("not confirmed") && !err.contains("audit:"),
        "{err}"
    );

    let t = std::time::Instant::now();
    let (ok, _, err) = probe(&ep, &["play", "--confirm", &ok_script], "");
    assert!(ok && t.elapsed().as_millis() >= 400, "{err}");
    assert_eq!(err.matches("audit: write").count(), 2, "{err}");
    assert!(err.contains("points=lowervalve.open=1 result=ACK"), "{err}");
    eventually(&ep, "lowervalve.open", "0");

    // --model rejects a bad value anywhere in the script before step 1 is sent
    let bad = script(
        "bad",
        "- write: lowervalve.open=1\n- write: lowervalve.open=5\n",
    );
    let (ok, _, err) = probe(&ep, &["play", "--confirm", "--model", MODEL, &bad], "");
    assert!(
        !ok && err.contains("step 2:") && !err.contains("audit:"),
        "{err}"
    );
    assert!(!probe(&ep, &["play", "--confirm", "/no/such.yaml"], "").0);
}

#[test]
fn play_aborts_on_err_unless_keep_going() {
    let (_srv, ep) = server("udp://*;127.0.0.1:9");
    let s = script(
        "err",
        "- write: lowervalve.opn=1\n- write: lowervalve.open=1\n",
    );
    let (ok, _, err) = probe(&ep, &["play", "--confirm", &s], "");
    assert!(
        !ok && err.contains("did you mean `lowervalve.open`")
            && err.contains("step 1 failed")
            && err.matches("audit:").count() == 1,
        "{err}"
    );
    std::thread::sleep(std::time::Duration::from_millis(300));
    let (_, out, _) = probe(&ep, &["read", "lowervalve.open"], "");
    assert_eq!(out, "lowervalve.open=0\n", "step 2 ran after step 1 failed");
    let (ok, _, err) = probe(&ep, &["play", "--confirm", "--keep-going", &s], "");
    assert!(
        !ok && err.contains("1 step(s) failed") && err.matches("audit:").count() == 2,
        "{err}"
    );
    eventually(&ep, "lowervalve.open", "1");
}

#[test]
fn play_wait_for_blocks_until_the_tag_changes() {
    let (_srv, ep) = server("udp://*;127.0.0.1:9");
    let s = script(
        "wait",
        "- write: lowergate.open=1\n  wait_for: lowervalve.open == 1\n",
    );
    let mut p = Command::new(BIN)
        .args(["probe", "--server", &ep, "play", "--confirm", &s])
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(700));
    assert!(p.try_wait().unwrap().is_none(), "did not wait");
    assert!(probe(&ep, &["write", "--confirm", "lowervalve.open=1"], "").0);
    assert!(p.wait().unwrap().success());
    eventually(&ep, "lowergate.open", "1");
}

struct Canned(Vec<&'static str>);
impl ferrofire::transport::Requester for Canned {
    fn call(&mut self, _: &[u8]) -> Result<Vec<u8>, String> {
        Ok(self.0.remove(0).as_bytes().to_vec())
    }
}

#[test]
fn wait_for_polls_until_met_or_out_of_time() {
    let c = Cond::parse("t >= 3").unwrap();
    let mut r = Canned(vec!["ACK=t:1,\0", "ACK=t:3,\0"]);
    assert_eq!(
        wait_for(&mut r, &c, std::time::Duration::from_secs(5)),
        Ok(())
    );
    assert!(r.0.is_empty());
    let mut r = Canned(vec!["ACK=t:1,\0"]);
    let e = wait_for(&mut r, &c, std::time::Duration::ZERO).unwrap_err();
    assert!(e.contains("t >= 3") && e.contains("not met"), "{e}");
    let e = wait_for(
        &mut Canned(vec!["ACK=t:x,\0"]),
        &c,
        std::time::Duration::ZERO,
    )
    .unwrap_err();
    assert!(e.contains("not a number"), "{e}");
    let e = wait_for(&mut Canned(vec!["ACK=\0"]), &c, std::time::Duration::ZERO).unwrap_err();
    assert!(e.contains("empty reply"), "{e}");
}

#[test]
fn recorder_writes_one_row_per_cycle_with_stale_flags() {
    let g = group_for("udp://*;239.0.0.1:40000");
    let dg = |msg: &str, seq: u64, cyc: u64| datagrams_msg(&g, msg, seq, cyc, 1).remove(0);
    let mut r = Recorder::new(&g, s(&["a", "b"]), 2);
    assert_eq!(r.header(), "cycle,t,a,a_stale,b,b_stale");
    assert_eq!(r.feed(&dg("a:1,", 1, 1), 0.5), None); // cycle 1 is not done yet
    assert_eq!(r.feed(&dg("b:2,", 2, 1), 0.6), None); // another chunk of cycle 1: same t, b joins
    assert_eq!(r.feed(b"garbage", 0.7), None);
    assert_eq!(r.feed(&dg("a:1,", 2, 1), 0.7), None); // a duplicate seq changes nothing
    assert_eq!(
        r.feed(&dg("a:3,", 3, 2), 1.0).as_deref(),
        Some("1,0.500,1,0,2,0")
    );
    assert_eq!(
        r.feed(&dg("a:4,", 4, 3), 1.5).as_deref(),
        Some("2,1.000,3,0,2,0")
    );
    assert_eq!(
        r.feed(&dg("a:5,", 5, 4), 2.0).as_deref(),
        Some("3,1.500,4,0,2,1")
    ); // b is 2 cycles behind
    assert_eq!(r.finish().as_deref(), Some("4,2.000,5,0,2,1"));
    assert_eq!(r.finish(), None);
    let mut e = Recorder::new(&g, s(&["a", "b"]), 2);
    assert_eq!(e.feed(&dg("a:1,", 1, 1), 0.0), None);
    assert_eq!(e.finish().as_deref(), Some("1,0.000,1,0,,")); // b never seen: empty
}

#[test]
fn record_to_csv_stops_after_duration() {
    let port = free_port();
    let listen = format!("udp://239.0.0.8:{port}");
    let (_srv, ep) = server(&listen);
    let dir = std::env::temp_dir().join(format!("ff-rec-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let csv = dir.join("run.csv");
    let (ok, _, err) = probe(
        &ep,
        &[
            "record",
            "--out",
            csv.to_str().unwrap(),
            "--listen",
            &listen,
            "--tags",
            "lowerwater.*",
            "--duration",
            "1.5",
        ],
        "",
    );
    assert!(ok, "{err}");
    let text = std::fs::read_to_string(&csv).unwrap();
    let mut lines = text.lines();
    assert!(
        lines.next().unwrap().starts_with("cycle,t,lowerwater."),
        "{text}"
    );
    let rows: Vec<&str> = lines.collect();
    assert!(rows.len() >= 2, "{text}");
    let cols = text.lines().next().unwrap().split(',').count();
    assert!(rows.iter().all(|r| r.split(',').count() == cols), "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// README "Record": a changed value becomes `--write tag=value@t` for `ferrofire check`.
const REPLAY: &str = r#"awk -F, 'NR==1{for(i=3;i<=NF;i++)h[i]=$i;next}{for(i=3;i<=NF;i++)if(h[i]!~/_stale$/&&$i!=""&&$i!=p[i]){print "--write";print h[i]"="$i"@"$2;p[i]=$i}}'"#;

#[test]
fn recorded_csv_replays_into_check() {
    let dir = std::env::temp_dir().join(format!("ff-replay-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let csv = dir.join("run.csv");
    std::fs::write(
        &csv,
        "cycle,t,lowervalve.open,lowervalve.open_stale\n1,0.000,0,0\n2,1.000,1,0\n3,2.000,1,0\n",
    )
    .unwrap();
    let o = Command::new("sh")
        .arg("-c")
        .arg(format!(
            "{REPLAY} {} | xargs {BIN} check {MODEL} --steps 100",
            csv.display()
        ))
        .output()
        .unwrap();
    let (out, err) = (
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr),
    );
    assert!(o.status.success() && !err.contains("ignored"), "{err}");
    assert!(
        out.contains("lowervalve.open=true") || out.contains("lowervalve.open=1"),
        "{out}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
