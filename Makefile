# Everything that needs cargo runs in the ff-rust container (docker/Dockerfile); the Mac needs only docker and uv.
IMAGE := ff-rust:22.04
PLATFORM ?= linux/arm64
IN := docker run --rm --platform $(PLATFORM) -v $(CURDIR):/src -w /src -e CARGO_TARGET_DIR=/src/target-linux $(IMAGE)
# ub-mode thresholds: CRAP < 10 per function (worst allowed 9.99), dryer similarity < 0.8.
CRAP_MAX := 9.99
DRY_MIN := 0.8
# mutator rewrites src/ in place while it runs, so building or testing at the same time compiles a mutant
# (ww-clk: a flipped `+ 1` panicked as "subtract with overflow"). make mutate holds this lock; src-free refuses.
# ponytail: pid lock; a kill -9 of make leaves a stale file that src-free ignores because the pid is gone.
MUTATE_LOCK := .mutate.lock

.PHONY: differential diff-dnp3 diff-modbus testpeers conformance conformance-selftest src-free help wireshark zeek-iccp bench bench-report bench-dg bench-dg-report charts image fmt fmt-check lint test golden linux crap dry deny mutate fuzz fuzz-smoke check clean
help:    # list targets
	@grep -E '^[a-z-]+:.*#' $(MAKEFILE_LIST) | sed -E 's/^([a-z-]+):[^#]*#/\1\t/'
src-free:   # fail fast while make mutate is rewriting src/
	@if [ -e $(MUTATE_LOCK) ] && kill -0 "$$(cat $(MUTATE_LOCK))" 2>/dev/null; then \
	  echo "make mutate (pid $$(cat $(MUTATE_LOCK))) is rewriting src/; wait for it to finish" >&2; exit 1; fi
image:   # build the ff-rust container if missing (rebuild: docker build -t ff-rust:22.04 docker)
	@docker image inspect $(IMAGE) >/dev/null 2>&1 || docker build --platform $(PLATFORM) -t $(IMAGE) docker
fmt: image src-free   # cargo fmt
	$(IN) cargo fmt
fmt-check: image src-free   # cargo fmt --check
	$(IN) cargo fmt --check
lint: image src-free   # cargo clippy --all-targets -D warnings
	$(IN) cargo clippy --all-targets -- -D warnings
golden:  # PYPOWER 5.1.19 reference results -> target/golden (needs uv; generated, never committed)
	uv run --python 3.11 --with PYPOWER==5.1.19 --with 'numpy<2' --with 'scipy<1.14' python -I -W ignore tests/golden/gen_pypower.py target/golden
test: golden image src-free   # unit + integration tests in the container (no cargo needed on the host)
	$(IN) cargo test
wireshark:   # tshark + Lua: datagram v1 test vector, and iccp.lua over tests/pcaps/iccp (field goldens, 0 malformed; host tool, not in check)
	tools/wireshark/test.sh
zeek-iccp:   # build the ICCP Spicy analyzer and run its btest suite on tests/pcaps/iccp in the ff-zeek container (--update: ZEEK_ICCP_FLAGS=--update)
	tools/zeek/iccp/test.sh $(ZEEK_ICCP_FLAGS)
linux: src-free   # glibc aarch64 binaries -> dist/ferrofire, dist/simulinksolver
	./build-linux.sh
crap: golden image src-free   # crapper: CRAP score per function, fails above $(CRAP_MAX); writes .metrics/crap.edn
	$(IN) crapper --threshold $(CRAP_MAX) src
dry: image src-free   # dryer: duplicate candidates scoring >= $(DRY_MIN), fails if any (dryer itself always exits 0)
	$(IN) bash -c 'dryer --threshold $(DRY_MIN) src | tee /tmp/dry.out; ! grep -q ^DUPLICATE /tmp/dry.out'
deny: image   # cargo-deny: licences, advisories, bans, sources (deny.toml; advisories need network)
	$(IN) cargo deny check
mutate: golden image src-free   # mutator: mutation testing (slow, not part of check); survivors exit 3
	@echo $$$$ > $(MUTATE_LOCK); trap 'rm -f $(MUTATE_LOCK)' EXIT; $(IN) mutator src
bench: image   # historical zmq vs nng benchmark matrix (NNG dropped 2026-11-09; needs the old nng build, ~1.2 h) -> docs/bench/results/raw; then: make bench-report
	python3 bench/run.py
bench-report:   # tables, charts and docs/bench/transport-comparison.md from the raw results (needs uv)
	uv run --with matplotlib python bench/report.py
charts:   # docs/img/charts/*.svg (+ CSVs) and docs/results.md from the committed raw results (needs uv)
	uv run --with matplotlib python bench/charts.py
bench-dg: image   # datagram v1 matrix (age, netem loss, per-RTU fan-out, restarts, faults; about 1.5 h) -> docs/bench/results/raw-datagram-v1
	python3 bench/run_dg.py
bench-dg-report:   # docs/bench/datagram-v1-bench.md from the datagram v1 raw results
	python3 bench/dg_report.py
FUZZ_TARGETS := request datagram expr model fd_datagram fd_config fd_modbus fd_dnp3 fd_iec61850 fd_iccp fd_mqtt
FUZZ_SECS ?= 10
fuzz-smoke: image   # every fuzz target for $(FUZZ_SECS)s each (CI); crashes land in fuzz/artifacts, turn each into a test
	$(IN) bash -c 'cd fuzz && for t in $(FUZZ_TARGETS); do cargo +nightly fuzz run $$t -- -max_total_time=$(FUZZ_SECS) -max_len=4096 || exit 1; done'
fuzz: image   # one target until it finds something: make fuzz T=expr (T defaults to request); Ctrl-C to stop
	$(IN) bash -c 'cd fuzz && cargo +nightly fuzz run $(or $(T),request)'
check: fmt-check lint test crap dry deny   # every gate except mutate
# The stock oracle image bennu:arm64 lives in the Lima VM's docker (not Docker Desktop's), so the conformance targets use that daemon.
# Override with TP_DOCKER_HOST=unix:///var/run/docker.sock (any daemon that has the DUT image).
TP_DOCKER_HOST ?= unix://$(HOME)/.lima/phenix/sock/docker.sock
testpeers: export DOCKER_HOST := $(TP_DOCKER_HOST)
testpeers:   # build the ferrofire-testpeers image (opendnp3, libiec61850, tshark, Zeek+ICSNPP, nmap); PLATFORM=linux/amd64 also works
	docker build --platform $(PLATFORM) -t ferrofire-testpeers docker/testpeers
conformance: export DOCKER_HOST := $(TP_DOCKER_HOST)
conformance:   # acceptance harness for one protocol vs the stock oracle: make conformance PROTO=dnp3 (see conformance/README.md)
	@test -n "$(PROTO)" || { echo "usage: make conformance PROTO=dnp3"; exit 2; }
	conformance/run.sh $(PROTO)
conformance-selftest: export DOCKER_HOST := $(TP_DOCKER_HOST)
conformance-selftest:   # prove the harness fails on a damaged and on an empty capture (negative control)
	SELFTEST=1 conformance/run.sh dnp3

differential: export DOCKER_HOST := $(TP_DOCKER_HOST)
differential:   # one differential probe against the stock oracle or our binary: make differential PROTO=dnp3 WHO=stock|rust SET=diff|big
	@test -n "$(PROTO)" -a -n "$(WHO)" || { echo "usage: make differential PROTO=dnp3 WHO=stock|rust [SET=diff|big]"; exit 2; }
	conformance/differential.sh $(PROTO) $(WHO) $(or $(SET),diff)
diff-dnp3: export DOCKER_HOST := $(TP_DOCKER_HOST)
diff-dnp3:   # our DNP3 outstation vs the recorded stock transcripts, live in containers (build target-linux/debug/ferrofire first: make test)
	@for s in diff big; do conformance/differential.sh dnp3 rust $$s | diff conformance/dnp3/stock-$$s.golden - && echo "dnp3 $$s: byte-equal to stock" || exit 1; done
diff-modbus: export DOCKER_HOST := $(TP_DOCKER_HOST)
diff-modbus:   # our Modbus server vs the recorded stock transcripts, live in containers (build target-linux/debug/ferrofire first: make test)
	@for s in diff waterway edges; do conformance/differential.sh modbus rust $$s | diff conformance/modbus/stock-$$s.golden - && echo "modbus $$s: byte-equal to stock" || exit 1; done

clean:   # remove build output and metrics
	rm -rf target target-linux dist .metrics
