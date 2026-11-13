# Control-latency bench on the live range (ww-1df.23.5)

Raw data of `bench/range/run_range.py` (see bench/README.md, "Control-latency on the live range"): `<stack>/R<R>_rep<k>.*` as in the docker runs, plus `summary-<stack>.md`
(`bench/control_summary.py`: per cell and R over the 3 reps; visible = `t_visible - t_cmd`, applied = provider `t_apply_ns - t_cmd`).

- Range: phenix experiment `ctlbench` (topology `control-bench-arm64` of phenix-waterway-range), 6 arm64 KVM VMs on one minimega host (16 vCPU, 80 GB): provider + SCADA driver VM (4 vCPU, 3 GB), rtu-1..5 (2 vCPU, 1 GB). RTUs above R idle.
  Boot in two batches of three VMs; all 6 cc clients confirmed before measuring. Stack runs 2026-11-12 14:52 to 18:14 MST, one run after the other.
- Matrix: R in {1,2,5} x breaker {1,10,100} and generators {1,5}, sequential and simultaneous, 3 reps, ferrofire and stock bennu (same image: stock = pybennu PyPower + `bennu-field-deviced`, ferrofire = `ferrofire run` + `field-device`).
  Ferrofire: one stack per (rep, R); stock: a fresh stack per cell. Stock gens are status flips only (no ramp), as in the docker runs.
- Contention: every run's `env.txt` says `contended_at_start False` (no other VM or started phenix experiment; the host load average at run start was 4.5 at most). One earlier phenix experiment (`kp-offline`, one VM) was stopped before the first run.
- Clock: the driver and the provider run on the same VM, so every stamp is on one clock.
