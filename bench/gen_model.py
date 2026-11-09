#!/usr/bin/env python3
"""gen_model.py N -> a ferrofire model with exactly N published output tags (stdout).

sim.t (state, integrates 1 per second), 4 writable inputs echoed as outputs (bench.in0..3), and N-5 analog
outputs `bench.tNNNN = sim.t * 0.1 + NNNN * 0.37`, cheap on purpose so the engine is never the bottleneck.
The long decimals are deliberate: they put realistic f64 text on the wire."""
import sys

n = int(sys.argv[1])
assert n >= 5
print("version: 1\ndt: 0.01\ninputs:")
for i in range(4):
    print(f"  bench.in{i}: {{type: analog, default: 0}}")
print("states:\n  sim.t: {ode: \"1\", init: 0}\noutputs:\n  sim.t: {type: analog}")
for i in range(4):
    print(f"  bench.in{i}: {{type: analog}}")
for k in range(n - 5):
    print(f"  bench.t{k:04d}: {{type: analog, expr: \"sim.t * 0.1 + {k} * 0.37\"}}")
