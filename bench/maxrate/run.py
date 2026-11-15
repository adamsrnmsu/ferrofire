#!/usr/bin/env python3
"""Run the maximum-publish-rate matrix (ww-1df.43): a real ferrofire provider -> R real ferrofire field devices, in docker.
Host side, stdlib only; every process runs in a container on the DEFAULT docker daemon (Docker Desktop VM).

Per cell (model, R, engine dt, publish period): a fresh provider container (alias `provider`), R field-device containers (alias `rtu-N`) on the network `ffmax`, and the observer
`ffmaxrate` (bench/maxrate/src, `--pid host`: it reads cpu, RSS and UDP counters of every process from /proc, and joins the same multicast groups as the devices). One cell takes about
WARM + SECS + 15 s. Reps are the outer loop, so slow drift hits every cell alike. Own containers (label ffmax=1, names ffmx-*; only the label is used to find or remove them).
Raw output in docs/bench/results/maxrate/: <model>_R<R>_dt<ms>_pub<ms>_rep<k>.{metrics.csv,s2r.csv.gz,env.txt,prov.log.gz,fd1.log.gz}. Finished cells are skipped (a run resumes).
Env: MODELS (S,M,L,G), RS (1,10), DTS (50,20,10,5,2,1: engine dt in ms, publish period = dt), DECOUPLED (L,G only, R=1: dt:publish pairs, publish slower than dt; default 1:10,1:5; "" = none),
REPS (3), SECS (15), WARM (3), OUTDIR, NOBUILD=1, WAIT (seconds to wait for other cargo containers before a cell, 600), MINIMAL=1 (print the cell list and exit).
The provider refuses to publish faster than it ticks (`every = max(1, round(period / dt))`), so publish < dt is not in the matrix."""
import gzip, hashlib, os, pathlib, subprocess, sys, time

ROOT = pathlib.Path(__file__).resolve().parents[2]
ENV = lambda k, d: [x for x in os.environ.get(k, d).split(",") if x]
MODELS, RS, DTS = ENV("MODELS", "S,M,L,G"), [int(x) for x in ENV("RS", "1,10")], [int(x) for x in ENV("DTS", "50,20,10,5,2,1")]
DEC = [tuple(int(y) for y in x.split(":")) for x in ENV("DECOUPLED", "1:10,1:5")]
REPS, SECS, WARM, WAIT = int(os.environ.get("REPS", "3")), os.environ.get("SECS", "15"), os.environ.get("WARM", "3"), int(os.environ.get("WAIT", "600"))
OUT = ROOT / os.environ.get("OUTDIR", "docs/bench/results/maxrate")
CFG = ROOT / "target-linux/maxrate/cfg"
LABEL, NET, PLAT, IMG = "ffmax=1", "ffmax", "linux/arm64", "ff-rust:22.04"
MNT = ["-v", f"{ROOT}:/src"]
FF, OBS = "/src/target-linux/release/ferrofire", "/src/target-linux/maxrate-build/release/ffmaxrate"


def sh(*a, check=True, **kw):
    return subprocess.run(a, check=check, text=True, **kw)


def out(*a):
    return sh(*a, capture_output=True, check=False).stdout.strip()


def build():
    """cargo on the default daemon, sized for the 7.7 GB VM: 2 jobs, no incremental."""
    sh("docker", "run", "--rm", "--platform", PLAT, *MNT, "-w", "/src", "-e", "CARGO_BUILD_JOBS=2", "-e", "CARGO_INCREMENTAL=0", IMG, "bash", "-c",
       "CARGO_TARGET_DIR=/src/target-linux cargo build --release && cd bench/maxrate && CARGO_TARGET_DIR=/src/target-linux/maxrate-build cargo build --release")


def cleanup():
    names = out("docker", "ps", "-a", "--filter", f"label={LABEL}", "--format", "{{.Names}}").split()
    if names:
        sh("docker", "rm", "-f", *names, check=False, capture_output=True)


def others():
    """Containers that are not ours and look like builds or gates (they contend for the VM); days-old ones are hung leftovers and ignored."""
    mine = out("docker", "ps", "--filter", f"label={LABEL}", "--format", "{{.Names}}").split()
    rows = [l.split("\t") for l in out("docker", "ps", "--format", "{{.Names}}\t{{.Image}}\t{{.Command}}\t{{.RunningFor}}").splitlines()]
    old = lambda up: any(w in up for w in ("day", "week", "month"))
    return [f"{n} {i} {c}" for n, i, c, up in rows if n not in mine and any(k in (i + c) for k in ("ff-rust", "cargo", "mutator")) and not old(up)]


def wait_idle():
    end = time.time() + WAIT
    while others() and time.time() < end:
        time.sleep(20)
    return others()


def cells():
    for rep in range(1, REPS + 1):
        for m in MODELS:
            for r in RS:
                for dt in DTS:
                    yield rep, m, r, dt, dt
            if m in ("L", "G"):
                for dt, pub in DEC:
                    yield rep, m, 1, dt, pub


def run(rep, m, r, dt, pub, rmem):
    name = f"{m}_R{r}_dt{dt}_pub{pub}_rep{rep}"
    base = OUT / name
    if pathlib.Path(f"{base}.metrics.csv").exists():
        return print("skip", name, flush=True)
    print("run ", name, time.strftime("%H:%M:%S"), flush=True)
    cleanup()
    busy = wait_idle()
    t0, others0, load0 = time.strftime("%FT%TZ", time.gmtime()), others(), os.popen("uptime").read().split("load averages:")[-1].strip()
    cfg = CFG / f"{m}_R{r}_dt{dt}_pub{pub}"
    sh("python3", str(ROOT / "bench/maxrate/gen.py"), m, str(r), str(dt), str(pub), str(cfg))
    rel = lambda p: "/src/" + str(pathlib.Path(p).relative_to(ROOT))
    sh("docker", "network", "create", NET, check=False, capture_output=True)
    run_ = lambda nm, alias, *cmd: sh("docker", "run", "-d", "--label", LABEL, "--name", nm, "--platform", PLAT, "--network", NET, "--network-alias", alias, *MNT, IMG, *cmd, stdout=subprocess.DEVNULL)
    run_("ffmx-prov", "provider", FF, "run", rel(cfg / "model.yaml"), "--server", "tcp://0.0.0.0:5555")
    time.sleep(1)
    for i in range(1, r + 1):
        run_(f"ffmx-rtu-{i}", f"rtu-{i}", FF, "field-device", "--file", rel(cfg / f"rtu-{i}.xml"))
    time.sleep(1)
    obs = sh("docker", "run", "--rm", "--label", LABEL, "--name", "ffmx-obs", "--platform", PLAT, "--network", NET, "--pid", "host", *MNT, IMG, OBS,
             "--groups", ",".join(f"239.0.1.{i}:40000" for i in range(1, r + 1)), "--fd-host", "rtu-1", "--ir", "39000", "--period-ms", str(pub), "--step", str(max(1, round(pub / dt))),
             "--warm", WARM, "--secs", SECS, "--out", rel(base), capture_output=True, check=False, timeout=600)
    state = lambda n: out("docker", "inspect", n, "--format", "{{.State.Status}} exit={{.State.ExitCode}} oom={{.State.OOMKilled}}")
    states = {n: state(n) for n in ["ffmx-prov"] + [f"ffmx-rtu-{i}" for i in range(1, r + 1)]}
    for nm, f in (("ffmx-prov", "prov"), ("ffmx-rtu-1", "fd1")):
        lg = sh("docker", "logs", nm, capture_output=True, check=False)
        with gzip.open(f"{base}.{f}.log.gz", "wt") as g:
            g.write(lg.stdout[-200000:] + lg.stderr[-200000:])
    cleanup()
    s2r = pathlib.Path(f"{base}.s2r.csv")
    if s2r.exists():
        with gzip.open(f"{base}.s2r.csv.gz", "wt") as g:
            g.write(s2r.read_text())
        s2r.unlink()
    binary = hashlib.sha256((ROOT / "target-linux/release/ferrofire").read_bytes()).hexdigest()[:16]
    pathlib.Path(f"{base}.env.txt").write_text(
        f"cell {name}\nstart {t0} end {time.strftime('%FT%TZ', time.gmtime())}\nobserver_exit {obs.returncode}\n"
        f"commit {out('git', '-C', str(ROOT), 'rev-parse', 'HEAD')} dirty {bool(out('git', '-C', str(ROOT), 'status', '--porcelain', '--', 'src', 'bench/maxrate'))}\n"
        f"ferrofire_sha256 {binary}\nconfig_sha256 {hashlib.sha256((cfg / 'model.yaml').read_bytes()).hexdigest()[:16]}\n"
        f"host_load_start {load0}\ndocker_vm {out('docker', 'info', '--format', 'cpus={{.NCPU}} mem={{.MemTotal}}')}\nrmem_max {rmem}\n"
        f"containers_at_end {'; '.join(f'{k}: {v}' for k, v in states.items())}\n"
        f"contended_at_start {bool(others0)}{(' (waited ' + str(WAIT) + ' s, still running: ' + '; '.join(busy) + ')') if busy else ''}\nother_containers_start {'; '.join(others0) or 'none'}\n")
    if obs.returncode:
        print("FAILED", name, (obs.stderr + obs.stdout)[-500:], flush=True)
        pathlib.Path(f"{base}.metrics.csv").unlink(missing_ok=True)
        (OUT / f"{name}.failed.txt").write_text(obs.stderr + obs.stdout + "\n" + str(states))


if __name__ == "__main__":
    if os.environ.get("MINIMAL"):
        sys.exit(print("\n".join(map(str, cells()))))
    OUT.mkdir(parents=True, exist_ok=True)
    if not os.environ.get("NOBUILD"):
        build()
    rmem = out("docker", "run", "--rm", "--platform", PLAT, IMG, "cat", "/proc/sys/net/core/rmem_max", "/proc/sys/net/core/rmem_default")
    for c in cells():
        run(*c, rmem.replace("\n", " "))
