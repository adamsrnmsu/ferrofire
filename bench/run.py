#!/usr/bin/env python3
"""Run the zmq-vs-nng benchmark matrix (host side, stdlib only; every tool runs in docker).

Per run: provider container (ferrofire run, PID 1) and a monitor sharing its network+PID namespaces (packet counters,
provider CPU/RSS) on a dedicated bridge network; the measuring client in a third container on the same bridge.
Raw output: docs/bench/results/raw/<transport>_n<N>_p<period>_r<rep>.{client.json,mon.csv,prov.log}. Finished runs are skipped.
Env: NS, PERIODS, REPS, PUB_SECS, REQ_SECS, TRANSPORTS, RAWDIR.

MODE=pull|push (ww-otj.11): N RTUs x K tags each out of 800 instead of one client; NS="8x50,8x100" (RTUs x tags), model m800, raw
files `<mode>_<transport>_n<R>x<K>_p<period>_r<rep>`, default RAWDIR docs/bench/results/raw-pull. `ffbench pull` runs the RTUs
(pull: one real `ferrofire pull-bridge` process per RTU; push: one SUB0 per RTU on the nng provider). The provider publishes once
per 10 s on loopback in pull mode, so its work is the batched READs alone. Summarise with `python3 bench/pull_report.py`."""
import os, subprocess, sys, time, pathlib

ROOT = pathlib.Path(__file__).resolve().parent.parent
MODE = os.environ.get("MODE", "push1")  # push1 = the original single-client matrix
RAW = ROOT / os.environ.get("RAWDIR", "docs/bench/results/raw" if MODE == "push1" else "docs/bench/results/raw-pull")  # RAWDIR=docs/bench/results/raw-after-nng-fix for the post-fix runs
ENV = lambda k, d: os.environ.get(k, d).split(",")
NS, PERIODS, TRANSPORTS = ENV("NS", "10,50,100,800" if MODE == "push1" else "8x50,8x100"), ENV("PERIODS", "0.1,0.01"), ENV("TRANSPORTS", "zmq,nng")
REPS, PUB, REQ = int(ENV("REPS", "3")[0]), ENV("PUB_SECS", "30")[0], ENV("REQ_SECS", "10")[0]
IMG, NET = "ffbench:22.04", "ffbench"
FF, CLI = "/src/target-linux/release/ferrofire", "/src/target-linux/bench/release/ffbench"
MNT = ["-v", f"{ROOT}:/src"]


def sh(*a, check=True, **kw):
    return subprocess.run(a, check=check, text=True, **kw)


def build():
    sh("docker", "build", "-q", "-t", IMG, "-f", str(ROOT / "bench/Dockerfile"), str(ROOT / "bench"), stdout=subprocess.DEVNULL)
    sh("docker", "volume", "create", "ffbench-cargo", stdout=subprocess.DEVNULL)
    run = ["docker", "run", "--rm", *MNT, "-w", "/src", "-v", "ffbench-cargo:/root/.cargo/registry", "-e", "CARGO_TARGET_DIR=/src/target-linux", IMG]
    sh(*run, "bash", "-c", "cargo build --release && cd bench/client && CARGO_TARGET_DIR=/src/target-linux/bench cargo build --release")
    for n in ["800"] if MODE != "push1" else NS:
        (ROOT / f"bench/models/m{n}.yaml").write_text(sh("python3", str(ROOT / "bench/gen_model.py"), n, capture_output=True).stdout)


def cell(tr, n, p, rep):
    multi = MODE != "push1"
    rtus, k = n.split("x") if multi else (n, n)
    tag = f"{MODE}_{tr}_n{n}_p{p}_r{rep}" if multi else f"{tr}_n{n}_p{p}_r{rep}"
    out = RAW / tag
    ext = lambda e: pathlib.Path(f"{out}.{e}")  # not with_suffix: the period has a dot in it
    if ext("client.json").exists():
        return print("skip", tag, flush=True)
    print("run ", tag, flush=True)
    sh("docker", "rm", "-f", "ffb-prov", "ffb-mon", check=False, capture_output=True)
    pargs = ["--transport", tr, "--rate", p]
    if MODE == "pull":  # nobody subscribes: publish once per 10 s on loopback so only the batched READs load the provider
        pargs = ["--transport", tr, "--rate", "10", "--publish", "udp://127.0.0.1:40000" if tr == "zmq" else "tcp://0.0.0.0:5556"]
    sh("docker", "run", "-d", "--name", "ffb-prov", "--network", NET, *MNT, IMG, FF, "run", f"/src/bench/models/m{800 if multi else n}.yaml",
       *pargs, stdout=subprocess.DEVNULL)
    time.sleep(1.5)
    rel = out.relative_to(ROOT)
    # offloads off in both namespaces so a captured frame is a real <=1514-byte wire frame, not a GSO super-packet
    sh("docker", "run", "-d", "--name", "ffb-mon", "--network", "container:ffb-prov", "--pid", "container:ffb-prov", "--cap-add", "NET_RAW",
       "--cap-add", "NET_ADMIN", *MNT, IMG, "bash", "-c",
       f"ethtool -K eth0 tso off gso off gro off >/dev/null; exec {CLI} mon --out /src/{rel}.mon.csv", stdout=subprocess.DEVNULL)
    time.sleep(1)
    what = (f"pull --mode {MODE} --transport {tr} --host ffb-prov --rtus {rtus} --k {k} --period {p} --pub-secs {PUB} --ff {FF}" if multi
            else f"client --transport {tr} --host ffb-prov --n {n} --period {p} --pub-secs {PUB} --req-secs {REQ}")
    cl = sh("docker", "run", "--rm", "--network", NET, "--cap-add", "NET_ADMIN", *MNT, IMG, "bash", "-c",
            f"ethtool -K eth0 tso off gso off gro off >/dev/null; {CLI} {what}", capture_output=True, check=False)
    sh("docker", "stop", "ffb-mon", stdout=subprocess.DEVNULL, check=False)
    log = sh("docker", "logs", "ffb-prov", capture_output=True, check=False)
    ext("prov.log").write_text(log.stdout + log.stderr)
    sh("docker", "stop", "ffb-prov", stdout=subprocess.DEVNULL, check=False)
    sh("docker", "rm", "-f", "ffb-prov", "ffb-mon", check=False, capture_output=True)
    if cl.returncode:
        print("FAILED", tag, cl.stderr[-400:], flush=True)
        return
    ext("client.json").write_text(cl.stdout)


if __name__ == "__main__":
    RAW.mkdir(parents=True, exist_ok=True)
    build()
    sh("docker", "network", "create", NET, check=False, capture_output=True)
    for rep in range(1, REPS + 1):  # reps outermost so slow drift hits every cell alike
        for p in PERIODS:
            for n in NS:
                for tr in TRANSPORTS:
                    cell(tr, n, p, rep)
