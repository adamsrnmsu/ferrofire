FROM ubuntu:22.04
# Ubuntu's own hostnames (ports/archive/security.ubuntu.com) are flaky or unresolvable from some networks (GitHub runners).
# apt-retry: per-file retries, a failed index fetch is an error, two attempts against the default mirrors, then
# the sources are pointed at a third-party mirror (MIRROR) and retried.
ARG MIRROR=http://mirror.us.leaseweb.net
COPY apt-retry /usr/local/bin/apt-retry
RUN printf 'Acquire::Retries "5";\nAPT::Update::Error-Mode "any";\n' > /etc/apt/apt.conf.d/80retry
RUN apt-retry curl gcc g++ pkg-config cmake file binutils ca-certificates git make && curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain 1.99.1
ENV PATH=/root/.cargo/bin:/root/.local/bin:$PATH
# Quality gates (ub-mode): rustfmt, clippy, coverage for crapper/mutator.
RUN rustup component add rustfmt clippy llvm-tools-preview \
 && curl -sSfL "https://github.com/taiki-e/cargo-llvm-cov/releases/latest/download/cargo-llvm-cov-$(uname -m)-unknown-linux-gnu.tar.gz" | tar xz -C /root/.cargo/bin
# uv, then unclebob's crapper (CRAP), dryer (duplication) and mutator (mutation testing), pinned.
# mutator imports crapper, so it gets crapper in its own tool venv.
RUN curl -LsSf https://astral.sh/uv/install.sh | env UV_INSTALL_DIR=/root/.local/bin sh
ARG CRAPPER=git+https://github.com/unclebob/crapper@6fd92c4c9bcc56efb64a2f8743d5a2993bcfd42d
RUN uv tool install --python 3.11 "$CRAPPER" \
 && uv tool install --python 3.11 git+https://github.com/unclebob/dryer@66ff6d21a42c04afcad89c78a80066176d1294b0 \
 && uv tool install --python 3.11 git+https://github.com/unclebob/mutator@a5a0cb803b66e0cdb8bf471b3b08af701d424a41 --with "$CRAPPER"
# Supply chain gate: licences + advisories + bans (deny.toml), pinned.
ARG DENY=0.20.2
RUN curl -sSfL "https://github.com/EmbarkStudios/cargo-deny/releases/download/${DENY}/cargo-deny-${DENY}-$(uname -m)-unknown-linux-musl.tar.gz" | tar xz --strip-components=1 -C /root/.cargo/bin --wildcards '*/cargo-deny'
# Fuzzing (make fuzz): cargo-fuzz needs a nightly toolchain for the sanitizer flags; the project itself stays on the pinned stable.
RUN rustup toolchain install nightly --profile minimal && cargo install --locked cargo-fuzz
# MQTT tests (tests/mqtt_net.rs) start a real Mosquitto broker as a child process; openssl makes the throwaway CA for the TLS case.
RUN apt-retry --no-install-recommends mosquitto mosquitto-clients openssl && rm -rf /var/lib/apt/lists/*
