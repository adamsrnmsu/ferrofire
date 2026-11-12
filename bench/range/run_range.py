#!/usr/bin/env python3
"""Live-range subset of the control-latency bench (ww-1df.23.5, epic ww-1df.23). Host side, stdlib only. The range is the phenix experiment of
phenix-waterway-range topologies/control-bench-arm64 (provider VM that also runs the SCADA driver, so every timestamp is on one clock; rtu-1..5), driven
through minimega `cc` (no host route to the guests). Same driver (ffcontrol), grid, cells and raw layout as bench/run_control.py (docker), so
bench/control_join.py / control_summary.py / control_check.py read the output unchanged: docs/bench/results/control/range/<stack>/R<R>_rep<k>.*

Subcommands: `up` (create + start the experiment in two boot batches of 3 VMs, wait for every cc client, ship the bundle), `run` (the matrix), `down`
(stop + delete the experiment, check minimega is empty), `status`. Env: EXP (ctlbench), NS (= EXP), STACKS (ferrofire,stock), RS (1,2,5), REPS (3), CELLS (breaker:{seq,sim}:{1,10,100},gen:{seq,sim}:{1,5}).
Ferrofire: one stack per (rep, R) runs every cell; stock: a fresh stack per cell (as run_control.py). Each run's env.txt records host load, other VMs and guest load."""
import base64, gzip, hashlib, io, os, pathlib, re, shlex, subprocess, sys, tarfile, time

ROOT = pathlib.Path(__file__).resolve().parent.parent.parent
RANGE = pathlib.Path(os.environ.get("RANGE_REPO", os.path.expanduser("~/git/phenix-waterway-range")))
EXP = os.environ.get("EXP", "ctlbench")
NS = os.environ.get("NS", EXP)
ENV = lambda k, d: os.environ.get(k, d).split(",")
STACKS, RS, REPS = ENV("STACKS", "ferrofire,stock"), [int(r) for r in ENV("RS", "1,2,5")], int(os.environ.get("REPS", "3"))
CELLS = ENV("CELLS", ",".join(f"breaker:{m}:{n}" for n in (1, 10, 100) for m in ("seq", "sim")) + "," + ",".join(f"gen:{m}:{n}" for n in (1, 5) for m in ("seq", "sim")))
NRTU = 5
ALL = ["provider"] + [f"rtu-{n}" for n in range(1, NRTU + 1)]
BATCH1, BATCH2 = ["provider", "rtu-1", "rtu-2"], ["rtu-3", "rtu-4", "rtu-5"]
OUT = ROOT / os.environ.get("OUTDIR", "docs/bench/results/control/range")  # OUTDIR: a scratch dir for smoke runs, relative to the repo
GRID = ROOT / "target-linux/control-grid-range"
DOCKER = {"DOCKER_HOST": "unix:///Users/ryanadams/.lima/phenix/sock/docker.sock"}
NOISE = re.compile(r"^\d{4}/\d\d/\d\d .*(INFO|WARN)|^\S* ?(INF|WRN) |^$")


def host(*a, t=120, check=True):
    r = subprocess.run(a, capture_output=True, text=True, timeout=t, env={**os.environ, **DOCKER})
    if check and r.returncode:
        raise RuntimeError(f"{a}: {r.stderr[-400:]}")
    return "\n".join(l for l in (r.stdout + r.stderr).splitlines() if not NOISE.search(l))


def mm(*a, t=60):
    return host("docker", "exec", "minimega", "mm", *a, t=t)


def px(*a, t=300, check=True):
    return host("docker", "exec", "phenix", "phenix", *a, t=t, check=check)


# --- cc plumbing: every command runs as base64-wrapped bash on one VM (or all); the output is read back with `cc responses ID raw` ---
def cc(hosts, script, wait=60):
    """Run SCRIPT on each of HOSTS (list; None = every VM) and return {host: stdout}. Ends when each answered or after WAIT s (then TIMEOUT)."""
    b = base64.b64encode(script.encode()).decode()
    ids = {}
    for h in hosts or ALL:
        mm("namespace", NS, "cc", "filter", f"hostname={h}")
        mm("namespace", NS, "cc", "exec", "bash", "-c", f"echo {b} | base64 -d | bash")
        cmds = mm("namespace", NS, "cc", "commands")
        ids[h] = max(int(m) for m in re.findall(r"^(?:\S+\s*\|\s*)?(\d+)\s*\|", cmds, re.M))
    out, end = {}, time.time() + wait
    while len(out) < len(ids) and time.time() < end:
        time.sleep(1)
        for h, i in ids.items():
            if h not in out:
                r = mm("namespace", NS, "cc", "responses", str(i), "raw")
                if r.strip() and not r.startswith("Error"):
                    out[h] = re.sub(r"^\S+: ", "", r, count=1)  # `raw` prefixes the minimega host name
    return {h: out.get(h, "TIMEOUT") for h in ids}


def vm(h, *args, wait=60):
    return cc([h], "/root/cb/vm.sh " + " ".join(map(str, args)), wait)[h].strip()


def fetch(h, path, wait=60):
    """A guest file (or, starting with a space, a command) as bytes (gzip + base64 through cc stdout)."""
    r = cc([h], f"{path} | gzip -c | base64 -w0" if path.startswith(" ") else f"gzip -c {path} | base64 -w0", wait)[h].strip()
    return gzip.decompress(base64.b64decode(r)) if r and r != "TIMEOUT" else b""


def clients():
    return [l for l in mm("namespace", NS, "cc", "clients").splitlines() if "arm64" in l or "linux" in l.lower()]


# --- bundle ---
def bundle():
    subprocess.run(["python3", str(ROOT / "bench/gen_grid.py"), str(GRID), "--rtus", ",".join(map(str, RS))], check=True, stdout=subprocess.DEVNULL)
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w:gz") as t:
        for src, dst in [("target-linux/release/ferrofire", "ferrofire"), ("target-linux/ctl/release/ffcontrol", "ffcontrol"), ("bench/stock_provider.py", "stock_provider.py"),
                         ("bench/range/vm.sh", "vm.sh")]:
            t.add(ROOT / src, dst)
        t.add(GRID, "grid")
    return buf.getvalue()


def up():
    px("exp", "create", EXP, "-t", "control-bench-arm64", check=False)
    px("exp", "start", EXP, t=900)
    t0 = time.time()
    # batch 1 = the VMs that boot with the experiment; batch 2 (do_not_boot) is restarted once batch 1 has registered with cc (ww-344.6 / ww-344.9)
    wait_clients(len(BATCH1), 600)
    launch(BATCH2)
    wait_clients(len(BATCH1) + len(BATCH2), 600)
    print("all cc clients after", int(time.time() - t0), "s")
    ship()


def launch(names):
    """Launch VMs phenix left out (do_not_boot) exactly as the experiment's own mm script does for the others (experiments/EXP/mm_files/EXP.mm): phenix has no
    command that boots a do_not_boot VM later (`vm restart` / `vm redeploy` cannot find it in minimega), so this replays that script's stanza by hand."""
    for h in names:
        d = "/phenix/experiments/%s/startup/%s-" % (EXP, h)
        snap = f"lima_{EXP}_{h}_snapshot"
        M = lambda *a: mm("namespace", NS, *a)
        M("disk", "snapshot", "bennu-ferrofire-arm64.qc2", snap)
        M("disk", "inject", f"{snap}:2", "files", f"{d}hostname.sh:/etc/phenix/startup/1_hostname-start.sh", f"{d}timezone.sh:/etc/phenix/startup/2_timezone-start.sh",
          f"{d}interfaces.sh:/etc/phenix/startup/3_interfaces-start.sh")
        for c in ["clear vm config", "vm config vcpus 2", "vm config cpu host", "vm config memory 1024", "vm config snapshot true", f"vm config disk {snap},virtio,writeback",
                  "vm config qemu-append -vga qxl", "vm config net phenix,ICS", "vm config machine virt", "vm config qemu-append -bios /usr/share/AAVMF/AAVMF_CODE.fd",
                  "vm config vga virtio", 'vm config qemu-override "bus=usb-bus.0" "bus=xhci.0"', f"vm launch kvm {h}"]:
            M(*shlex.split(c))
    M("vm", "launch")
    M("vm", "start", ",".join(names))


def wait_clients(n, secs):
    end = time.time() + secs
    while time.time() < end:
        c = clients()
        if len(c) >= n:
            return c
        time.sleep(10)
    sys.exit(f"only {len(clients())} of {n} cc clients after {secs} s")


def ship():
    """Bundle -> minimega files dir -> every VM (`cc send` never overwrites a file the guest already has, so each ship has its own name) -> /root/cb."""
    name = f"cb-{int(time.time())}.tgz"
    pathlib.Path("/tmp/cb.tgz").write_bytes(bundle())
    host("docker", "exec", "minimega", "mkdir", "-p", "/phenix/images/cbdeploy")
    host("docker", "cp", "/tmp/cb.tgz", f"minimega:/phenix/images/cbdeploy/{name}")
    mm("namespace", NS, "clear", "cc", "filter")
    mm("namespace", NS, "cc", "send", f"cbdeploy/{name}")
    time.sleep(15)
    r = cc(None, f"mkdir -p /root/cb && tar xzf /tmp/miniccc/files/cbdeploy/{name} -C /root/cb && /root/cb/vm.sh setup && md5sum /root/cb/vm.sh", 90)
    print(r)
    assert len({v.split()[-2] for v in r.values()}) == 1 and "TIMEOUT" not in str(r), "bundle not on every VM"


def down():
    px("exp", "stop", EXP, t=300, check=False)
    px("exp", "delete", EXP, t=300, check=False)
    print(mm("namespace", NS, "vm", "info") or "namespace vm info: empty")


# --- runs ---
def sha(p):
    return hashlib.sha256(pathlib.Path(p).read_bytes()).hexdigest()[:16]


def load(h):
    return vm(h, "x") if False else cc([h], "cat /proc/loadavg | cut -d' ' -f1-3", 30)[h].strip()


def others():
    """Contention at the start of a run: VMs on the minimega host beyond ours (every namespace) and other phenix experiments that are started."""
    extra = int(mm("host").splitlines()[-1].split("|")[7]) - len(ALL)
    exps = [l.split("|")[1].strip() for l in px("exp", "list", check=False).splitlines() if l.startswith("|") and "STARTED" not in l and l.split("|")[4].strip() and l.split("|")[1].strip() != EXP]
    return ([f"{extra} other VM(s) in minimega"] if extra else []) + [f"experiment {e} started" for e in exps]


def stack_up(stack, r):
    hs = ["provider"] + [f"rtu-{n}" for n in range(1, r + 1)]
    cc(None, "/root/cb/vm.sh stop", 60)
    cc(["provider"], f"/root/cb/vm.sh prov {stack} {r}", 30)
    for n in range(1, r + 1):
        cc([f"rtu-{n}"], f"/root/cb/vm.sh fd {stack} {r} {n}", 30)
    ok = [vm("provider", "wait", 5555, 60)] + [vm(f"rtu-{n}", "wait", 502, 90) for n in range(1, r + 1)]
    if any(x != "up" for x in ok):
        raise RuntimeError(f"stack not up: {ok}")
    time.sleep(5)  # field devices subscribe and fetch their first state


def drive(stack, r, rep, base, cells):
    vm("provider", "drv", stack, r, rep, base, cells)
    end = time.time() + 6 * 3600
    while time.time() < end:
        time.sleep(10)
        d = cc(["provider"], f"cat /root/cb/out/{base}.done 2>/dev/null; echo", 30)["provider"].strip()
        if d and d != "TIMEOUT":
            return int(d)
    return -1


def run(stack, rep, r):
    base = OUT / stack / f"R{r}_rep{rep}"
    if (base.parent / f"{base.name}.cmds.csv").exists():
        return print("skip", base.name, flush=True)
    print("run ", stack, base.name, time.strftime("%H:%M:%S"), flush=True)
    base.parent.mkdir(parents=True, exist_ok=True)
    t0, mm0, oth = time.strftime("%FT%TZ", time.gmtime()), mm("host").splitlines()[-1], others()
    gl0 = {h: load(h) for h in ["provider"] + [f"rtu-{n}" for n in range(1, r + 1)]}
    batches = [[c] for c in CELLS] if stack == "stock" else [CELLS]
    rc, dlog, parts, provlog, fdlog = 0, "", [], b"", b""
    for i, b in enumerate(batches):
        stack_up(stack, r)
        pb = f"{base.name}.part{i}"
        rc2 = drive(stack, r, rep, pb, ",".join(b))
        rc, dlog = rc or rc2, dlog + f"== {','.join(b)} exit {rc2}\n" + fetch("provider", f"/root/cb/out/{pb}.driver.log").decode(errors="replace")
        for ext in ("cmds.csv", "cells.csv"):
            (base.parent / f"{pb}.{ext}").write_bytes(fetch("provider", f"/root/cb/out/{pb}.{ext}"))
        parts.append(base.parent / pb)
        provlog += f"== provider {','.join(b)}\n".encode() + fetch("provider", "/root/cb/out/prov.log")
        for n in range(1, r + 1):
            fdlog += f"== rtu-{n} {','.join(b)}\n".encode() + fetch(f"rtu-{n}", " /root/cb/vm.sh fdlog")
    cc(None, "/root/cb/vm.sh stop", 60)
    (base.parent / f"{base.name}.driver.log").write_text(dlog)
    for n, d in (("prov", provlog), ("fd", fdlog)):
        (base.parent / f"{base.name}.{n}.log.gz").write_bytes(gzip.compress(d))
    for ext in ("cmds.csv", "cells.csv"):
        rows = [l for p in parts if (p.parent / f"{p.name}.{ext}").exists() for l in (p.parent / f"{p.name}.{ext}").read_text().splitlines(True)]
        hdr = rows[0] if rows else ""
        (base.parent / f"{base.name}.{ext}").write_text(hdr + "".join(l for l in rows if l != hdr))
        for p in parts:
            (p.parent / f"{p.name}.{ext}").unlink(missing_ok=True)
    gl1 = {h: load(h) for h in gl0}
    (base.parent / f"{base.name}.env.txt").write_text(
        f"stack {stack} R {r} rep {rep} (live range, experiment {EXP}, VMs on one minimega host; driver on the provider VM)\nstart {t0} end {time.strftime('%FT%TZ', time.gmtime())}\ndriver_exit {rc}\n"
        f"commit {subprocess.run(['git', '-C', str(ROOT), 'rev-parse', 'HEAD'], capture_output=True, text=True).stdout.strip()}\n"
        f"ferrofire_sha256 {sha(ROOT / 'target-linux/release/ferrofire')} ffcontrol_sha256 {sha(ROOT / 'target-linux/ctl/release/ffcontrol')}\n"
        f"image bennu-ferrofire-arm64.qc2 (stock pybennu + bennu-field-deviced and ferrofire; the stock stack runs only the stock binaries)\n"
        f"host_load_start {mm0}\nhost_load_end {mm('host').splitlines()[-1]}\nguest_load_start {gl0}\nguest_load_end {gl1}\n"
        f"contended_at_start {bool(oth)} {'; '.join(oth) or 'no other VM or started experiment'}\ncells {','.join(CELLS)}\n")
    if rc:
        print("FAILED", base.name, dlog[-400:], flush=True)


if __name__ == "__main__":
    cmd = sys.argv[1] if len(sys.argv) > 1 else "run"
    if cmd == "up":
        up()
    elif cmd == "down":
        down()
    elif cmd == "status":
        print(clients(), others())
    else:
        for rep in range(1, REPS + 1):  # reps outermost so drift hits every cell alike
            for stack in STACKS:
                for r in RS:
                    run(stack, rep, r)
