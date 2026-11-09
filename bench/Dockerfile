# Runtime image for the benchmark containers: ff-rust (libzmq, toolchain) plus ethtool to switch off offloads.
FROM ff-rust:22.04
RUN apt-get update -qq && apt-get install -y -qq ethtool iproute2 >/dev/null
