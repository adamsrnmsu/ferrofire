//! ferrofire CLI. Also the `simulinksolver` entry point: selected when argv[0] ends in `simulinksolver`.

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
    match Cli::parse().cmd {
        Cmd::Run(a) => ferrofire::bennu::main(a),
        Cmd::PullBridge(a) => ferrofire::pull::main(a),
        Cmd::Pypower(a) => ferrofire::pypower::run_cli(a),
        Cmd::Simulink { args } => ferrofire::simulink::main(&args),
        Cmd::FieldDevice(a) => std::process::exit(ferrofire::fd::main(a)),
        Cmd::Check(a) => check(a),
        Cmd::Completions { shell } => completions(shell),
    }
}

fn completions(shell: clap_complete::Shell) {
    clap_complete::generate(
        shell,
        &mut Cli::command(),
        "ferrofire",
        &mut std::io::stdout(),
    );
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
