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
STOCK_PUBLISH_S (STACK=stock, default 1 = as shipped): stock publish interval, s (ww-1df.35; stock hard-codes 1 s, bench/stock_provider.py patches it); use OUTDIR=docs/bench/results/control/stock-<s>s.
PROVIDER=ferrofire|stock and FD=ferrofire|stock (ww-1df.40, the 2x2 cross matrix): pick the provider and the field device independently (either set = cross mode, STACK ignored, the other defaults to ferrofire);
the stack is named `cross-<provider>-<fd>`, results default to docs/bench/results/control/cross-<provider>-<fd>[-<s>s] (suffix only for PROVIDER=stock with a tuned STOCK_PUBLISH_S).
A mixed pairing gets copies of the field device's config whose stream endpoint is the provider's (ferrofire publishes one group per RTU, stock one group for everything); driver flags follow the provider
(stock: --read-per 1 --legacy --status-only), a fresh stack per cell when either side is stock. Without these env vars the behaviour is unchanged.
STACK=stock: pybennu + stock bennu field devices (image ffc-stock:22.04 from bench/Dockerfile.stock; bench/stock_provider.py), same driver and cells, raw in .../stock/.
Joining the provider stamps to the commands: bench/control_join.py."""
import gzip, hashlib, os, pathlib, subprocess, sys, time

ROOT = pathlib.Path(__file__).resolve().parent.parent
STACK = os.environ.get("STACK", "ferrofire")
CROSS = "PROVIDER" in os.environ or "FD" in os.environ
PROV, FDK = (os.environ.get("PROVIDER", "ferrofire"), os.environ.get("FD", "ferrofire")) if CROSS else (STACK, STACK)
assert PROV in ("ferrofire", "stock") and FDK in ("ferrofire", "stock"), (PROV, FDK)
FRESH = "stock" in (PROV, FDK)   # a fresh stack per cell (see run())
if CROSS:
    STACK = f"cross-{PROV}-{FDK}"
PUB = os.environ.get("STOCK_PUBLISH_S", "1")   # stock publish interval, s (bench/stock_provider.py; ww-1df.35)
ENV = lambda k, d: os.environ.get(k, d).split(",")
RS, REPS = [int(r) for r in ENV("RS", "1,2,5,10")], int(os.environ.get("REPS", "5"))
BREAKERS, GENS = [1, 5, 10, 50, 100, 1000], [1, 2, 5, 10]
CELLS = ENV("CELLS", ",".join([f"breaker:{m}:{n}" for n in BREAKERS for m in ("seq", "sim")] + [f"gen:{m}:{n}" for n in GENS for m in ("seq", "sim")]))
SLOW, SLOW_REPS = [c for c in ENV("SLOW", "") if c], int(os.environ.get("SLOW_REPS", str(REPS)))
WAIT = int(os.environ.get("WAIT", "600"))
OUT = ROOT / os.environ.get("OUTDIR", f"docs/bench/results/control/{STACK}" + (f"-{PUB}s" if CROSS and PROV == "stock" and PUB != "1" else ""))  # OUTDIR: a scratch dir for smoke runs, relative to the repo
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


STOCK_IMG = "ffc-stock:22.04"
STREAM = {"ferrofire": "udp://*;239.0.1.{n}:40000", "stock": "udp://*;239.0.0.1:40000"}   # the provider's stream endpoint per RTU n


def start_provider(prov, r):
    d = f"/src/target-linux/control-grid/R{r}"
    if prov == "ferrofire":
        docker_run("ffc-prov", "provider", FF, "run", f"{d}/model.yaml", "--stamp-writes")
    else:  # pybennu-power-solver (PyPower; bench/stock_provider.py = stock plus `Applied:` stamps); stock publishes the whole model, once a second, to the one endpoint of config.ini
        docker_run("ffc-prov", "provider", "python3", "/src/bench/stock_provider.py", "start", "-c", f"{d}/config.ini", img=STOCK_IMG,
                   extra=["-v", f"{GRID}/case.py:/etc/sceptre/case_control.py:ro", "-e", f"STOCK_PUBLISH_S={PUB}", "-e", "STOCK_PUBLISH_STATS=1"])


def start_fd(fd, prov, r):
    """R field devices of kind fd subscribed to prov's stream. Native pairing: the generated config as is; mixed: a copy whose stream endpoint is prov's (gen_grid writes
    each kind's own endpoint; that is the only difference between ferrofire/rtu-N.xml and bennu/rtu-N.xml)."""
    d, kind = f"/src/target-linux/control-grid/R{r}", "ferrofire" if fd == "ferrofire" else "bennu"
    for n in range(1, r + 1):
        xml = f"{d}/{kind}/rtu-{n}.xml"
        if fd != prov:
            src, dst = GRID / f"R{r}/{kind}/rtu-{n}.xml", GRID / f"R{r}/cross-{fd}-{prov}/rtu-{n}.xml"
            old, new = STREAM[fd].replace("{n}", str(n)), STREAM[prov].replace("{n}", str(n))
            text = src.read_text()
            assert text.count(old) == 1, (src, old)
            dst.parent.mkdir(exist_ok=True)
            dst.write_text(text.replace(old, new))
            xml = f"{d}/cross-{fd}-{prov}/rtu-{n}.xml"
        if fd == "ferrofire":
            docker_run(f"ffc-rtu-{n}", f"rtu-{n}", FF, "field-device", "--file", xml)
        else:
            docker_run(f"ffc-rtu-{n}", f"rtu-{n}", "bash", "-c", f"bennu-field-deviced --command start --file {xml} && exec tail --pid=$(pgrep -o bennu-field-d) -f /dev/null", img=STOCK_IMG)  # it daemonizes


def start_stack(r):
    start_provider(PROV, r)
    start_fd(FDK, PROV, r)
    return f"/src/target-linux/control-grid/R{r}/scada.json", STREAM[PROV]


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
    batches = [[c] for c in cells_for(rep).split(",")] if FRESH else [cells_for(rep).split(",")]
    rel = lambda p: "/src/" + str(pathlib.Path(p).relative_to(ROOT))
    drv_rc, drv_log, parts = 0, "", []
    for i, batch in enumerate(batches):
        if i:
            cleanup()
        scada, stream = start_stack(r)
        part = f"{base}.part{i}"
        cmd = ["docker", "run", "--rm", "--label", LABEL, "--name", "ffc-drv", "--platform", PLAT, "--network", NET, *MNT, IMG, CTL, "--scada", scada, "--provider", "tcp://provider:5555",
               "--stream", stream, "--cells", ",".join(batch), "--stack", STACK, "--read-per", "1" if PROV == "stock" else "100",
               *(["--legacy", "--status-only"] if PROV == "stock" else []), *(["--fresh"] if FRESH else []), "--rep", str(rep), "--out", rel(part)]
        drv = sh(*cmd, capture_output=True, check=False, timeout=6 * 3600)
        drv_rc, drv_log = drv_rc or drv.returncode, drv_log + f"== {','.join(batch)} exit {drv.returncode}\n" + drv.stderr + drv.stdout
        parts.append(part)
        if FRESH:
            collect_logs(base, r, ",".join(batch))
    (OUT / f"{base.name}.driver.log").write_text(drv_log)
    if not FRESH:
        collect_logs(base, r, "")
    cleanup()
    binary = hashlib.sha256((ROOT / "target-linux/release/ferrofire").read_bytes()).hexdigest()[:16]
    if "stock" in (PROV, FDK):
        binary = f"stock image {out('docker', 'image', 'inspect', STOCK_IMG, '--format', '{{.Id}}')[:23]} (driver ffcontrol, ferrofire {binary})"
    pub_line = f"stock_publish_s {PUB}\n" if PROV == "stock" else ""
    (OUT / f"{base.name}.env.txt").write_text(
        f"stack {STACK} provider {PROV} fd {FDK} R {r} rep {rep}\nstart {t0} end {time.strftime('%FT%TZ', time.gmtime())}\ndriver_exit {drv_rc}\n"
        f"commit {out('git', '-C', str(ROOT), 'rev-parse', 'HEAD')} dirty {bool(out('git', '-C', str(ROOT), 'status', '--porcelain', '--', 'src', 'bench/control'))}\n"
        f"ferrofire_sha256 {binary}\n{pub_line}host_load_start {load0}\nhost_load_end {load()}\n"
        f"docker_vm {out('docker', 'info', '--format', 'cpus={{.NCPU}} mem={{.MemTotal}}')}\n"
        f"contended_at_start {bool(others0)}{(' (waited ' + str(WAIT) + ' s, still running: ' + '; '.join(busy) + ')') if busy else ''}\n"
        f"other_containers_start {'; '.join(others0) or 'none'}\nother_containers_end {'; '.join(others()) or 'none'}\nstale_containers_ignored {'; '.join(others(True)) or 'none'}\ncells {cells_for(rep)}\n")
    if drv_rc:
        print("FAILED", base.name, drv_log[-400:], flush=True)
        if not FRESH:
            return  # ferrofire: the partial files stay and the run is redone; stock: a cell that cannot run is a result, kept in driver.log
    for ext in ("cmds.csv", "cells.csv"):  # one header, the rows of every batch (a batch that died early has a header and maybe some rows)
        rows = [l for p in parts if os.path.exists(f"{p}.{ext}") for l in open(f"{p}.{ext}").read().splitlines(True)]
        hdr = rows[0] if rows else ""
        (OUT / f"{base.name}.{ext}").write_text(hdr + "".join(l for l in rows if l != hdr))
        for p in parts:
            pathlib.Path(f"{p}.{ext}").unlink(missing_ok=True)


if __name__ == "__main__":
    OUT.mkdir(parents=True, exist_ok=True)
    if not os.environ.get("NOBUILD") and "ferrofire" in (PROV, FDK):
        build()
    grid()
    for rep in range(1, REPS + 1):
        for r in RS:
            run(rep, r)
