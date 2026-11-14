# Stock bennu baseline at a 0.1 s publish interval (ww-1df.35)

Raw data of `STOCK_PUBLISH_S=0.1 STACK=stock REPS=5 SLOW=breaker:seq:1000,breaker:sim:1000 SLOW_REPS=1 python3 bench/run_control.py`, same matrix, grid and driver as [../stock/](../stock/README.md) (1 s as shipped), R = 1, 2, 5, 10, 5 reps, a fresh stack per cell.
**Deviation from stock as shipped**: the provider's publish loop sleeps 0.1 s instead of 1 s (pybennu 6.0.0 hard-codes `time.sleep(1)`; `bench/stock_provider.py` replaces `PyPower.periodic_publish` by a copy that differs only in the sleep, see bench/README.md).
The `.prov.log.gz` files also carry one `Publish-stats:` line per 10 s (pack and send ms, publishes, provider CPU seconds). `summary.md` is `python3 bench/control_summary.py docs/bench/results/control/stock-0.1s`. Report: docs/bench/control-latency.md section 8.

R10_rep2 was run twice: the first attempt ended with `gen:seq:5` exit 2 ("baseline never reached: every breaker closed, every generator on at its setpoint", right after the stack started); the whole (R = 10, rep 2) run was redone and only the redo is kept here.
