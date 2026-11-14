# Stock bennu baseline at a 0.05 s publish interval (ww-1df.35)

Raw data of `STOCK_PUBLISH_S=0.05 STACK=stock REPS=5 SLOW=breaker:seq:1000,breaker:sim:1000 SLOW_REPS=1 python3 bench/run_control.py`, same matrix, grid and driver as [../stock/](../stock/README.md) (1 s as shipped), R = 1, 2, 5, 10, 5 reps, a fresh stack per cell.
**Deviation from stock as shipped**: the provider's publish loop sleeps 0.05 s instead of 1 s (pybennu 6.0.0 hard-codes `time.sleep(1)`; `bench/stock_provider.py` replaces `PyPower.periodic_publish` by a copy that differs only in the sleep, see bench/README.md).
The `.prov.log.gz` files also carry one `Publish-stats:` line per 10 s (pack and send ms, publishes, provider CPU seconds). `summary.md` is `python3 bench/control_summary.py docs/bench/results/control/stock-0.05s`. Report: docs/bench/control-latency.md section 8.
