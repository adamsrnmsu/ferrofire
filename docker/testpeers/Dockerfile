# ferrofire-testpeers: reference peers + blue-team tooling for the field-device acceptance harness (conformance/).
#   docker build -t ferrofire-testpeers docker/testpeers          (arm64 and amd64: every stage is multi-arch)
# Licence policy: opendnp3 is Apache-2.0. libiec61850 is GPL-3.0 and is built and shipped here ONLY as a separate test peer in
# its own binaries; ferrofire never links it, and nothing in this image is copied into a ferrofire release artefact.
ARG ZEEK_VERSION=9.0.1

FROM debian:trixie AS build
RUN apt-get update -qq && apt-get install -y -qq --no-install-recommends build-essential cmake git ca-certificates >/dev/null
# Pinned by commit (a tag can move). opendnp3 3.1.2, libiec61850 v1.6.2.
ARG OPENDNP3=26b4c01e4839bbbda8866655e086471c4917ee53
ARG LIBIEC61850=f0f797bf955da763353f7081c892a1ff6b55e5a1
RUN fetch() { git init -q "$3" && git -C "$3" fetch -q --depth 1 "$1" "$2" && git -C "$3" checkout -q FETCH_HEAD; } \
 && fetch https://github.com/dnp3/opendnp3 $OPENDNP3 /src/opendnp3 \
 && fetch https://github.com/mz-automation/libiec61850 $LIBIEC61850 /src/libiec61850
# Our scriptable master joins opendnp3's own example tree so it links exactly like the stock examples do.
COPY ff-dnp3-master.cpp CMakeLists.txt /src/opendnp3/cpp/examples/ff-master/
RUN echo 'add_subdirectory(./cpp/examples/ff-master)' >> /src/opendnp3/CMakeLists.txt \
 && cmake -S /src/opendnp3 -B /b/dnp3 -DCMAKE_BUILD_TYPE=Release -DDNP3_STATIC_LIBS=ON -DDNP3_EXAMPLES=ON >/dev/null \
 && cmake --build /b/dnp3 -j"$(nproc)" >/dev/null
# R-GOOSE/R-SMV need mbedtls; the harness does not use them.
RUN cmake -S /src/libiec61850 -B /b/iec -DCMAKE_BUILD_TYPE=Release -DBUILD_EXAMPLES=ON \
      -DCONFIG_IEC61850_R_GOOSE=OFF -DCONFIG_IEC61850_R_SMV=OFF >/dev/null \
 && cmake --build /b/iec -j"$(nproc)" >/dev/null
RUN mkdir -p /out/bin /out/licenses \
 && cp /b/dnp3/cpp/examples/outstation/outstation-demo /b/dnp3/cpp/examples/master/master-demo /b/dnp3/cpp/examples/ff-master/ff-dnp3-master /out/bin/ \
 && cd /b/iec/examples && for e in server_example_simple server_example_basic_io server_example_control server_example_goose \
      iec61850_client_example1 client_example_control iec61850_client_example_reporting goose_publisher_example goose_subscriber_example \
      goose_observer mms_utility; do cp "$(find . -name "$e" -type f -perm -u+x | head -1)" /out/bin/; done \
 && cp /src/opendnp3/LICENSE /out/licenses/opendnp3-APACHE-2.0 && cp /src/libiec61850/COPYING /out/licenses/libiec61850-GPL-3.0

FROM zeek/zeek:${ZEEK_VERSION}
RUN apt-get update -qq && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends \
      tshark tcpdump nmap python3 ca-certificates git cmake g++ make libpcap-dev >/dev/null \
 && rm -rf /var/lib/apt/lists/*
# CISA ICSNPP parsers. Not every package builds on every Zeek release or arch; the ones that did are listed in
# /etc/ff-testpeers.zeek-packages and a harness that needs a missing one skips its Zeek step with a warning.
ARG ICSNPP="icsnpp-dnp3 icsnpp-modbus icsnpp-bacnet icsnpp-s7comm icsnpp-enip icsnpp-opcua-binary icsnpp-profinet icsnpp-synchrophasor"
ENV PATH=/usr/local/zeek/bin:$PATH
RUN zkg autoconfig >/dev/null 2>&1; : > /etc/ff-testpeers.zeek-packages; \
    for p in $ICSNPP; do if zkg install --force --skiptests "https://github.com/cisagov/$p" >/tmp/zkg-$p.log 2>&1; \
      then echo "$p" >> /etc/ff-testpeers.zeek-packages; else echo "zkg: $p not installed (see build log)" >&2; tail -5 /tmp/zkg-$p.log >&2; fi; done; \
    cat /etc/ff-testpeers.zeek-packages
COPY --from=build /out/bin/ /usr/local/bin/
COPY --from=build /out/licenses/ /usr/share/doc/ferrofire-testpeers/
CMD ["bash"]
