#!/usr/bin/env python3
"""Run the field-device microbench (ww-1df.41): ferrofire field-device vs stock bennu-field-deviced, one scripted provider (bench/fdmicro harness), no real provider.
Host side, stdlib only; every process runs in a container on the DEFAULT docker daemon (Docker Desktop). Per (rep, profile, protocol) the two stacks run one after
the other, the order alternating with the rep; per run: a fresh device container (alias `rtu`) and the harness container (alias `provider`, sharing the
device's PID namespace so it can read the device's cpu, RSS and UDP counters from /proc). Own label ffmicro=1 and network ffmicro: only the label is used to find or remove containers.
Raw output in docs/bench/results/fdmicro/: <stack>_<P>_<proto>_rep<k>.{metrics.csv,s2r.csv,wf.csv,env.txt,fd.log.gz}. Finished runs are skipped (a run resumes).
Env: REPS (3), PROFILES (S,M,L), PROTOS (mb,dnp3), STACKS (ferrofire,stock), OUTDIR, SECS (5, window and read-phase length), FLIPS (100), RATES (S: 1,10,100),
NOBUILD=1, WAIT (seconds to wait for other cargo containers before a run, 600).
"""
import gzip, hashlib, os, pathlib, subprocess, sys, time

ROOT = pathlib.Path(__file__).resolve().parents[2]
ENV = lambda k, d: os.environ.get(k, d).split(",")
REPS, PROFILES, PROTOS, STACKS = int(os.environ.get("REPS", "3")), ENV("PROFILES", "S,M,L"), ENV("PROTOS", "mb,dnp3"), ENV("STACKS", "ferrofire,stock")
SECS, FLIPS, WAIT = os.environ.get("SECS", "5"), os.environ.get("FLIPS", "100"), int(os.environ.get("WAIT", "600"))
OUT = ROOT / os.environ.get("OUTDIR", "docs/bench/results/fdmicro")
CFG = ROOT / "target-linux/fdmicro/cfg"
LABEL, NET, PLAT, IMG, STOCK = "ffmicro=1", "ffmicro", "linux/arm64", "ff-rust:22.04", "ffc-stock:22.04"
MNT = ["-v", f"{ROOT}:/src"]
HARNESS = "/src/target-linux/fdmicro-build/release/fdmicro"
FF = "/src/target-linux/release/ferrofire"


def sh(*a, check=True, **kw):
    return subprocess.run(a, check=check, text=True, **kw)


def out(*a):
    return sh(*a, capture_output=True, check=False).stdout.strip()


def build():
    """cargo on the default daemon, sized for the 7.7 GB VM: 2 jobs, no incremental."""
    sh("docker", "run", "--rm", "--platform", PLAT, *MNT, "-w", "/src/bench/fdmicro", "-e", "CARGO_BUILD_JOBS=2", "-e", "CARGO_INCREMENTAL=0", "-e", "CARGO_TARGET_DIR=/src/target-linux/fdmicro-build", IMG, "cargo", "build", "--release")


def cleanup():
    names = out("docker", "ps", "-a", "--filter", f"label={LABEL}", "--format", "{{.Names}}").split()
    if names:
        sh("docker", "rm", "-f", *names, check=False, capture_output=True)


def others(stale=False):
    """Containers that are not ours and look like builds or gates (they contend for the VM's cpu and memory); days-old ones are hung leftovers, listed apart."""
    mine = out("docker", "ps", "--filter", f"label={LABEL}", "--format", "{{.Names}}").split()
    rows = [l.split("\t") for l in out("docker", "ps", "--format", "{{.Names}}\t{{.Image}}\t{{.Command}}\t{{.RunningFor}}").splitlines()]
    old = lambda up: any(w in up for w in ("day", "week", "month"))
    return [f"{n} {i} {c}" for n, i, c, up in rows if n not in mine and any(k in (i + c) for k in ("ff-rust", "cargo", "mutator")) and old(up) == stale]


def wait_idle():
    end = time.time() + WAIT
    while others() and time.time() < end:
        time.sleep(20)
    return others()


def start_fd(stack, cfg):
    common = ["docker", "run", "-d", "--label", LABEL, "--name", "ffm-fd", "--platform", PLAT, "--network", NET, "--network-alias", "rtu", *MNT]
    if stack == "ferrofire":
        sh(*common, IMG, FF, "field-device", "--file", cfg, stdout=subprocess.DEVNULL)
    else:  # the stock daemon forks; the container lives as long as it does
        sh(*common, STOCK, "bash", "-c", f"bennu-field-deviced --command start --file {cfg} && exec tail --pid=$(pgrep -o bennu-field-d) -f /dev/null", stdout=subprocess.DEVNULL)


def run(rep, prof, proto, stack):
    base = OUT / f"{stack}_{prof}_{proto}_rep{rep}"
    if pathlib.Path(f"{base}.metrics.csv").exists():
        return print("skip", base.name, flush=True)
    print("run ", base.name, time.strftime("%H:%M:%S"), flush=True)
    cleanup()
    busy = wait_idle()
    t0, others0, load0 = time.strftime("%FT%TZ", time.gmtime()), others(), os.popen("uptime").read().split("load averages:")[-1].strip()
    sh("docker", "network", "create", NET, check=False, capture_output=True)
    cfg = f"/src/target-linux/fdmicro/cfg/{prof}-{proto}.xml"
    start_fd(stack, cfg)
    time.sleep(1)
    rel = lambda p: "/src/" + str(pathlib.Path(p).relative_to(ROOT))
    rates = os.environ.get("RATES", "1,10,100") if prof == "S" else ""
    drv = sh("docker", "run", "--rm", "--label", LABEL, "--name", "ffm-drv", "--platform", PLAT, "--network", NET, "--network-alias", "provider", "--pid", "container:ffm-fd", *MNT, IMG,
             HARNESS, "--layout", f"/src/target-linux/fdmicro/cfg/{prof}.json", "--proto", proto, "--stack", stack, "--fd-host", "rtu", "--out", rel(base), "--secs", SECS, "--flips", FLIPS,
             "--rates", rates, capture_output=True, check=False, timeout=3600)
    fdlog = out("docker", "logs", "ffm-fd")
    stats = out("docker", "stats", "--no-stream", "--format", "{{.CPUPerc}} {{.MemUsage}}", "ffm-fd")
    cleanup()
    (OUT / f"{base.name}.driver.log").write_text(drv.stderr + drv.stdout)
    with gzip.open(f"{base}.fd.log.gz", "wt") as f:
        f.write(fdlog)
    binary = hashlib.sha256((ROOT / "target-linux/release/ferrofire").read_bytes()).hexdigest()[:16]
    cfgsum = hashlib.sha256((CFG / f"{prof}-{proto}.xml").read_bytes()).hexdigest()[:16]
    (OUT / f"{base.name}.env.txt").write_text(
        f"stack {stack} profile {prof} proto {proto} rep {rep}\nstart {t0} end {time.strftime('%FT%TZ', time.gmtime())}\nharness_exit {drv.returncode}\n"
        f"commit {out('git', '-C', str(ROOT), 'rev-parse', 'HEAD')} dirty {bool(out('git', '-C', str(ROOT), 'status', '--porcelain', '--', 'src', 'bench/fdmicro'))}\n"
        f"ferrofire_sha256 {binary}\nstock_image {out('docker', 'image', 'inspect', STOCK, '--format', '{{.Id}}')[:23]}\nconfig_sha256 {cfgsum}\n"
        f"host_load_start {load0}\ndocker_vm {out('docker', 'info', '--format', 'cpus={{.NCPU}} mem={{.MemTotal}}')}\n"
        f"contended_at_start {bool(others0)}{(' (waited ' + str(WAIT) + ' s, still running: ' + '; '.join(busy) + ')') if busy else ''}\n"
        f"other_containers_start {'; '.join(others0) or 'none'}\nother_containers_end {'; '.join(others()) or 'none'}\nstale_containers_ignored {'; '.join(others(True)) or 'none'}\n")
    if drv.returncode:
        print("FAILED", base.name, (drv.stderr + drv.stdout)[-600:], flush=True)
        pathlib.Path(f"{base}.metrics.csv").unlink(missing_ok=True)   # not a result: the run is redone
        (OUT / f"{base.name}.failed.txt").write_text(drv.stderr + drv.stdout + "\n" + fdlog[-2000:])


if __name__ == "__main__":
    OUT.mkdir(parents=True, exist_ok=True)
    if not os.environ.get("NOBUILD"):
        build()
    sh("python3", str(ROOT / "bench/fdmicro/gen.py"), str(CFG))
    for rep in range(1, REPS + 1):
        for prof in PROFILES:
            for proto in PROTOS:
                for stack in (STACKS if rep % 2 else STACKS[::-1]):   # alternate who goes first
                    run(rep, prof, proto, stack)
