FROM ubuntu:22.04
RUN apt-get update -qq && apt-get install -y -qq curl gcc g++ pkg-config cmake file binutils ca-certificates git make >/dev/null && curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain 1.99.1
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
