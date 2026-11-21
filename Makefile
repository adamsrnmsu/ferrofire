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

.PHONY: dut netns differential diff-dnp3 diff-modbus diff-serial diff-serial-mismatch testpeers conformance conformance-selftest src-free help wireshark zeek-iccp zeek-iccp-coexist bench bench-report bench-dg bench-dg-report bench-control bench-fdmicro bench-maxrate bench-maxrate-control charts image rtu-image rtu-image-smoke fmt fmt-check lint test golden linux crap dry deny mutate fuzz fuzz-smoke check clean
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
netns: image src-free   # ww-1df.27: RADIO publisher in a private netns with one interface address (needs a privileged container: CAP_SYS_ADMIN, /dev/net/tun)
	$(subst docker run --rm,docker run --rm --privileged,$(IN)) cargo test --test netns -- --nocapture
wireshark:   # tshark + Lua: datagram v1 test vector, and iccp.lua over tests/pcaps/iccp (field goldens, 0 malformed; host tool, not in check)
	tools/wireshark/test.sh
zeek-iccp:   # build the ICCP Spicy analyzer and run its btest suite on tests/pcaps/iccp in the ff-zeek container (--update: ZEEK_ICCP_FLAGS=--update)
	tools/zeek/iccp/test.sh $(ZEEK_ICCP_FLAGS)
zeek-iccp-coexist:   # ICCP analyzer next to CISA ICSNPP s7comm on tcp/102: DPD gives each its own flows (needs the testpeers image, Lima docker)
	tools/zeek/iccp/test-coexist.sh
dut: image src-free   # debug linux binaries the conformance targets run (target-linux/debug/ferrofire, examples/iccp-client); incremental, so a stale DUT is rebuilt (ww-tyc)
	$(IN) cargo build --bins --examples
linux: src-free   # glibc aarch64 binaries -> dist/ferrofire, dist/simulinksolver
	./build-linux.sh
rtu-image: linux   # Buildroot arm64 RTU image -> image/out/ferrofire-rtu-arm64.qc2 (clean build ~20 min; image/build.sh)
	image/build.sh
rtu-image-smoke:   # boot the RTU image in QEMU (container, TCG) and run the SSH/telnet smoke checks
	image/test/smoke.sh
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
STACK ?= ferrofire
bench-control: image   # control-latency matrix (breaker flips, generator spin up/down, 1-10 RTUs; hours) -> docs/bench/results/control/$(STACK); STACK=ferrofire (see bench/run_control.py for the env knobs)
	STACK=$(STACK) python3 bench/run_control.py
bench-fdmicro: image   # field-device microbench, ferrofire vs stock bennu-field-deviced with a scripted provider (about 2 h, 3 reps) -> docs/bench/results/fdmicro; then: make charts (bench/fdmicro/README.md)
	python3 bench/fdmicro/run.py
bench-maxrate: image   # maximum publish rate, ferrofire provider -> ferrofire field device (3 reps, about 1.2 h) -> docs/bench/results/maxrate; then: make charts (bench/maxrate/README.md)
	SECS=10 python3 bench/maxrate/run.py
bench-maxrate-control: image   # grid flip latency at each engine dt (bench/run_control.py DT_MS; about 1 h) -> docs/bench/results/maxrate/control/dt<ms>
	for d in 50 20 10 5 2 1; do DT_MS=$$d REPS=3 RS=1,10 CELLS=breaker:seq:100,breaker:sim:100 OUTDIR=docs/bench/results/maxrate/control/dt$$d python3 bench/run_control.py || exit 1; done
bench-dg-report:   # docs/bench/datagram-v1-bench.md from the datagram v1 raw results
	python3 bench/dg_report.py
FUZZ_TARGETS := request datagram expr model fd_datagram fd_config fd_cmdif fd_modbus fd_modbus_client fd_dnp3 fd_dnp3_master fd_iec61850 fd_iccp fd_mqtt fd_scpi fd_hislip fd_vxi11
FUZZ_SECS ?= 10
fuzz-smoke: image   # every fuzz target for $(FUZZ_SECS)s each (CI); crashes land in fuzz/artifacts, turn each into a test
	$(IN) bash -c 'cd fuzz && for t in $(FUZZ_TARGETS); do cargo +nightly fuzz run $$t -- -max_total_time=$(FUZZ_SECS) -max_len=4096 || exit 1; done'
fuzz: image   # one target until it finds something: make fuzz T=expr (T defaults to request); Ctrl-C to stop
	$(IN) bash -c 'cd fuzz && cargo +nightly fuzz run $(or $(T),request)'
check: fmt-check lint test crap dry deny   # every gate except mutate
# The stock oracle image ffc-stock-2026.11.03:22.04 (bennu v2026.11.03) and ferrofire-testpeers live on Docker Desktop's daemon, so the
# conformance targets use it. Override with TP_DOCKER_HOST=unix:///var/run/docker.sock (any daemon that has both images).
TP_DOCKER_HOST ?= unix://$(HOME)/.docker/run/docker.sock
# Set per command, not as a target-specific export: that would leak into prerequisites (dut, image) and build on the wrong daemon.
TP := DOCKER_HOST=$(TP_DOCKER_HOST)
testpeers:   # build the ferrofire-testpeers image (opendnp3, libiec61850, tshark, Zeek+ICSNPP, nmap); PLATFORM=linux/amd64 also works
	$(TP) docker build --platform $(PLATFORM) -t ferrofire-testpeers docker/testpeers
conformance: dut   # acceptance harness for one protocol vs the stock oracle: make conformance PROTO=dnp3 (see conformance/README.md)
	@test -n "$(PROTO)" || { echo "usage: make conformance PROTO=dnp3"; exit 2; }
	$(TP) conformance/run.sh $(PROTO)
conformance-selftest:   # prove the harness fails on a damaged and on an empty capture (negative control)
	$(TP) SELFTEST=1 conformance/run.sh dnp3

differential: dut   # one differential probe against the stock oracle or our binary: make differential PROTO=dnp3 WHO=stock|rust SET=diff|big|out|grp|cfg|ovf|nack|nopv
	@test -n "$(PROTO)" -a -n "$(WHO)" || { echo "usage: make differential PROTO=dnp3 WHO=stock|rust [SET=diff|big|out|grp|cfg|ovf|nack|nopv]"; exit 2; }
	$(TP) conformance/differential.sh $(PROTO) $(WHO) $(or $(SET),diff)
diff-dnp3: dut   # our DNP3 outstation vs the recorded stock transcripts, live in containers
	@for s in diff big out grp cfg ovf nack nopv; do $(TP) conformance/differential.sh dnp3 rust $$s | diff conformance/dnp3/stock-$$s.golden - && echo "dnp3 $$s: byte-equal to stock" || exit 1; done
diff-modbus: dut   # our Modbus server vs the recorded stock transcripts, live in containers
	@for s in diff waterway edges; do $(TP) conformance/differential.sh modbus rust $$s | diff conformance/modbus/stock-$$s.golden - && echo "modbus $$s: byte-equal to stock" || exit 1; done
SERIAL_SETTINGS := \# serial line: 9600 8N1 flow=none raw
diff-serial: dut   # both servers on a serial line (a paced 9600 8N1 pty relayed to the TCP port, ww-8k4, ww-y51) vs the stock transcripts and port settings
	@mkdir -p target; for s in diff waterway edges; do g=conformance/modbus/stock-serial-$$s.golden; [ -f $$g ] || g=conformance/modbus/stock-$$s.golden; \
	  { cat $$g; echo "$(SERIAL_SETTINGS)"; } > target/serial-want.txt; \
	  SERIAL=listen:5502 $(TP) conformance/differential.sh modbus $(or $(WHO),rust) $$s 2>/dev/null | diff target/serial-want.txt - && echo "modbus $$s on serial: byte-equal to stock" || exit 1; done
	@for s in diff big out grp cfg ovf nack nopv; do g=conformance/dnp3/stock-serial-$$s.golden; [ -f $$g ] || g=conformance/dnp3/stock-$$s.golden; \
	  { cat $$g; echo "$(SERIAL_SETTINGS)"; } > target/serial-want.txt; \
	  SERIAL=listen:20000 $(TP) conformance/differential.sh dnp3 $(or $(WHO),rust) $$s 2>/dev/null | diff target/serial-want.txt - && echo "dnp3 $$s on serial: byte-equal to stock" || exit 1; done
diff-serial-mismatch: dut   # negative control: on a line at another speed or parity neither stock nor ours may give the stock transcript (WHO=stock rust)
	@mkdir -p target; for line in 19200-8N1 9600-8E1; do for who in $(or $(WHO),stock rust); do \
	  for job in modbus,5502,waterway dnp3,20000,big; do p=$${job%%,*}; rest=$${job#*,}; port=$${rest%%,*}; set=$${rest#*,}; \
	    SERIAL=listen:$$port SERIAL_LINE=$$line $(TP) conformance/differential.sh $$p $$who $$set 2>target/serial-mismatch.err >target/serial-mismatch.txt; \
	    if diff -q conformance/$$p/stock-$$set.golden target/serial-mismatch.txt >/dev/null; then echo "$$p $$who at $$line: talks, it should not"; exit 1; fi; \
	    grep -q MISMATCH target/serial-mismatch.err || { echo "$$p $$who at $$line: the relay saw no mismatch"; exit 1; }; \
	    echo "$$p $$who at $$line: no stock transcript, as it should ($$(grep -m1 MISMATCH target/serial-mismatch.err))"; \
	  done; done; done

clean:   # remove build output and metrics
	rm -rf target target-linux dist .metrics
