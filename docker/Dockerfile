FROM ubuntu:22.04
RUN apt-get update -qq && apt-get install -y -qq curl gcc g++ pkg-config libzmq3-dev file binutils ca-certificates >/dev/null && curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable
ENV PATH=/root/.cargo/bin:$PATH
