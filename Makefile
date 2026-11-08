.PHONY: test linux
test:    # unit + integration tests in an ubuntu:22.04 container (no cargo needed on the host)
	docker image inspect rb-rust:22.04 >/dev/null 2>&1 || docker build --platform linux/arm64 -t rb-rust:22.04 docker
	docker run --rm --platform linux/arm64 -v $(CURDIR):/src -w /src -e CARGO_TARGET_DIR=/src/target-linux rb-rust:22.04 cargo test
linux:   # glibc aarch64 binaries -> dist/rustbelt, dist/simulinksolver
	./build-linux.sh
