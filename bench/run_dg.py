#!/usr/bin/env python3
"""Datagram v1 benchmark matrix (ww-otj.18). Host side, stdlib only; every tool runs in docker (reuses bench/run.py's build + images).

    python3 bench/run_dg.py                 # everything, 3 reps; finished cells are skipped
    SCEN=C REPS=1 python3 bench/run_dg.py   # one scenario family

Scenarios (cell names are `<scenario>_<params>_r<rep>`; raw files in docs/bench/results/raw-datagram-v1/):
  A age      800 tags x {10,100} Hz, multicast alone (compare with the zmq rows of raw/ = before sync publish)
  C netem    tc netem on the provider's eth0 (delay 2ms 1ms, loss 0/0.1/1/5 %): multicast stream and a pull-bridge side by side
  D fanout   8/16/32 RTUs x 50/100 tags: per-RTU multicast groups vs one whole-model stream vs pull-bridges (egress B/s, CPU)
  E restart  subscriber (and the pull-bridge) leaves at t=12 s, rejoins 2 s later: time to the first complete cycle
  F faults   provider killed + restarted mid-run, REP replies blackholed, garbage injected at 100/s, misspelt tags
Summarise with `python3 bench/dg_report.py`. Env: REPS, SCEN, SECS (window seconds, default 30), RAWDIR."""
import json, os, pathlib, subprocess, sys, time

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import run as R  # noqa: E402  (build(), sh(), image/mount constants)

ROOT, SECS, REPS = R.ROOT, os.environ.get("SECS", "30"), int(os.environ.get("REPS", "3"))
SCEN = os.environ.get("SCEN", "A,C,D,E,F").split(",")
RAW = ROOT / os.environ.get("RAWDIR", "docs/bench/results/raw-datagram-v1")
NETEM = "delay 2ms 1ms"
CLIENT = f"ethtool -K eth0 tso off gso off gro off >/dev/null; {R.CLI}"


def all_tags(n):
    """The provider's tag list in model order (bench/gen_model.py): sim.t, the 4 inputs, then bench.tNNNN."""
    return ["sim.t"] + [f"bench.in{i}" for i in range(4)] + [f"bench.t{k:04d}" for k in range(n - 5)]


def rtu_tags(all_, i, k):
    """Mirror of bench/client/src/pull.rs rtu_tags (sim.t plus k-1 round-robin slice of the rest)."""
    rest = [t for t in all_ if t != "sim.t"]
    return ["sim.t"] + [rest[(i * (k - 1) + j) % len(rest)] for j in range(k - 1)]


def docker(*a, **kw):
    return R.sh("docker", *a, **kw)


def clean():
    docker("rm", "-f", "ffb-prov", "ffb-mon", "ffb-inj", "ffb-cl", check=False, capture_output=True)


def start_provider(n, pargs, pid1=True):
    """Provider container on the bench bridge. pid1: ferrofire is PID 1 (the monitor samples its CPU); else it is exec'd
    so it can be killed and restarted without the container (and the shared network namespace) going away."""
    base = ["docker", "run", "-d", "--name", "ffb-prov", "--network", R.NET, "--cap-add", "NET_ADMIN", *R.MNT, R.IMG]
    cmd = [R.FF, "run", f"/src/bench/models/m{n}.yaml", *pargs]
    if pid1:
        R.sh(*base, *cmd, stdout=subprocess.DEVNULL)
    else:
        R.sh(*base, "sleep", "infinity", stdout=subprocess.DEVNULL)
        launch(cmd)
    time.sleep(1.5)
    return cmd


def launch(cmd):
    """(Re)start the provider inside ffb-prov; returns the VM-clock ns before and after the command."""
    return vm(f"(setsid sh -c 'exec {' '.join(cmd)}' >> /tmp/prov.log 2>&1 &)")


def vm(shell):
    """Run `shell` in ffb-prov, bracketed by VM-clock stamps (the same clock the client's timestamps use)."""
    out = docker("exec", "ffb-prov", "sh", "-c", f"date +%s%N; {shell}; date +%s%N", capture_output=True).stdout.split()
    return [int(out[0]), int(out[-1])]


def start_mon(out):
    rel = out.relative_to(ROOT)
    docker("run", "-d", "--name", "ffb-mon", "--network", "container:ffb-prov", "--pid", "container:ffb-prov", "--cap-add", "NET_RAW",
           "--cap-add", "NET_ADMIN", *R.MNT, R.IMG, "bash", "-c",
           f"ethtool -K eth0 tso off gso off gro off >/dev/null; exec {R.CLI} mon --out /src/{rel}.mon.csv", stdout=subprocess.DEVNULL)
    time.sleep(1)


def client_cmd(what):
    return ["docker", "run", "--rm", "--name", "ffb-cl", "--network", R.NET, "--cap-add", "NET_ADMIN", *R.MNT, R.IMG, "bash", "-c", f"{CLIENT} {what}"]


def finish(out, cl, tc=False, execd=False):
    """Collect logs, stop everything, store the client JSON last (its presence marks the cell done)."""
    ext = lambda e: pathlib.Path(f"{out}.{e}")
    if tc:
        ext("tc.txt").write_text(docker("exec", "ffb-prov", "tc", "-s", "qdisc", "show", "dev", "eth0", capture_output=True, check=False).stdout)
    docker("stop", "ffb-mon", stdout=subprocess.DEVNULL, check=False)
    logs = docker("logs", "ffb-prov", capture_output=True, check=False)
    log = docker("exec", "ffb-prov", "cat", "/tmp/prov.log", capture_output=True, check=False).stdout if execd else ""
    ext("prov.log").write_text(log + logs.stdout + logs.stderr)
    docker("stop", "-t", "2", "ffb-prov", stdout=subprocess.DEVNULL, check=False)
    clean()
    if cl.returncode:
        return print("FAILED", out.name, cl.stderr[-600:], flush=True)
    ext("client.json").write_text(cl.stdout)


def cell(name, rep, body):
    out = RAW / f"{name}_r{rep}"
    if pathlib.Path(f"{out}.client.json").exists():
        return print("skip", out.name, flush=True)
    print("run ", out.name, flush=True)
    clean()
    try:
        body(out)
    finally:
        clean()


def simple(n, pargs, what, mon=True, netem=None):
    """Provider (PID 1) + monitor + client to completion."""
    def body(out):
        start_provider(n, pargs)
        if mon:
            start_mon(out)
        if netem:
            docker("exec", "ffb-prov", "tc", "qdisc", "add", "dev", "eth0", "root", "netem", *netem.split(), stdout=subprocess.DEVNULL)
        cl = R.sh(*client_cmd(what), capture_output=True, check=False)
        finish(out, cl, tc=bool(netem))
    return body


# ---- scenarios --------------------------------------------------------------------------------------------------

def scen_a(rep):
    for p in ["0.1", "0.01"]:
        cell(f"A_age_n800_p{p}", rep, simple(800, ["--rate", p], f"dg --whole --period {p} --secs {SECS}"))


def scen_c(rep):
    for n, p in [(100, "0.1"), (100, "0.01"), (800, "0.1")]:
        for loss in ["0", "0.1", "1", "5"]:
            nm = NETEM + (f" loss {loss}%" if loss != "0" else "")
            cell(f"C_netem_n{n}_p{p}_loss{loss}", rep,
                 simple(n, ["--rate", p], f"dg --whole --pull 1 --k {n} --period {p} --secs {SECS}", netem=nm))


def slice_args(rtus, k, base=None):
    a = all_tags(800)
    return sum([["--publish-tags", f"udp://*;239.0.0.1:{41000 + i}=" + ",".join(rtu_tags(a, i, k))] for i in range(rtus)], [])


def scen_d(rep):
    cases = [(r, k, "0.1") for r in (8, 16, 32) for k in (50, 100)] + [(32, k, "0.01") for k in (50, 100)]
    for r, k, p in cases:
        cell(f"D_slices_r{r}_k{k}_p{p}", rep, simple(800, ["--rate", p, *slice_args(r, k)], f"dg --slices {r} --k {k} --period {p} --secs {SECS}"))
        # pull: nobody subscribes to the provider's own stream (once per 10 s on loopback) so only the batched READs load it
        cell(f"D_pull_r{r}_k{k}_p{p}", rep, simple(800, ["--rate", "10", "--publish", "udp://127.0.0.1:40000"], f"dg --pull {r} --k {k} --period {p} --secs {SECS}"))
    for p in ["0.1", "0.01"]:
        cell(f"D_whole_n800_p{p}", rep, simple(800, ["--rate", p], f"dg --whole --period {p} --secs {SECS}"))


def scen_e(rep):
    for n, p in [(100, "0.1"), (100, "0.01"), (800, "0.1")]:
        cell(f"E_restart_n{n}_p{p}", rep, simple(n, ["--rate", p], f"dg --whole --pull 1 --k {n} --period {p} --secs {SECS} --restart-at 12 --restart-gap 2 --events", mon=False))


def timed(what, actions, pargs, n=100, mon=False):
    """Client in the background (events on); `actions` = [(seconds after the client's window start, label, fn(cmd) -> VM-clock stamps)]."""
    def body(out):
        cmd = start_provider(n, pargs, pid1=False)
        t0 = time.time()
        cl = subprocess.Popen(client_cmd(what + " --events"), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        events = {}
        for at, label, act in actions:
            time.sleep(max(0, t0 + 4.5 + at - time.time()))  # ~1.5 s docker start-up + 3 s warm-up before the client's window
            events[label] = act(cmd)
        so, se = cl.communicate()
        pathlib.Path(f"{out}.events.json").write_text(json.dumps(events))
        finish(out, subprocess.CompletedProcess(cl.args, cl.returncode, so, se), execd=True)
    return body


BLACKHOLE = ("tc qdisc add dev eth0 root handle 1: prio bands 3 priomap 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 && "
             "tc qdisc add dev eth0 parent 1:3 handle 30: netem loss 100% && "
             "tc filter add dev eth0 protocol ip parent 1: prio 1 u32 match ip sport 5555 0xffff flowid 1:3")


def scen_f(rep):
    p, w = "0.1", "dg --whole --pull 1 --k 100 --period 0.1 --secs 40"
    cell("F_kill_n100_p0.1", rep, timed(w, [(12, "kill", lambda c: vm("pkill -9 -x ferrofire")), (15, "restart", launch)], ["--rate", p]))
    cell("F_repdrop_n100_p0.1", rep, timed(w, [(12, "blackhole_on", lambda c: vm(BLACKHOLE)), (16, "blackhole_off", lambda c: vm("tc qdisc del dev eth0 root"))], ["--rate", p]))
    for inj in (0, 1):
        def body(out, inj=inj):
            start_provider(100, ["--rate", p])
            docker("run", "-d", "--name", "ffb-inj", "--network", R.NET, *R.MNT, R.IMG, "bash", "-c",
                   f"sleep 5; {R.CLI} inject --rate 100 --secs {int(SECS) - 4 if inj else 0} > /src/{(out.relative_to(ROOT))}.inject.json", stdout=subprocess.DEVNULL)
            finish(out, R.sh(*client_cmd(f"dg --whole --pull 1 --k 100 --period 0.1 --secs {SECS}"), capture_output=True, check=False))
        cell(f"F_inject{inj}_n100_p0.1", rep, body)


def scen_misspell():
    """Deterministic: one run, no reps. A misspelt tag in a WRITE/READ gets ERR; in a publish list or pull-bridge list the process refuses to start."""
    out = RAW / "F_misspell"
    if pathlib.Path(f"{out}.txt").exists():
        return print("skip F_misspell", flush=True)
    print("run  F_misspell", flush=True)
    clean()
    res = []
    run_in = lambda *c: R.sh("docker", "run", "--rm", "--network", R.NET, *R.MNT, R.IMG, *c, capture_output=True, check=False)
    start_provider(100, ["--rate", "0.1"])
    cl = run_in(R.CLI, "req", "--host", "ffb-prov", "WRITE=bench.in0x:1", "WRITE=bench.in0:nan", "READ=bench.t0001x", "READ=bench.t0001", "WRITE=bench.in0:7", "READ=bench.in0")
    res.append(("runtime requests (provider up)", cl.returncode, cl.stdout + cl.stderr))
    bridge = run_in(R.FF, "pull-bridge", "--server", "tcp://ffb-prov:5555", "--publish", "udp://127.0.0.1:42000", "--tags", "sim.t,bench.t0001,bench.t0002x", "--rate", "100")
    res.append(("pull-bridge --tags sim.t,bench.t0001,bench.t0002x", bridge.returncode, bridge.stdout + bridge.stderr))
    clean()
    pub = run_in(R.FF, "run", "/src/bench/models/m100.yaml", "--publish-tags", "udp://*;239.0.0.1:41000=sim.t,bench.t0001x")
    res.append(("run --publish-tags ...=sim.t,bench.t0001x", pub.returncode, pub.stdout + pub.stderr))
    pathlib.Path(f"{out}.txt").write_text("".join(f"## {what}\nexit {rc}\n{text.strip()}\n\n" for what, rc, text in res))


def preflight():
    """The python tag-slice mirror must equal the provider's QUERY order, or the slices cells measure nothing."""
    clean()
    start_provider(800, ["--rate", "0.1"])
    q = R.sh("docker", "run", "--rm", "--network", R.NET, *R.MNT, R.IMG, R.CLI, "req", "--host", "ffb-prov", "QUERY=", capture_output=True).stdout
    clean()
    assert q.split("=> ACK=")[1].strip().rstrip("\x00").rstrip(",").split(",") == all_tags(800), "tag order differs from gen_model.py"


if __name__ == "__main__":
    RAW.mkdir(parents=True, exist_ok=True)
    R.build()
    docker("network", "create", R.NET, check=False, capture_output=True)
    preflight()
    for rep in range(1, REPS + 1):  # reps outermost so slow drift hits every cell alike
        for s in SCEN:
            {"A": scen_a, "C": scen_c, "D": scen_d, "E": scen_e, "F": scen_f}[s](rep)
    if "F" in SCEN:
        scen_misspell()
