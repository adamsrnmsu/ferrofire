#!/usr/bin/env python3
"""stock_provider.py ARGS... : the stock pybennu-power-solver (PyPower) for the control bench's stock baseline (ww-1df.23.4), unchanged except that
PyPower.write logs one `Applied: t_apply_ns=N t_solved_ns=N tag=T val=V` line per tag (unix ns; `t_apply_ns` when the write handler is entered,
`t_solved_ns` when the re-solve has finished, just before the ACK), the shape bench/control_join.py joins. ARGS are those of pybennu-power-solver,
e.g. `start -c /etc/sceptre/config.ini`. Publish interval (ww-1df.35): pybennu 6.0.0 hard-codes `time.sleep(1)` in PyPower.periodic_publish (no config option), so
the env var STOCK_PUBLISH_S (default 1, i.e. stock as shipped) replaces that method by a verbatim copy whose only change is the sleep (ww-1df.47: v2026.11.03's copy logs the publish error with loguru's `logger.error` instead of print; the patch uses `logger.error` when pybennu imports loguru, else print, so one script drives both images, STOCK_IMAGE in run_control.py); the loop also prints a
`Publish-stats:` line every 10 s (publishes, mean / max pack and send ms, process CPU s) so a provider that cannot keep its interval shows in the log. runpf(OUT_ALL=1) prints the whole power-flow report on every solve (stock behaviour, a few hundred KB at 1250
branches): it is still formatted and written, but to /dev/null, so the container log keeps only the `Applied:` lines and whatever else stock prints to stderr. ww-1df.47: the same script drives pybennu 6.0.0 and v2026.11.03 (whose publish loop logs the error with loguru instead of print; only that line differs, see log_error). Needs the bennu image (bench/Dockerfile.stock or bench/stock_image.sh); stock has no write-stamping of its own."""
import os, sys, time
from pybennu.providers.power.solvers.python_power import PyPower
try:
    from loguru import logger  # pybennu >= v2026.11.03 logs with loguru
    log_error = lambda msg, e: logger.error(msg + ": {}", e)
except ImportError:  # 6.0.0 prints
    log_error = lambda msg, e: print(f"{msg}: {e}")
from pybennu.providers.power.power_daemon import server

_write = PyPower.write
_out = os.fdopen(os.dup(1), "w")
os.dup2(os.open(os.devnull, os.O_WRONLY), 1)  # pypower binds sys.stdout at import: redirect the fd


def write(self, tags):
    t0 = time.time_ns()
    reply = _write(self, tags)
    t1 = time.time_ns()
    if reply.startswith("ACK"):
        for tag, val in tags.items():
            print(f"Applied: t_apply_ns={t0} t_solved_ns={t1} tag={tag.strip(chr(0))} val={val}", file=_out, flush=True)
    return reply


PyPower.write = write

PERIOD = float(os.environ.get("STOCK_PUBLISH_S", "1"))
if PERIOD != 1.0 or os.environ.get("STOCK_PUBLISH_STATS"):
    def periodic_publish(self):  # pybennu 6.0.0 / v2026.11.03 PyPower.periodic_publish, `time.sleep(1)` -> PERIOD, plus the stats line
        n, pack, send, nxt = 0, [0.0, 0.0], [0.0, 0.0], time.time() + 10
        while True:
            a = time.perf_counter()
            msg = self._pack_data()
            b = time.perf_counter()
            try:
                self.publish(msg)
            except Exception as e:
                log_error("provider periodic_publish error", e)
            c = time.perf_counter()
            n += 1
            pack[0] += b - a; pack[1] = max(pack[1], b - a); send[0] += c - b; send[1] = max(send[1], c - b)
            if time.time() >= nxt:
                print(f"Publish-stats: t_ns={time.time_ns()} period_s={PERIOD} publishes={n} pack_ms_mean={pack[0] / n * 1e3:.2f} pack_ms_max={pack[1] * 1e3:.2f} "
                      f"send_ms_mean={send[0] / n * 1e3:.2f} send_ms_max={send[1] * 1e3:.2f} cpu_s={time.process_time():.3f} up_s={time.monotonic():.1f}", file=_out, flush=True)
                n, pack, send, nxt = 0, [0.0, 0.0], [0.0, 0.0], nxt + 10
            time.sleep(PERIOD)
    PyPower.periodic_publish = periodic_publish
sys.exit(server())
