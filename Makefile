# Everything that needs cargo runs in the ff-rust container (docker/Dockerfile); the Mac needs only docker and uv.
IMAGE := ff-rust:22.04
PLATFORM ?= linux/arm64
IN := docker run --rm --platform $(PLATFORM) -v $(CURDIR):/src -w /src -e CARGO_TARGET_DIR=/src/target-linux $(IMAGE)
# ub-mode thresholds: CRAP < 10 per function (worst allowed 9.99), dryer similarity < 0.8.
CRAP_MAX := 9.99
DRY_MIN := 0.8

.PHONY: help wireshark bench bench-report bench-dg bench-dg-report image fmt fmt-check lint test golden linux crap dry mutate check clean
help:    # list targets
	@grep -E '^[a-z-]+:.*#' $(MAKEFILE_LIST) | sed -E 's/^([a-z-]+):[^#]*#/\1\t/'
image:   # build the ff-rust container if missing (rebuild: docker build -t ff-rust:22.04 docker)
	@docker image inspect $(IMAGE) >/dev/null 2>&1 || docker build --platform $(PLATFORM) -t $(IMAGE) docker
fmt: image   # cargo fmt
	$(IN) cargo fmt
fmt-check: image   # cargo fmt --check
	$(IN) cargo fmt --check
lint: image   # cargo clippy --all-targets -D warnings
	$(IN) cargo clippy --all-targets -- -D warnings
golden:  # PYPOWER 5.1.19 reference results -> target/golden (needs uv; generated, never committed)
	uv run --python 3.11 --with PYPOWER==5.1.19 --with 'numpy<2' --with 'scipy<1.14' python -I -W ignore tests/golden/gen_pypower.py target/golden
test: golden image   # unit + integration tests in the container (no cargo needed on the host)
	$(IN) cargo test
wireshark:   # decode the datagram v1 test vector with tshark and the Lua dissector (host tool, not in check)
	tools/wireshark/test.sh
linux:   # glibc aarch64 binaries -> dist/ferrofire, dist/simulinksolver
	./build-linux.sh
crap: golden image   # crapper: CRAP score per function, fails above $(CRAP_MAX); writes .metrics/crap.edn
	$(IN) crapper --threshold $(CRAP_MAX) src
dry: image   # dryer: duplicate candidates scoring >= $(DRY_MIN), fails if any (dryer itself always exits 0)
	$(IN) bash -c 'dryer --threshold $(DRY_MIN) src | tee /tmp/dry.out; ! grep -q ^DUPLICATE /tmp/dry.out'
mutate: golden image   # mutator: mutation testing (slow, not part of check); survivors exit 3
	$(IN) mutator src
bench: image   # historical zmq vs nng benchmark matrix (NNG dropped 2026-11-09; needs the old nng build, ~1.2 h) -> docs/bench/results/raw; then: make bench-report
	python3 bench/run.py
bench-report:   # tables, charts and docs/bench/transport-comparison.md from the raw results (needs uv)
	uv run --with matplotlib python bench/report.py
bench-dg: image   # datagram v1 matrix (age, netem loss, per-RTU fan-out, restarts, faults; about 1.5 h) -> docs/bench/results/raw-datagram-v1
	python3 bench/run_dg.py
bench-dg-report:   # docs/bench/datagram-v1-bench.md from the datagram v1 raw results
	python3 bench/dg_report.py
check: fmt-check lint test crap dry   # every gate except mutate
clean:   # remove build output and metrics
	rm -rf target target-linux dist .metrics
