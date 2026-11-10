# libiec61850 (GPL-3.0) built in a throwaway container to make MMS captures. Nothing from it is vendored or committed;
# only the resulting pcaps (data) are. Run via ../capture-iec61850.sh.
FROM debian:12-slim
RUN apt-get update && apt-get install -y --no-install-recommends git ca-certificates cmake make g++ tcpdump iproute2 \
    && rm -rf /var/lib/apt/lists/*
RUN git clone --depth 1 https://github.com/mz-automation/libiec61850.git /lib61850 \
    && cd /lib61850 && cmake -DBUILD_EXAMPLES=ON -DBUILD_PYTHON_BINDINGS=OFF . >/dev/null && make -j4 >/dev/null 2>&1; \
    cd /lib61850 && git rev-parse HEAD > /lib61850/COMMIT; ls /lib61850/examples | head -50
