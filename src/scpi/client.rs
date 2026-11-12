//! `ferrofire scpi`: a client for any SCPI instrument that speaks the raw socket protocol (LXI port 5025), HiSLIP or VXI-11
//! (`transport`), the field device's `<scpi-server>` included. A message is sent whole; if some header of it ends in `?` one response
//! line is read. With `--check` every command is followed by `SYST:ERR?` until the instrument says `0`, and any other code fails the
//! run. Nothing here is specific to ferrofire's server.

use super::syntax::is_query;
use super::transport::{connect_to, open, parse_target, Lines, Wire, DEFAULT_PORT};
use crate::probe::json_str;
use std::io::{BufRead, IsTerminal, Write};
use std::net::TcpStream;
use std::time::Duration;
/// `--check` reads at most this many errors after one command (the queue of a real instrument is 16 to 20 deep).
const MAX_ERRORS: usize = 32;

#[derive(Debug, clap::Args)]
pub struct ScpiArgs {
    /// Instrument as HOST[:PORT] (the raw socket, port 5025 by default; an IPv6 address in brackets: [::1]:5025) or a VISA resource:
    /// TCPIP::host::INSTR (VXI-11), TCPIP::host::hislip0::INSTR (HiSLIP, hislip0,PORT for another port), TCPIP::host::PORT::SOCKET
    #[arg(value_name = "HOST[:PORT]|RESOURCE")]
    target: String,
    /// Commands to send, one program message each (`;` joins units inside one); a message with a `?` header is answered with a line
    #[arg(value_name = "COMMAND")]
    commands: Vec<String>,
    /// Send the commands in FILE (`-` for standard input), one per line; blank lines and lines starting with # are skipped
    #[arg(long, short = 'f', value_name = "FILE")]
    file: Option<String>,
    /// Seconds to wait to connect and for each response
    #[arg(long, default_value_t = 5.0, value_name = "SECONDS")]
    timeout: f64,
    /// After each command read the instrument's error queue (SYST:ERR?); exit 1, with the errors on stderr, if any was not 0
    #[arg(long)]
    check: bool,
    /// One JSON object per command instead of the bare response
    #[arg(long)]
    json: bool,
    /// TCP port of the VXI-11 portmapper, for a TCPIP::host::INSTR resource (a VISA resource cannot name it)
    #[arg(long, default_value_t = 111, value_name = "PORT")]
    portmapper: u16,
}

/// An error read from `SYST:ERR?`.
#[derive(Debug, PartialEq, Clone)]
pub struct Reported {
    pub code: i32,
    pub message: String,
}

/// One command and what came of it.
#[derive(Debug, PartialEq)]
pub struct Outcome {
    pub command: String,
    /// The response line, for a query.
    pub response: Option<String>,
    /// The errors `--check` found after it.
    pub errors: Vec<Reported>,
}

impl Outcome {
    /// What goes to stdout: the response line of a query, nothing for a command.
    pub fn text(&self) -> Option<&str> {
        self.response.as_deref()
    }

    /// What goes to stderr: one line per error `--check` found.
    pub fn problems(&self) -> Vec<String> {
        self.errors
            .iter()
            .map(|e| format!("error: {}: {},{}", self.command, e.code, e.message))
            .collect()
    }

    pub fn json(&self) -> String {
        let response = self.response.as_deref().map_or("null".into(), json_str);
        let errors: Vec<String> = self
            .errors
            .iter()
            .map(|e| {
                format!(
                    "{{\"code\":{},\"message\":{}}}",
                    e.code,
                    json_str(&e.message)
                )
            })
            .collect();
        format!(
            "{{\"command\":{},\"response\":{response},\"errors\":[{}]}}",
            json_str(&self.command),
            errors.join(",")
        )
    }
}

/// `5,"No error"` -> (5, `No error`): the code, and the text with its quotes taken off (a doubled quote inside is one quote).
pub fn parse_reported(line: &str) -> Result<Reported, String> {
    let bad = || format!("`{line}` is not an SCPI error response (code,\"message\")");
    let (code, msg) = line.split_once(',').ok_or_else(bad)?;
    let code = code.trim().parse().map_err(|_| bad())?;
    let msg = msg.trim();
    let message = match msg.strip_prefix('"').and_then(|m| m.strip_suffix('"')) {
        Some(inner) => inner.replace("\"\"", "\""),
        None => msg.to_string(),
    };
    Ok(Reported { code, message })
}

/// The host and the port text of `HOST[:PORT]`, `[V6]:PORT`, `[V6]` or a bare IPv6 address (more than one `:`, so no port).
fn host_port(t: &str) -> Option<(&str, Option<&str>)> {
    if let Some(v6) = t.strip_prefix('[') {
        let (host, rest) = v6.split_once(']')?;
        return match rest {
            "" => Some((host, None)),
            r => Some((host, Some(r.strip_prefix(':')?))),
        };
    }
    match t.split_once(':') {
        Some((host, port)) if !port.contains(':') => Some((host, Some(port))),
        _ => Some((t, None)),
    }
}

pub fn split_target(t: &str) -> Result<(String, u16), String> {
    let bad = || format!("`{t}`: expected HOST[:PORT]");
    let (host, port) = host_port(t).ok_or_else(bad)?;
    let port = port.map_or(Ok(DEFAULT_PORT), |p| p.parse().map_err(|_| bad()))?;
    match host.is_empty() {
        true => Err(bad()),
        false => Ok((host.to_string(), port)),
    }
}

/// The commands of a script: trimmed, without blank lines and `#` comments.
pub fn script_commands(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_string)
        .collect()
}

/// One connection to an instrument, over whichever protocol.
pub struct Link<T> {
    wire: T,
    wait: Duration,
}

impl<R: BufRead, W: Write> Link<Lines<R, W>> {
    /// A raw-socket link over a reader and a writer.
    pub fn new(rd: R, wr: W, wait: Duration) -> Self {
        Link {
            wire: Lines { rd, wr },
            wait,
        }
    }
}

/// What an instrument answers to a query followed by `*OPC?`: the response and `;1`, or only `1` when the query was rejected and so
/// has no response. `None` is the rejected query; anything else (an instrument that does not join the two) is taken as it is.
fn without_opc(line: String) -> Option<String> {
    match line.strip_suffix(";1") {
        Some(response) => Some(response.to_string()),
        None if line == "1" => None,
        None => Some(line),
    }
}

impl<T: Wire> Link<T> {
    /// Send `sent` (the command `cmd`, possibly with more after it); for a query, read its one response line. A query the instrument rejects gets no response at all.
    fn exchange(&mut self, cmd: &str, sent: &str) -> Result<Option<String>, Failure> {
        let fail = |msg: String| Failure {
            msg,
            timeout: false,
        };
        self.wire
            .send(sent)
            .map_err(|e| fail(format!("sending `{cmd}`: {e}")))?;
        if !is_query(cmd) {
            return Ok(None);
        }
        match self.wire.recv() {
            Ok(None) => Err(fail(format!(
                "the instrument closed the connection before answering `{cmd}`"
            ))),
            Ok(line) => Ok(line),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                Err(Failure {
                    msg: format!(
                        "no response to `{cmd}` within {} s",
                        self.wait.as_secs_f64()
                    ),
                    timeout: true,
                })
            }
            Err(e) => Err(fail(format!("reading the response to `{cmd}`: {e}"))),
        }
    }

    /// The instrument's error queue, oldest first, until it reports 0.
    fn errors(&mut self) -> Result<Vec<Reported>, String> {
        let mut out = vec![];
        while out.len() < MAX_ERRORS {
            let line = self
                .exchange("SYST:ERR?", "SYST:ERR?")
                .map_err(|f| f.msg)?
                .unwrap_or_default();
            match parse_reported(&line)? {
                r if r.code == 0 => break,
                r => out.push(r),
            }
        }
        Ok(out)
    }

    /// Run one command; with `check`, its errors too. A query is sent with `;*OPC?` after it, so that an instrument which rejects the
    /// query still answers (with the `1`) and is not waited out; a timeout is then a silent instrument, which fails the run unless
    /// its error queue says why.
    pub fn run(&mut self, cmd: &str, check: bool) -> Result<Outcome, String> {
        let query = check && is_query(cmd);
        let sent = if query {
            format!("{cmd};*OPC?")
        } else {
            cmd.to_string()
        };
        let (response, late) = match self.exchange(cmd, &sent) {
            Ok(r) if query => (r.and_then(without_opc), None),
            Ok(r) => (r, None),
            Err(f) if f.timeout && check => (None, Some(f.msg)),
            Err(f) => return Err(f.msg),
        };
        let errors = if check { self.errors()? } else { vec![] };
        match (errors.is_empty(), late) {
            (true, Some(msg)) => Err(msg),
            _ => Ok(Outcome {
                command: cmd.to_string(),
                response,
                errors,
            }),
        }
    }
}

/// Why an exchange failed; `timeout`: the instrument was silent (as opposed to gone or unwritable).
struct Failure {
    msg: String,
    timeout: bool,
}

/// Print an outcome the way the flags ask; `false` if it carried errors.
fn show(o: &Outcome, json: bool) -> bool {
    match (json, o.text()) {
        (true, _) => println!("{}", o.json()),
        (false, Some(t)) => println!("{t}"),
        (false, None) => {}
    }
    o.problems().iter().for_each(|p| eprintln!("{p}"));
    o.errors.is_empty()
}

pub fn connect(target: &str, wait: Duration) -> Result<TcpStream, String> {
    let (host, port) = split_target(target)?;
    connect_to(&host, port, wait)
}

fn commands(a: &ScpiArgs) -> Result<Vec<String>, String> {
    let mut cmds = a.commands.clone();
    if let Some(f) = &a.file {
        let text = match f.as_str() {
            "-" => std::io::read_to_string(std::io::stdin()).map_err(|e| format!("stdin: {e}")),
            path => std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}")),
        }?;
        cmds.extend(script_commands(&text));
    }
    Ok(cmds)
}

/// Read commands from `input` until `quit`, `exit` or its end, prompting on `prompt`; an instrument error does not end the session.
fn repl<T: Wire>(
    link: &mut Link<T>,
    input: &mut impl BufRead,
    prompt: &mut impl Write,
    (check, json): (bool, bool),
) -> Result<bool, String> {
    let mut ok = true;
    loop {
        let _ = write!(prompt, "scpi> ").and_then(|()| prompt.flush());
        let mut line = String::new();
        if input
            .read_line(&mut line)
            .map_err(|e| format!("stdin: {e}"))?
            == 0
        {
            return Ok(ok);
        }
        match line.trim() {
            "" => {}
            "quit" | "exit" => return Ok(ok),
            cmd => ok &= show(&link.run(cmd, check)?, json),
        }
    }
}

fn wait_of(a: &ScpiArgs) -> Result<Duration, String> {
    Duration::try_from_secs_f64(a.timeout)
        .ok()
        .filter(|d| !d.is_zero())
        .ok_or_else(|| {
            format!(
                "--timeout {}: expected a positive number of seconds",
                a.timeout
            )
        })
}

fn connect_link(a: &ScpiArgs, wait: Duration) -> Result<Link<Box<dyn Wire>>, String> {
    let wire = open(&parse_target(&a.target)?, wait, a.portmapper)?;
    Ok(Link { wire, wait })
}

/// Nothing to send and a person at the keyboard.
fn interactive(a: &ScpiArgs, cmds: &[String]) -> bool {
    cmds.is_empty() && a.file.is_none() && std::io::stdin().is_terminal()
}

fn run(a: &ScpiArgs) -> Result<bool, String> {
    let wait = wait_of(a)?;
    let cmds = commands(a)?;
    let mut link = connect_link(a, wait)?;
    if interactive(a, &cmds) {
        let (mut stdin, mut stderr) = (std::io::stdin().lock(), std::io::stderr());
        return repl(&mut link, &mut stdin, &mut stderr, (a.check, a.json));
    }
    cmds.iter().try_fold(true, |ok, cmd| {
        Ok(show(&link.run(cmd, a.check)?, a.json) && ok)
    })
}

/// Exit 0 when every command ran (and, with `--check`, left no error), 1 when the instrument reported errors, 2 when the run itself
/// failed (no connection, no response in time, a bad argument).
pub fn main(a: ScpiArgs) {
    match run(&a) {
        Ok(true) => {}
        Ok(false) => std::process::exit(1),
        Err(e) => {
            eprintln!("ferrofire scpi: {e}");
            std::process::exit(2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn link(replies: &str) -> Link<Lines<Cursor<Vec<u8>>, Vec<u8>>> {
        Link::new(
            Cursor::new(replies.as_bytes().to_vec()),
            vec![],
            Duration::from_secs(2),
        )
    }

    fn sent(l: &Link<Lines<Cursor<Vec<u8>>, Vec<u8>>>) -> String {
        String::from_utf8(l.wire.wr.clone()).unwrap()
    }

    #[test]
    fn a_query_reads_one_line_and_a_command_none() {
        let mut l = link("a,b\r\nsecond\n");
        let o = l.run("*IDN?", false).unwrap();
        assert_eq!((o.text(), o.errors.len()), (Some("a,b"), 0));
        assert_eq!(l.run("*CLS", false).unwrap().response, None);
        assert_eq!(
            l.run("*CLS;*OPC?", false).unwrap().response.as_deref(),
            Some("second")
        );
        assert_eq!(sent(&l), "*IDN?\n*CLS\n*CLS;*OPC?\n");
        assert!(l
            .run("*IDN?", false)
            .unwrap_err()
            .contains("closed the connection before answering `*IDN?`"));
    }

    #[test]
    fn check_reads_the_error_queue_until_it_says_zero() {
        let mut l = link("-113,\"Undefined header;X\"\n-222,\"Data out of range\"\n+0,\"No error\"\n0,\"No error\"\n");
        let o = l.run("X", true).unwrap();
        assert_eq!(
            o.errors,
            [
                Reported {
                    code: -113,
                    message: "Undefined header;X".into()
                },
                Reported {
                    code: -222,
                    message: "Data out of range".into()
                },
            ]
        );
        assert_eq!(sent(&l), "X\nSYST:ERR?\nSYST:ERR?\nSYST:ERR?\n");
        assert_eq!(l.run("*CLS", true).unwrap().errors, []);
        let mut l = link("5,\"positive codes fail too\"\n0,\"No error\"\n");
        assert_eq!(l.run("Y", true).unwrap().errors[0].code, 5);
        let mut l = link(&"1,\"x\"\n".repeat(40));
        assert_eq!(l.run("Z", true).unwrap().errors.len(), MAX_ERRORS);
        let mut l = link("garbage\n");
        assert!(l
            .run("Z", true)
            .unwrap_err()
            .contains("not an SCPI error response"));
    }

    #[test]
    fn a_connection_that_fails_for_another_reason_is_not_waited_on_under_check() {
        let e = link("").run("*IDN?", true).unwrap_err();
        assert_eq!(
            e,
            "the instrument closed the connection before answering `*IDN?`"
        );
    }

    #[test]
    fn a_connection_has_the_timeouts_and_nodelay_set() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let target = listener.local_addr().unwrap().to_string();
        let wait = Duration::from_millis(1234);
        let s = connect(&target, wait).unwrap();
        assert_eq!(s.read_timeout().unwrap(), Some(wait));
        assert_eq!(s.write_timeout().unwrap(), Some(wait));
        assert!(s.nodelay().unwrap());
        assert!(connect("127.0.0.1:1", wait)
            .unwrap_err()
            .contains("connect 127.0.0.1:1"));
    }

    #[test]
    fn error_lines_are_parsed_with_or_without_quotes() {
        let r = |code, message: &str| {
            Ok(Reported {
                code,
                message: message.into(),
            })
        };
        assert_eq!(parse_reported("0,\"No error\""), r(0, "No error"));
        assert_eq!(
            parse_reported("-200, \"say \"\"hi\"\"\" "),
            r(-200, "say \"hi\"")
        );
        assert_eq!(
            parse_reported("-100,Command error"),
            r(-100, "Command error")
        );
        assert_eq!(parse_reported("+0,\"No error\""), r(0, "No error"));
        assert_eq!(parse_reported("-1,\"a,b\""), r(-1, "a,b"));
        assert_eq!(parse_reported("-1,\""), r(-1, "\""));
        for bad in ["", "0", "x,\"y\"", ",\"y\""] {
            assert!(parse_reported(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn targets_are_host_port_with_5025_by_default() {
        let t = |s: &str| split_target(s);
        assert_eq!(t("lab"), Ok(("lab".into(), 5025)));
        assert_eq!(t("10.0.0.5:9"), Ok(("10.0.0.5".into(), 9)));
        assert_eq!(t("[::1]"), Ok(("::1".into(), 5025)));
        assert_eq!(t("[::1]:6000"), Ok(("::1".into(), 6000)));
        assert_eq!(t("fe80::1"), Ok(("fe80::1".into(), 5025)));
        for bad in [
            "", ":5025", "h:", "h:x", "h:70000", "[::1", "[::1]x", "[::1]:", "[]", "[]:5",
        ] {
            assert!(t(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_script_has_no_blank_lines_or_comments() {
        assert_eq!(
            script_commands("# set up\n*CLS\n\n  SOUR:X 1  \n   # later\n*IDN?\n"),
            ["*CLS", "SOUR:X 1", "*IDN?"]
        );
        assert!(script_commands("").is_empty());
    }

    #[test]
    fn a_timeout_names_the_command() {
        struct Slow;
        impl std::io::Read for Slow {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::ErrorKind::WouldBlock.into())
            }
        }
        let mut l = Link::new(
            std::io::BufReader::new(Slow),
            vec![],
            Duration::from_millis(1500),
        );
        assert_eq!(
            l.run("*IDN?", false).unwrap_err(),
            "no response to `*IDN?` within 1.5 s"
        );
        assert_eq!(
            l.run("*IDN?", true).unwrap_err(),
            "no response to `SYST:ERR?` within 1.5 s",
            "a silent instrument is a failure also when its error queue cannot be read"
        );
    }

    /// Lines to read, in order; `None` is a read that times out.
    struct Script(Vec<Option<&'static str>>);
    impl std::io::Read for Script {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            match self.0.remove(0) {
                None => Err(std::io::ErrorKind::TimedOut.into()),
                Some(l) => {
                    buf[..l.len()].copy_from_slice(l.as_bytes());
                    Ok(l.len())
                }
            }
        }
    }

    fn scripted(
        lines: Vec<Option<&'static str>>,
    ) -> Link<Lines<std::io::BufReader<Script>, Vec<u8>>> {
        Link::new(
            std::io::BufReader::new(Script(lines)),
            vec![],
            Duration::from_secs(1),
        )
    }

    #[test]
    fn a_query_the_instrument_rejects_is_explained_by_its_error_queue_under_check() {
        let mut l = scripted(vec![
            Some("1\n"),
            Some("-113,\"Undefined header\"\n"),
            Some("0,\"No error\"\n"),
        ]);
        let o = l.run("NOPE?", true).unwrap();
        assert_eq!((o.response, o.errors[0].code), (None, -113));
        assert_eq!(
            String::from_utf8(l.wire.wr).unwrap(),
            "NOPE?;*OPC?\nSYST:ERR?\nSYST:ERR?\n",
            "the *OPC? answers at once, so the timeout is not waited out"
        );
        let mut l = scripted(vec![Some("2.5;1\n"), Some("0,\"No error\"\n")]);
        let o = l.run("MEAS:SCAL:X?", true).unwrap();
        assert_eq!((o.response.as_deref(), o.errors.len()), (Some("2.5"), 0));
        let mut l = scripted(vec![Some("a;b;1\n"), Some("0,\"No error\"\n")]);
        assert_eq!(
            l.run("A?;B?", true).unwrap().response.as_deref(),
            Some("a;b")
        );
        let mut l = scripted(vec![Some("not joined\n"), Some("0,\"No error\"\n")]);
        assert_eq!(
            l.run("A?", true).unwrap().response.as_deref(),
            Some("not joined")
        );
        let mut l = scripted(vec![Some("x\n")]);
        assert_eq!(l.run("A?", false).unwrap().response.as_deref(), Some("x"));
        assert_eq!(
            String::from_utf8(l.wire.wr).unwrap(),
            "A?\n",
            "without --check nothing is added"
        );
        let mut l = scripted(vec![None, Some("0,\"No error\"\n")]);
        assert!(l
            .run("NOPE?", true)
            .unwrap_err()
            .contains("no response to `NOPE?`"));
    }

    #[test]
    fn the_repl_prompts_runs_each_line_and_stops_at_quit_or_the_end() {
        let mut l = link("1;1\n-113,\"x\"\n0,\"No error\"\n0,\"No error\"\n");
        let (mut prompt, mut input) = (
            vec![],
            Cursor::new(b"\n  *OPC?  \nFOO\nquit\n*IDN?\n".to_vec()),
        );
        assert_eq!(
            repl(&mut l, &mut input, &mut prompt, (true, false)),
            Ok(false)
        );
        assert_eq!(
            String::from_utf8(prompt.clone()).unwrap(),
            "scpi> scpi> scpi> scpi> "
        );
        assert_eq!(
            sent(&l),
            "*OPC?;*OPC?\nSYST:ERR?\nSYST:ERR?\nFOO\nSYST:ERR?\n"
        );
        let mut l = link("a\n");
        let mut input = Cursor::new(b"exit\n*IDN?\n".to_vec());
        assert_eq!(
            repl(&mut l, &mut input, &mut vec![], (false, false)),
            Ok(true)
        );
        assert_eq!(sent(&l), "");
        let mut input = Cursor::new(b"*IDN?".to_vec());
        assert_eq!(
            repl(&mut l, &mut input, &mut vec![], (false, true)),
            Ok(true)
        );
        assert_eq!(sent(&l), "*IDN?\n");
        let mut input = Cursor::new(b"*IDN?\n".to_vec());
        assert!(repl(&mut link(""), &mut input, &mut vec![], (false, false)).is_err());
    }

    #[test]
    fn outcomes_print_as_text_and_json() {
        let o = Outcome {
            command: "A \"q\"?".into(),
            response: Some("1;x".into()),
            errors: vec![Reported {
                code: -113,
                message: "Undefined \"h\"".into(),
            }],
        };
        assert_eq!(o.text(), Some("1;x"));
        assert_eq!(o.problems(), ["error: A \"q\"?: -113,Undefined \"h\""]);
        assert_eq!(
            o.json(),
            "{\"command\":\"A \\\"q\\\"?\",\"response\":\"1;x\",\"errors\":[{\"code\":-113,\"message\":\"Undefined \\\"h\\\"\"}]}"
        );
        let c = Outcome {
            command: "*CLS".into(),
            response: None,
            errors: vec![],
        };
        assert_eq!(
            c.json(),
            "{\"command\":\"*CLS\",\"response\":null,\"errors\":[]}"
        );
        assert_eq!(c.text(), None);
        assert!(c.problems().is_empty());
    }
}
