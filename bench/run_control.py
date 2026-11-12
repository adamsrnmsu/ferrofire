#!/usr/bin/env python3
"""Run the control-latency matrix (ww-1df.23.3, epic ww-1df.23): breaker flips and generator spin-up/down against 1, 2, 5, 10 RTUs, in docker.
Host side, stdlib only; cargo and every bench process run in containers on the DEFAULT docker daemon (Docker Desktop).

Per (rep, R) one stack: a provider container (`ferrofire run --stamp-writes`, alias `provider`), R field-device containers (alias `rtu-N`) on the
network `ffcontrol`, and the SCADA driver `ffcontrol` (bench/control) as a one-shot container that runs every cell against it (cells restore the
grid after themselves). Reps are the outer loop so slow drift hits every cell alike. Own containers (name `ffc-*` and the label `ffcontrol=1`; only the label is ever used to find or remove them), own network.
Raw output in docs/bench/results/control/<stack>/: R<R>_rep<k>.cmds.csv (one row per command), .cells.csv (one row per phase), .driver.log,
.prov.log.gz (provider output: the `Applied:` stamps), .fd.log.gz, .env.txt (commit, binary hash, host load, other containers, contention).
Finished runs are skipped, so an interrupted matrix resumes.

Env: STACK (ferrofire), OUTDIR, RS (1,2,5,10), REPS (5), CELLS (default: the full matrix), SLOW (cells that get SLOW_REPS reps only), SLOW_REPS,
NOBUILD=1 (use the binaries already in target-linux), WAIT (seconds to wait for other cargo containers to finish before a run, 600).
STACK=stock: pybennu + stock bennu field devices (image ffc-stock:22.04 from bench/Dockerfile.stock; bench/stock_provider.py), same driver and cells, raw in .../stock/.
Joining the provider stamps to the commands: bench/control_join.py."""
import gzip, hashlib, os, pathlib, subprocess, sys, time

ROOT = pathlib.Path(__file__).resolve().parent.parent
STACK = os.environ.get("STACK", "ferrofire")
ENV = lambda k, d: os.environ.get(k, d).split(",")
RS, REPS = [int(r) for r in ENV("RS", "1,2,5,10")], int(os.environ.get("REPS", "5"))
BREAKERS, GENS = [1, 5, 10, 50, 100, 1000], [1, 2, 5, 10]
CELLS = ENV("CELLS", ",".join([f"breaker:{m}:{n}" for n in BREAKERS for m in ("seq", "sim")] + [f"gen:{m}:{n}" for n in GENS for m in ("seq", "sim")]))
SLOW, SLOW_REPS = [c for c in ENV("SLOW", "") if c], int(os.environ.get("SLOW_REPS", str(REPS)))
WAIT = int(os.environ.get("WAIT", "600"))
OUT = ROOT / os.environ.get("OUTDIR", f"docs/bench/results/control/{STACK}")  # OUTDIR: a scratch dir for smoke runs, relative to the repo
GRID = ROOT / "target-linux/control-grid"
LABEL = "ffcontrol=1"
IMG, NET, PLAT = "ff-rust:22.04", "ffcontrol", "linux/arm64"
FF, CTL = "/src/target-linux/release/ferrofire", "/src/target-linux/ctl/release/ffcontrol"
MNT = ["-v", f"{ROOT}:/src"]


def sh(*a, check=True, **kw):
    return subprocess.run(a, check=check, text=True, **kw)


def out(*a):
    return sh(*a, capture_output=True, check=False).stdout.strip()


def build():
    """cargo on the default daemon, sized for the 7.7 GB VM: 2 jobs, no incremental (the link OOMs otherwise)."""
    run = ["docker", "run", "--rm", "--platform", PLAT, *MNT, "-w", "/src", "-e", "CARGO_BUILD_JOBS=2", "-e", "CARGO_INCREMENTAL=0", IMG]
    sh(*run, "bash", "-c", "CARGO_TARGET_DIR=/src/target-linux cargo build --release && cd bench/control && CARGO_TARGET_DIR=/src/target-linux/ctl cargo build --release")


def grid():
    sh("python3", str(ROOT / "bench/gen_grid.py"), str(GRID), "--rtus", ",".join(map(str, RS)), stdout=subprocess.DEVNULL)


def start_ferrofire(r):
    """provider + R ferrofire field devices; returns the stream endpoint template of the RTUs (one group each)."""
    d = f"/src/target-linux/control-grid/R{r}"
    docker_run("ffc-prov", "provider", FF, "run", f"{d}/model.yaml", "--stamp-writes")
    for n in range(1, r + 1):
        docker_run(f"ffc-rtu-{n}", f"rtu-{n}", FF, "field-device", "--file", f"{d}/ferrofire/rtu-{n}.xml")
    return f"{d}/scada.json", "udp://*;239.0.1.{n}:40000"


def start_stock(r):
    """stock bennu: pybennu-power-solver (PyPower; bench/stock_provider.py = stock plus `Applied:` stamps) + R bennu-field-deviced, all one image (STOCK_IMG).
    One publish group for every RTU (stock publishes the whole model, once a second, to the one endpoint of config.ini)."""
    d = f"/src/target-linux/control-grid/R{r}"
    docker_run("ffc-prov", "provider", "python3", "/src/bench/stock_provider.py", "start", "-c", f"{d}/config.ini", img=STOCK_IMG,
               extra=["-v", f"{GRID}/case.py:/etc/sceptre/case_control.py:ro"])
    for n in range(1, r + 1):
        docker_run(f"ffc-rtu-{n}", f"rtu-{n}", "bash", "-c", f"bennu-field-deviced --command start --file {d}/bennu/rtu-{n}.xml && exec tail --pid=$(pgrep -o bennu-field-d) -f /dev/null", img=STOCK_IMG)  # it daemonizes
    return f"{d}/scada.json", "udp://*;239.0.0.1:40000"


STACKS = {"ferrofire": start_ferrofire, "stock": start_stock}
STOCK_IMG = "ffc-stock:22.04"


def docker_run(name, alias, *cmd, img=IMG, extra=()):
    sh("docker", "run", "-d", "--label", LABEL, "--name", name, "--platform", PLAT, "--network", NET, "--network-alias", alias, *MNT, *extra, img, *cmd, stdout=subprocess.DEVNULL)


def cleanup():
    names = out("docker", "ps", "-a", "--filter", f"label={LABEL}", "--format", "{{.Names}}").split()
    if names:
        sh("docker", "rm", "-f", *names, check=False, capture_output=True)


def others(stale=False):
    """Containers that are not ours and look like builds or gates (they contend for the VM's cpu and memory). One that has been up for
    days is a hung leftover, not a running build: waiting for it never ends, so it is listed apart (`stale=True`) and not waited for."""
    mine = out("docker", "ps", "--filter", f"label={LABEL}", "--format", "{{.Names}}").split()
    rows = [l.split("\t") for l in out("docker", "ps", "--format", "{{.Names}}\t{{.Image}}\t{{.Command}}\t{{.RunningFor}}").splitlines()]
    old = lambda up: any(w in up for w in ("day", "week", "month"))
    return [f"{n} {i} {c}" for n, i, c, up in rows if n not in mine and any(k in (i + c) for k in ("ff-rust", "cargo", "mutator")) and old(up) == stale]


def wait_idle():
    """Wait (bounded) for other ff-rust/cargo/mutator containers to finish; returns what was still running."""
    end = time.time() + WAIT
    while others() and time.time() < end:
        time.sleep(20)
    return others()


def load():
    return os.popen("uptime").read().strip().split("load averages:")[-1].strip()


def collect_logs(base, r, label):
    for n, log in [("prov", "ffc-prov")] + [("fd", f"ffc-rtu-{i}") for i in range(1, r + 1)]:
        lg = sh("docker", "logs", log, capture_output=True, check=False)
        with gzip.open(OUT / f"{base.name}.{n}.log.gz", "ab") as f:
            f.write((f"== {log} {label}\n" + lg.stdout + lg.stderr).encode())


def cells_for(rep):
    return ",".join(c for c in CELLS if rep <= SLOW_REPS or c not in SLOW)


def run(rep, r):
    base = OUT / f"R{r}_rep{rep}"
    if (base.parent / f"{base.name}.cmds.csv").exists():
        return print("skip", base.name, flush=True)
    print("run ", base.name, time.strftime("%H:%M:%S"), flush=True)
    cleanup()
    for ext in ("prov", "fd"):
        (OUT / f"{base.name}.{ext}.log.gz").unlink(missing_ok=True)
    busy = wait_idle()
    t0, load0, others0 = time.strftime("%FT%TZ", time.gmtime()), load(), others()
    sh("docker", "network", "create", NET, check=False, capture_output=True)
    # ferrofire: one stack per (rep, R) runs every cell, restoring the grid after each. stock: a fresh stack per cell (a stock provider that falls behind keeps
    # applying a backlog for minutes, which would leak into the next cell; restoring a dirty stack would cost as long as the cell)
    batches = [[c] for c in cells_for(rep).split(",")] if STACK == "stock" else [cells_for(rep).split(",")]
    rel = lambda p: "/src/" + str(pathlib.Path(p).relative_to(ROOT))
    drv_rc, drv_log, parts = 0, "", []
    for i, batch in enumerate(batches):
        if i:
            cleanup()
        scada, stream = STACKS[STACK](r)
        part = f"{base}.part{i}"
        cmd = ["docker", "run", "--rm", "--label", LABEL, "--name", "ffc-drv", "--platform", PLAT, "--network", NET, *MNT, IMG, CTL, "--scada", scada, "--provider", "tcp://provider:5555",
               "--stream", stream, "--cells", ",".join(batch), "--stack", STACK, "--read-per", "1" if STACK == "stock" else "100",
               *(["--legacy", "--status-only", "--fresh"] if STACK == "stock" else []), "--rep", str(rep), "--out", rel(part)]
        drv = sh(*cmd, capture_output=True, check=False, timeout=6 * 3600)
        drv_rc, drv_log = drv_rc or drv.returncode, drv_log + f"== {','.join(batch)} exit {drv.returncode}\n" + drv.stderr + drv.stdout
        parts.append(part)
        if STACK == "stock":
            collect_logs(base, r, ",".join(batch))
    (OUT / f"{base.name}.driver.log").write_text(drv_log)
    if STACK != "stock":
        collect_logs(base, r, "")
    cleanup()
    binary = hashlib.sha256((ROOT / "target-linux/release/ferrofire").read_bytes()).hexdigest()[:16]
    if STACK == "stock":
        binary = f"stock image {out('docker', 'image', 'inspect', STOCK_IMG, '--format', '{{.Id}}')[:23]} (driver ffcontrol, ferrofire {binary})"
    (OUT / f"{base.name}.env.txt").write_text(
        f"stack {STACK} R {r} rep {rep}\nstart {t0} end {time.strftime('%FT%TZ', time.gmtime())}\ndriver_exit {drv_rc}\n"
        f"commit {out('git', '-C', str(ROOT), 'rev-parse', 'HEAD')} dirty {bool(out('git', '-C', str(ROOT), 'status', '--porcelain', '--', 'src', 'bench/control'))}\n"
        f"ferrofire_sha256 {binary}\nhost_load_start {load0}\nhost_load_end {load()}\n"
        f"docker_vm {out('docker', 'info', '--format', 'cpus={{.NCPU}} mem={{.MemTotal}}')}\n"
        f"contended_at_start {bool(others0)}{(' (waited ' + str(WAIT) + ' s, still running: ' + '; '.join(busy) + ')') if busy else ''}\n"
        f"other_containers_start {'; '.join(others0) or 'none'}\nother_containers_end {'; '.join(others()) or 'none'}\nstale_containers_ignored {'; '.join(others(True)) or 'none'}\ncells {cells_for(rep)}\n")
    if drv_rc:
        print("FAILED", base.name, drv_log[-400:], flush=True)
        if STACK != "stock":
            return  # ferrofire: the partial files stay and the run is redone; stock: a cell that cannot run is a result, kept in driver.log
    for ext in ("cmds.csv", "cells.csv"):  # one header, the rows of every batch (a batch that died early has a header and maybe some rows)
        rows = [l for p in parts if os.path.exists(f"{p}.{ext}") for l in open(f"{p}.{ext}").read().splitlines(True)]
        hdr = rows[0] if rows else ""
        (OUT / f"{base.name}.{ext}").write_text(hdr + "".join(l for l in rows if l != hdr))
        for p in parts:
            pathlib.Path(f"{p}.{ext}").unlink(missing_ok=True)


if __name__ == "__main__":
    OUT.mkdir(parents=True, exist_ok=True)
    if not os.environ.get("NOBUILD") and STACK != "stock":
        build()
    grid()
    for rep in range(1, REPS + 1):
        for r in RS:
            run(rep, r)
