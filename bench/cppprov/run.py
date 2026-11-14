#!/usr/bin/env python3
"""Run the stub-model comparison (ww-1df.42): Sandia's C++ bennu-simulink-provider (unmodified, with bench/cppprov/stub_model.c as the "Simulink model") vs ferrofire
(with the equally trivial model.yaml of bench/cppprov/gen.py), through the same field devices and the same ffcontrol SCADA driver as the control bench. PLUMBING ONLY: no solver.
Host side, stdlib only; everything runs in containers on the DEFAULT docker daemon, own containers (label ffcpp=1) on the network ffcpp.

Env: PAIRS (cpp:stock,ferrofire:stock,ferrofire:ferrofire,cpp:ferrofire; provider:field-device), RS (10; RTUs), NS (100,1250 points), PUBS (0.1,0.05 s publish interval), REPS (3),
CELLS (default per N, below), IDLE_S (20: idle window for the achieved publish rate and cpu), OUTDIR (docs/bench/results/cppprov).
Per (rep, pair, N, pub): an idle stack measures the achieved publish rate (rate.py on the stream group) and provider/field-device cpu and RSS over IDLE_S seconds; then a FRESH stack
per cell (like the stock runs of the control bench) runs one ffcontrol cell, and the provider / field-device cpu ticks and RSS are read at the end of it.
Raw: OUTDIR/<provider>-<fd>/N<n>_P<pub>_rep<k>.{env.txt,<cell>.cmds.csv,<cell>.cells.csv,<cell>.prov.log.gz}. Finished runs are skipped."""
import gzip, hashlib, json, os, pathlib, subprocess, sys, time

ROOT = pathlib.Path(__file__).resolve().parent.parent.parent
ENV = lambda k, d: os.environ.get(k, d).split(",")
PAIRS, NS, PUBS = ENV("PAIRS", "cpp:stock,ferrofire:stock,ferrofire:ferrofire,cpp:ferrofire"), [int(n) for n in ENV("NS", "100,1250")], ENV("PUBS", "0.1,0.05")
RS = [int(r) for r in ENV("RS", "10")]
REPS, IDLE_S = int(os.environ.get("REPS", "3")), int(os.environ.get("IDLE_S", "20"))
OUT = ROOT / os.environ.get("OUTDIR", "docs/bench/results/cppprov")
CELLS = {100: "breaker:seq:10,breaker:seq:50,breaker:sim:10,breaker:sim:100", 1250: "breaker:seq:10,breaker:seq:50,breaker:sim:10,breaker:sim:100,breaker:sim:1000"}
CELLS = {n: os.environ["CELLS"] if "CELLS" in os.environ else CELLS.get(n, CELLS[1250]) for n in NS}
CFG, LABEL, NET, PLAT = ROOT / "target-linux/cppprov/cfg", "ffcpp=1", "ffcpp", "linux/arm64"
FF_IMG, STOCK_IMG = "ff-rust:22.04", "ffc-stock:22.04"
FF, CTL, STUB = "/src/target-linux/release/ferrofire", "/src/target-linux/ctl/release/ffcontrol", "/src/target-linux/cppprov/stub_model"
GROUP = {"cpp": "239.0.0.1", "ferrofire": "239.0.1.{n}"}   # the provider's stream group per RTU n (cpp: one for all, like stock pybennu; ferrofire: one per RTU slice, gen_grid.py)
MNT = ["-v", f"{ROOT}:/src"]
PROCS = r'''for d in /proc/[0-9]*; do c=$(cat $d/comm 2>/dev/null); case $c in stub_model|bennu-simulink-|bennu-field-d*|ferrofire) echo "$c $(cut -d' ' -f14,15 $d/stat) $(grep -E 'VmRSS|VmHWM' $d/status | tr -s ' ' | cut -d' ' -f2 | tr '\n' ' ')";; esac; done'''


def sh(*a, check=True, **kw):
    return subprocess.run(a, check=check, text=True, **kw)


def out(*a):
    return sh(*a, capture_output=True, check=False).stdout.strip()


def run_c(name, alias, img, *cmd, extra=()):
    sh("docker", "run", "-d", "--label", LABEL, "--name", name, "--platform", PLAT, "--network", NET, "--network-alias", alias, *MNT, *extra, img, *cmd, stdout=subprocess.DEVNULL)


def cleanup():
    names = out("docker", "ps", "-a", "--filter", f"label={LABEL}", "--format", "{{.Names}}").split()
    if names:
        sh("docker", "rm", "-f", *names, check=False, capture_output=True)


def start(prov, fd, n, r, pub):
    """Provider container `provider` and R field-device containers `rtu-1..R` (names ffc-rtu-k). Returns the stream endpoint pattern ({n} = RTU)."""
    c = f"/src/target-linux/cppprov/cfg/N{n}_R{r}"
    if prov == "cpp":   # stub first (creates the semaphores, FIFO, shm), then the unmodified provider (it blocks on publish_sem until the stub is initialised)
        run_c("ffc-prov", "provider", STOCK_IMG, "bash", "-c", f"{STUB} {n} 0.05 > /proc/1/fd/1 2>&1 & sleep 1; exec bennu-simulink-provider --server-endpoint tcp://0.0.0.0:5555 "
              f"--publish-endpoint 'udp://*;{GROUP[prov]}:40000' --publish-rate {pub}")
    else:
        (CFG / f"N{n}_R{r}/model-{pub}.yaml").write_text((CFG / f"N{n}_R{r}/model.yaml").read_text().replace("__RATE__", pub))
        run_c("ffc-prov", "provider", FF_IMG, FF, "run", f"{c}/model-{pub}.yaml", "--stamp-writes")
    for k in range(1, r + 1):
        xml = f"{c}/rtu-{k}-{GROUP[prov].format(n=k)}.xml"
        if fd == "ferrofire":
            run_c(f"ffc-rtu-{k}", f"rtu-{k}", FF_IMG, FF, "field-device", "--file", xml)
        else:
            run_c(f"ffc-rtu-{k}", f"rtu-{k}", STOCK_IMG, "bash", "-c", f"bennu-field-deviced --command start --file {xml} && exec tail --pid=$(pgrep -o bennu-field-d) -f /dev/null")
    return f"udp://*;{GROUP[prov]}:40000"


def procs(name):
    """[(comm, cpu ticks (100/s), peak rss kB, rss kB): /proc status lists VmHWM before VmRSS] of the bench processes in container `name`."""
    rows = []
    for l in out("docker", "exec", name, "sh", "-c", PROCS).splitlines():
        p = l.split()
        if len(p) >= 5:
            rows.append((p[0], int(p[1]) + int(p[2]), int(p[3]), int(p[4])))
    return rows


def pfmt(rows, label):
    return "".join(f"{label} {c} ticks={t} hwm_kb={r} rss_kb={h}\n" for c, t, r, h in rows)


def fd_procs(r):
    """The field-device containers' processes, one list per RTU."""
    return [row for k in range(1, r + 1) for row in procs(f"ffc-rtu-{k}")]


def idle(prov, fd, n, r, pub):
    """Achieved publish rate and idle cpu: a stack with nothing driving it for IDLE_S seconds (the rate is that of RTU 1's group)."""
    start(prov, fd, n, r, pub)
    time.sleep(8)
    a = (procs("ffc-prov"), fd_procs(r))
    rate = sh("docker", "run", "--rm", "--label", LABEL, "--platform", PLAT, "--network", NET, *MNT, STOCK_IMG, "python3", "/src/bench/cppprov/rate.py", GROUP[prov].format(n=1), "40000", str(IDLE_S), pub,
              capture_output=True, check=False)
    b = (procs("ffc-prov"), fd_procs(r))
    txt = f"idle_rate {rate.stdout.strip() or rate.stderr.strip()[-200:]}\nidle_window_s {IDLE_S}\n" + pfmt(a[0], "idle_prov_start") + pfmt(b[0], "idle_prov_end") + pfmt(a[1], "idle_fd_start") + pfmt(b[1], "idle_fd_end")
    cleanup()
    return txt


def cell(prov, fd, n, r, pub, rep, c, base):
    start(prov, fd, n, r, pub)
    scada = f"/src/target-linux/cppprov/cfg/N{n}_R{r}/scada.json"
    rel = "/src/" + str(base.relative_to(ROOT))
    cmd = ["docker", "run", "--rm", "--label", LABEL, "--name", "ffc-drv", "--platform", PLAT, "--network", NET, *MNT, FF_IMG, CTL, "--scada", scada, "--provider", "tcp://provider:5555",
           "--stream", f"udp://*;{GROUP[prov]}:40000", "--cells", c, "--stack", f"{prov}-{fd}", "--read-per", "100" if prov == "ferrofire" else "1", "--fresh", "--rep", str(rep),
           *(["--legacy", "--status-only"] if prov == "cpp" else []), "--out", rel]
    drv = sh(*cmd, capture_output=True, check=False, timeout=3600)
    txt = f"== {c} exit {drv.returncode}\n" + drv.stderr[-600:] + pfmt(procs("ffc-prov"), f"cell_prov {c}") + pfmt(fd_procs(r), f"cell_fd {c}")
    for k, x in enumerate(["ffc-prov"] + [f"ffc-rtu-{k}" for k in range(1, r + 1)]):
        lg = sh("docker", "logs", x, capture_output=True, check=False)
        with gzip.open(f"{base}.{'prov' if k == 0 else 'fd'}.log.gz", "wt" if k < 2 else "at") as f:
            f.write(f"== {x}\n" + lg.stdout + lg.stderr)
    cleanup()
    return drv.returncode, txt


def sums():
    h = lambda p: hashlib.sha256((ROOT / p).read_bytes()).hexdigest()[:16]
    prov = out("docker", "run", "--rm", "--platform", PLAT, STOCK_IMG, "sha256sum", "/usr/local/bin/bennu-simulink-provider")[:16]
    return f"ferrofire_sha256 {h('target-linux/release/ferrofire')}\nffcontrol_sha256 {h('target-linux/ctl/release/ffcontrol')}\nstub_model_sha256 {h('target-linux/cppprov/stub_model')}\nbennu_simulink_provider_sha256 {prov} (image {STOCK_IMG})\n"


def main():
    SUMS = sums()
    for r in RS:
        sh("python3", str(ROOT / "bench/cppprov/gen.py"), str(CFG), str(r), *map(str, NS), stdout=subprocess.DEVNULL)
    sh("docker", "network", "create", NET, check=False, capture_output=True)
    for rep in range(1, REPS + 1):
        for pair in PAIRS:
            prov, fd = pair.split(":")
            d = OUT / f"{prov}-{fd}"
            d.mkdir(parents=True, exist_ok=True)
            for r in RS:
                for n in NS:
                    for pub in PUBS:
                        base = d / f"N{n}_R{r}_P{pub}_rep{rep}"
                        if pathlib.Path(f"{base}.env.txt").exists():
                            print("skip", base.name, pair, flush=True)
                            continue
                        print("run ", pair, base.name, time.strftime("%H:%M:%S"), flush=True)
                        cleanup()
                        load = lambda: os.popen("uptime").read().strip().split("load averages:")[-1].strip()
                        env = f"pair {pair} N {n} R {r} publish_s {pub} rep {rep}\nstart {time.strftime('%FT%TZ', time.gmtime())}\nhost_load_start {load()}\ncommit {out('git', '-C', str(ROOT), 'rev-parse', 'HEAD')}\n" \
                              f"{SUMS}docker_vm {out('docker', 'info', '--format', 'cpus={{.NCPU}} mem={{.MemTotal}}')}\nother_containers {'; '.join(out('docker', 'ps', '--format', '{{.Names}} {{.Image}}').splitlines()) or 'none'}\n"
                        env += idle(prov, fd, n, r, pub)
                        for c in CELLS[n].split(","):
                            rc, txt = cell(prov, fd, n, r, pub, rep, c, pathlib.Path(f"{base}.{c.replace(':', '-')}"))
                            env += txt
                        pathlib.Path(f"{base}.env.txt").write_text(env + f"end {time.strftime('%FT%TZ', time.gmtime())}\nhost_load_end {load()}\n")


if __name__ == "__main__":
    main()
