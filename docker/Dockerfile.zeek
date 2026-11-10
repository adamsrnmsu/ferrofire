# Zeek + Spicy + a C++ toolchain, for building and testing tools/zeek/iccp (make zeek-iccp). Not Malcolm's image:
# that one refuses a direct exec of zeek. Official zeek/zeek ships spicyz but no compiler.
FROM zeek/zeek:latest
RUN apt-get update && apt-get install -y --no-install-recommends g++ make cmake libpcap-dev tcpdump \
    && rm -rf /var/lib/apt/lists/*
