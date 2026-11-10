#!/usr/bin/env python3
"""analyze.py trk CSV | fault CSV : summaries for the live runs (trk.py and fault.py outputs)."""
import sys
mode, path = sys.argv[1], sys.argv[2]
rows = [l.strip() for l in open(path) if l.strip()]
if mode == "trk":
    d = [r.split(",") for r in rows[1:]]; n = len(d); bad = sum(1 for r in d if 'x' in r)
    f = lambda v: float(v)
    ok = [r for r in d if 'x' not in r]
    eb = [abs(f(r[1]) - f(r[2])) for r in ok]; el = [abs(f(r[3]) - f(r[4])) for r in ok]; e2 = [abs(f(r[5]) - f(r[4])) for r in ok]
    pct = lambda e, tol: 100.0 * sum(1 for x in e if x <= tol) / max(1, len(e))
    mx = lambda e: max(e) if e else float('nan')
    print("samples %d (%d with a failed read), span %.0f s" % (n, bad, float(d[-1][0]) if d else 0))
    print("rtu-1 boat.location vs provider READ: within 6 m (1 s of travel at 5 m/s) %.1f%%, max error %.1f m" % (pct(eb, 6), mx(eb)))
    print("rtu-1 lock.height vs provider: within 0.7 m (1 s of fill at 0.5 m/s) %.1f%%, max error %.2f m" % (pct(el, 0.7), mx(el)))
    print("rtu-2 lock.height vs provider: within 0.7 m %.1f%%, max error %.2f m" % (pct(e2, 0.7), mx(e2)))
    bs = [f(r[2]) for r in ok]; ls = [f(r[4]) for r in ok]
    print("process moved: boat.location %.0f..%.0f m, lock.height %.1f..%.1f m" % (min(bs), max(bs), min(ls), max(ls)))
else:
    ev = [(float(r.split()[1]), " ".join(r.split()[2:])) for r in rows if r.startswith("#")]
    d = [(r.split(",")) for r in rows if not r.startswith("#")]
    d = [(float(r[0]), r[1], r[2], r[3]) for r in d if len(r) == 4]
    # event times are unix; sample t is seconds since the first '# start' event
    base = ev[0][0]
    for t, m in ev: print("event +%.1fs %s" % (t - base, m[:150]))
    kill = [t - base for t, m in ev if m.startswith(("kill", "SIGSTOP"))][0]
    back = [t - base for t, m in ev if m.startswith(("provider start", "SIGCONT"))][0]
    # restart model: boat at 5 m/s from 0 at `back` (+ a few hundred ms to bind); stock RTU scan is 1000 ms
    pre = [r for r in d if r[0] < kill and r[1] != 'x']
    last = float(pre[-1][1]) if pre else 0
    fails = [(r[0]) for r in d if kill <= r[0] < back + 60 and 'x' in r[1:]]
    print("boat reg just before the fault: %.1f m; samples with an unanswered Modbus read after the fault: %d" % (last, len(fails)))
    t_chg = next((r[0] for r in d if r[0] > back and r[1] != 'x' and abs(float(r[1]) - last) > 15 and float(r[1]) < last), None)
    t_trk = next((r[0] for r in d if r[0] > back and r[1] != 'x' and float(r[1]) < 40 and abs(float(r[1]) - 5.0 * (r[0] - back)) <= 8), None)
    if t_chg is not None: print("rtu-1 first shows the restarted model (boat reg dropped >15 m): %.1f s after the restart command" % (t_chg - back))
    if t_trk is not None: print("rtu-1 within 8 m of the restarted model's boat position: %.1f s after the restart command" % (t_trk - back))
    t2 = next((r[0] for r in d if r[0] > back and r[3] != 'x' and float(r[3]) < 5.5 and r[0] > back + 0.2), None)
    print("rtu-2 lock.height back at the restarted value (<5.5 m): %s" % ("%.1f s after the restart command" % (t2 - back) if t2 is not None else "n/a (see csv)"))
    if "SIGCONT" in str(ev):
        print("(SIGSTOP mode: the model does not restart; tracking is judged from the csv, boat reg should keep rising after SIGCONT)")
