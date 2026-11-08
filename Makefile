.PHONY: test golden linux
golden:  # PYPOWER 5.1.19 reference results -> target/golden (needs uv; generated, never committed)
	uv run --python 3.11 --with PYPOWER==5.1.19 --with 'numpy<2' --with 'scipy<1.14' python -I -W ignore tests/golden/gen_pypower.py target/golden
test: golden   # unit + integration tests in an ubuntu:22.04 container (no cargo needed on the host)
	docker image inspect ff-rust:22.04 >/dev/null 2>&1 || docker build --platform linux/arm64 -t ff-rust:22.04 docker
	docker run --rm --platform linux/arm64 -v $(CURDIR):/src -w /src -e CARGO_TARGET_DIR=/src/target-linux ff-rust:22.04 cargo test
linux:   # glibc aarch64 binaries -> dist/ferrofire, dist/simulinksolver
	./build-linux.sh
