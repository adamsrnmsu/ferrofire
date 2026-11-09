# Everything that needs cargo runs in the ff-rust container (docker/Dockerfile); the Mac needs only docker and uv.
IMAGE := ff-rust:22.04
PLATFORM ?= linux/arm64
IN := docker run --rm --platform $(PLATFORM) -v $(CURDIR):/src -w /src -e CARGO_TARGET_DIR=/src/target-linux $(IMAGE)
# ub-mode thresholds: CRAP < 10 per function (worst allowed 9.99), dryer similarity < 0.8.
CRAP_MAX := 9.99
DRY_MIN := 0.8

.PHONY: help image fmt fmt-check lint test golden variants linux crap dry mutate check clean
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
variants: golden image   # single-transport builds (--features zmq / nng): clippy + tests; the nng-only binary must link no libzmq
	$(IN) bash -c 'for f in zmq nng; do cargo clippy --all-targets --no-default-features --features $$f -- -D warnings && cargo test --no-default-features --features $$f || exit 1; done; cargo build --no-default-features --features nng && ! ldd target-linux/debug/ferrofire | grep -q zmq && echo "nng-only binary links no libzmq"'
linux:   # glibc aarch64 binaries -> dist/ferrofire, dist/simulinksolver
	./build-linux.sh
crap: golden image   # crapper: CRAP score per function, fails above $(CRAP_MAX); writes .metrics/crap.edn
	$(IN) crapper --threshold $(CRAP_MAX) src
dry: image   # dryer: duplicate candidates scoring >= $(DRY_MIN), fails if any (dryer itself always exits 0)
	$(IN) bash -c 'dryer --threshold $(DRY_MIN) src | tee /tmp/dry.out; ! grep -q ^DUPLICATE /tmp/dry.out'
mutate: golden image   # mutator: mutation testing (slow, not part of check); survivors exit 3
	$(IN) mutator src
check: fmt-check lint test crap dry   # every gate except mutate
clean:   # remove build output and metrics
	rm -rf target target-linux dist .metrics
