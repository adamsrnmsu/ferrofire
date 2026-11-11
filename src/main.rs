//! ferrofire CLI. Also the `simulinksolver` entry point: selected when argv[0] ends in `simulinksolver`.

#![forbid(unsafe_code)]

use clap::{CommandFactory, Parser, Subcommand};
use ferrofire::model::{Model, Value};

#[derive(Parser)]
#[command(
    version,
    about = "Process and power-flow simulator for sceptre/phenix ranges"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Serve a model over the field-device REP protocol and publish its tags
    Run(ferrofire::bennu::RunArgs),
    /// Poll a field-device REP server and publish what it answers
    PullBridge(ferrofire::pull::PullCli),
    /// pybennu-power-solver compatible power-flow daemon
    Pypower(ferrofire::pypower::Cli),
    /// Shared-memory simulinksolver stand-in; takes `--model FILE`, ignores the legacy solver flags (-tf inf, -v, ...)
    #[command(disable_help_flag = true)]
    Simulink {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Offline run printing the model snapshot at t=0 and after N steps
    Check(CheckArgs),
    /// Toggle file logging, level and tail on a running process
    #[command(disable_help_flag = true)]
    Log {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Query, read, write and watch tags on a running provider (bennu-probe equivalent)
    Probe(ferrofire::probe::ProbeCli),
    /// Talk SCPI to an instrument (the field device's <scpi-server> or a real one): send commands, read responses, check the error queue
    Scpi(ferrofire::scpi::client::ScpiArgs),
    /// Live view of tag values, age and staleness with a log pane (local process, datagram stream or recorded CSV)
    Top(ferrofire::top::TopArgs),
    /// Check a range's plumbing: provider reachable, multicast arriving, gaps, skew, stale tags; prints a fix per problem
    Doctor(ferrofire::doctor::DoctorArgs),
    /// pybennu groundtruth_monitor drop-in: provider datagrams -> Elasticsearch/OpenSearch bennu-YYYY.MM.DD
    #[cfg(feature = "elastic")]
    Groundtruth(ferrofire::groundtruth::Cli),
    /// Describe a tag from the model file: kind, expression, inputs, units, limits
    Explain {
        /// Model file
        #[arg(value_name = "MODEL.YAML")]
        path: String,
        tag: String,
    },
    /// Stand-in for bennu-field-device: reads a sceptre config.xml, serves protocols over the provider's data
    FieldDevice(ferrofire::fd::Cli),
    /// Print a shell completion script
    Completions { shell: clap_complete::Shell },
}

/// Run as an install name (`simulinksolver`, `pybennu-power-solver`) when argv[0] says so.
fn by_install_name(prog: &str, rest: &[String]) -> bool {
    match prog {
        p if p.ends_with("simulinksolver") => ferrofire::simulink::main(rest),
        p if p.ends_with("pybennu-power-solver") => ferrofire::pypower::main(rest),
        _ => return false,
    }
    true
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (prog, rest) = args.split_first().expect("argv[0]");
    if by_install_name(prog, rest) {
        return;
    }
    dispatch(Cli::parse().cmd);
}

/// The subcommands that only read files or print; any other command is handed back.
fn offline(cmd: Cmd) -> Option<Cmd> {
    match cmd {
        Cmd::Check(a) => check(a),
        Cmd::Explain { path, tag } => explain(&path, &tag),
        Cmd::Completions { shell } => completions(shell),
        other => return Some(other),
    }
    None
}

/// The subcommands that look at a running range.
fn inspect(cmd: Cmd) -> Option<Cmd> {
    match cmd {
        Cmd::Probe(a) => ferrofire::probe::main(a),
        Cmd::Scpi(a) => ferrofire::scpi::client::main(a),
        Cmd::Doctor(a) => ferrofire::doctor::main(a),
        Cmd::Top(a) => ferrofire::top::main(a),
        other => return Some(other),
    }
    None
}

fn dispatch(cmd: Cmd) {
    let Some(cmd) = offline(cmd).and_then(inspect) else {
        return;
    };
    match cmd {
        Cmd::Run(a) => ferrofire::bennu::main(a),
        Cmd::PullBridge(a) => ferrofire::pull::main(a),
        Cmd::Pypower(a) => ferrofire::pypower::run_cli(a),
        Cmd::Simulink { args } => ferrofire::simulink::main(&args),
        Cmd::Log { args } => ferrofire::log::main(&args),
        Cmd::FieldDevice(a) => std::process::exit(ferrofire::fd::main(a)),
        #[cfg(feature = "elastic")]
        Cmd::Groundtruth(a) => ferrofire::groundtruth::run_cli(a),
        _ => {}
    }
}

/// Tag completion for `probe read|write|watch`, appended to the generated script: the names come from `probe cached-tags`
/// (the last `probe query`, read, write or watch). ponytail: elvish and powershell get the static script only.
fn tag_hook(shell: clap_complete::Shell) -> &'static str {
    use clap_complete::Shell::*;
    match shell {
        Bash => "\n_ferrofire_tags() {\n  local cur=${COMP_WORDS[COMP_CWORD]}\n  if [[ ${COMP_WORDS[1]} == probe && ${COMP_WORDS[2]} =~ ^(read|write|watch)$ && $cur != -* ]]; then\n    COMPREPLY=($(compgen -W \"$(ferrofire probe cached-tags 2>/dev/null)\" -- \"$cur\"))\n  else\n    _ferrofire \"$@\"\n  fi\n}\ncomplete -F _ferrofire_tags -o bashdefault -o default ferrofire\n",
        Zsh => "\n_ferrofire_tags() {\n  if [[ $words[2] == probe && $words[3] == (read|write|watch) && $words[CURRENT] != -* ]]; then\n    compadd -- ${(f)\"$(ferrofire probe cached-tags 2>/dev/null)\"}\n  else\n    _ferrofire \"$@\"\n  fi\n}\ncompdef _ferrofire_tags ferrofire\n",
        Fish => "\ncomplete -c ferrofire -n '__fish_seen_subcommand_from probe; and __fish_seen_subcommand_from read write watch' -f -a '(ferrofire probe cached-tags 2>/dev/null)'\n",
        _ => "",
    }
}

fn completions(shell: clap_complete::Shell) {
    clap_complete::generate(
        shell,
        &mut Cli::command(),
        "ferrofire",
        &mut std::io::stdout(),
    );
    print!("{}", tag_hook(shell));
}

type Write = (f64, String, f64);

/// `tag=value[@t]` (value `true`/`false` allowed) -> (t, tag, value).
fn parse_write(s: &str) -> Result<Write, String> {
    let bad = || format!("`{s}`: expected tag=value[@t]");
    let (tv, t) = s.split_once('@').unwrap_or((s, "0"));
    let (tag, v) = tv.split_once('=').ok_or_else(bad)?;
    let v = match v {
        "true" => 1.0,
        "false" => 0.0,
        _ => v.parse().map_err(|_| bad())?,
    };
    Ok((t.parse().map_err(|_| bad())?, tag.to_string(), v))
}

#[derive(clap::Args)]
struct CheckArgs {
    /// Model file
    #[arg(value_name = "MODEL.YAML")]
    path: String,
    /// Steps to run
    #[arg(long, default_value_t = 100)]
    steps: usize,
    /// Set a tag at model time t, repeatable
    #[arg(long = "write", value_name = "TAG=VALUE[@T]", value_parser = parse_write)]
    writes: Vec<Write>,
}

/// Print what defines `tag` in the model file.
fn explain(path: &str, tag: &str) {
    let yaml = std::fs::read_to_string(path).unwrap_or_else(|e| fail(format!("{path}: {e}")));
    let dir = std::path::Path::new(path).parent();
    match ferrofire::model::explain(&yaml, dir, tag) {
        Ok(s) => print!("{s}"),
        Err(e) => fail(e),
    }
}

fn fail(msg: impl std::fmt::Display) -> ! {
    eprintln!("{msg}");
    std::process::exit(1)
}

/// Apply (and drop) the writes due at or before the model's current time.
fn apply_due(m: &mut Model, writes: &mut Vec<Write>) {
    let due: Vec<_> = writes
        .iter()
        .filter(|(t, ..)| *t <= m.time + 1e-9)
        .map(|(_, tag, v)| (tag.clone(), Value::Analog(*v)))
        .collect();
    writes.retain(|(t, ..)| *t > m.time + 1e-9);
    for tag in m.apply_writes(&due) {
        eprintln!("write to unknown/readonly tag {tag} ignored");
    }
}

/// Offline run printing the snapshot at t=0 and after N steps; `--write tag=value@t` applies at model time t.
fn check(
    CheckArgs {
        path,
        steps,
        mut writes,
    }: CheckArgs,
) {
    let yaml = std::fs::read_to_string(&path).unwrap_or_else(|e| fail(format!("{path}: {e}")));
    let mut m = Model::load_in(&yaml, std::path::Path::new(&path).parent())
        .unwrap_or_else(|e| fail(format!("{path}: {e}")));
    for _ in 0..steps {
        apply_due(&mut m, &mut writes);
        m.step(m.dt);
    }
    println!("t={:.3}", m.time);
    for (tag, v) in m.snapshot() {
        match v {
            Value::Analog(x) => println!("{tag}={x:.6}"),
            Value::Binary(b) => println!("{tag}={b}"),
        }
    }
}
