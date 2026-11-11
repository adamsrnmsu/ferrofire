# ferrofire-testpeers: reference peers + blue-team tooling for the field-device acceptance harness (conformance/).
#   docker build -t ferrofire-testpeers docker/testpeers          (arm64 and amd64: every stage is multi-arch)
# Licence policy: opendnp3 is Apache-2.0. libiec61850 (and mbpoll, from Debian) are GPL-3.0 and are shipped here ONLY as separate test peers in
# its own binaries; ferrofire never links it (iec61850bean Apache-2.0, go-iec61850 MIT, dnp3py MIT, igs/telcon permissive, Tahu/Paho/Mosquitto EPL-2.0), and nothing in this image is copied into a ferrofire release artefact.
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

# Independent TASE.2/ICCP stacks (ICCP conformance, both directions). Young projects, possibly AI-assisted: agreement with them is evidence, not proof.
# Each runs as its own process; they are never in ferrofire's Cargo graph and none of their code is copied. Pinned by commit.
#   igs-iec-tase2 + igs-iso9506-mms (aklira), Apache-2.0: igstase2-server, igstase2-client (the MMS stack is a sibling path dependency)
#   telcon-tase2 (janfietz), MIT OR Apache-2.0: tase2-server and tase2-client, the JSON-controlled daemons of crates/tase2-test-tools
FROM rust:1-trixie AS rustpeers
RUN apt-get update -qq && apt-get install -y -qq --no-install-recommends git ca-certificates >/dev/null
ARG IGS_TASE2=66a7a581b109e33a098aa234f25f832a178ac5b4
ARG IGS_MMS=075a7984564e18b288646e43d01417d1148b9730
ARG TELCON=c84e119564d80d2b5484fbb211ff0565945104c5
RUN fetch() { git init -q "$3" && git -C "$3" fetch -q --depth 1 "$1" "$2" && git -C "$3" checkout -q FETCH_HEAD; } \
 && fetch https://github.com/aklira/igs-iec-tase2 $IGS_TASE2 /src/igs-iec-tase2 \
 && fetch https://github.com/aklira/igs-iso9506-mms $IGS_MMS /src/igs-iso9506-mms \
 && fetch https://github.com/janfietz/telcon-tase2 $TELCON /src/telcon-tase2
RUN cd /src/igs-iec-tase2 && cargo build --release --locked -p igstase2-server -p igstase2-client >/dev/null 2>&1 \
 || (cd /src/igs-iec-tase2 && cargo build --release -p igstase2-server -p igstase2-client 2>&1 | tail -20; false)
RUN cd /src/telcon-tase2 && cargo build --release --locked -p tase2-test-tools --bins >/dev/null 2>&1 \
 || (cd /src/telcon-tase2 && cargo build --release -p tase2-test-tools --bins 2>&1 | tail -20; false)
RUN mkdir -p /out/bin /out/licenses \
 && cp /src/igs-iec-tase2/target/release/igstase2-server /src/igs-iec-tase2/target/release/igstase2-client /src/telcon-tase2/target/release/tase2-server /src/telcon-tase2/target/release/tase2-client /out/bin/ \
 && cp /src/igs-iec-tase2/LICENSE /out/licenses/igs-APACHE-2.0 && cp /src/telcon-tase2/LICENSE-MIT /out/licenses/telcon-MIT && cp /src/telcon-tase2/LICENSE-APACHE /out/licenses/telcon-APACHE-2.0; ls /out/licenses /out/bin

# otfabric/go-iec61850 v1.16.2 (MIT): a Go MMS + GOOSE stack independent of libiec61850 and iec61850-rs. goiec/ is our ~100-line client over it
# (go.sum pins the module graph); it runs as its own process (conformance/iec61850) and is never in ferrofire's Cargo graph.
FROM golang:1.26 AS gopeers
COPY goiec/ /src/goiec/
RUN cd /src/goiec && go build -trimpath -o /out/bin/ffgoiec . \
 && mkdir -p /out/licenses && cp "$(go list -m -f '{{.Dir}}' github.com/otfabric/go-iec61850)/LICENSE" /out/licenses/go-iec61850-MIT

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
# Independent Modbus/TCP clients (conformance/modbus): mbpoll (libmodbus) and pymodbus. A layer of its own so that adding a peer
# does not rebuild the Zeek packages above.
RUN apt-get update -qq && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends mbpoll python3-pymodbus >/dev/null \
 && rm -rf /var/lib/apt/lists/*
# jq: telcon's daemons answer JSON on a control socket
RUN apt-get update -qq && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends jq >/dev/null && rm -rf /var/lib/apt/lists/*
COPY --from=rustpeers /out/bin/ /usr/local/bin/
COPY --from=rustpeers /out/licenses/ /usr/share/doc/ferrofire-testpeers/
# MQTT peers (conformance/mqtt): Mosquitto (EPL-2.0 / EDL-1.0) broker and clients, Eclipse Paho for Python as the independent second
# client, and Eclipse Tahu (EPL-2.0, jars from Maven Central) behind our TahuHost.java as the Sparkplug B host application. All of
# them are separate test peers; ferrofire writes its own Sparkplug B schema from the public spec and links none of this.
RUN apt-get update -qq && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends \
      mosquitto mosquitto-clients python3-paho-mqtt python3-zmq default-jdk-headless maven >/dev/null && rm -rf /var/lib/apt/lists/*
COPY tahu/ /opt/tahu-host/
RUN cd /opt/tahu-host && mvn -q -B dependency:copy-dependencies -DoutputDirectory=lib >/dev/null && javac -cp 'lib/*' TahuHost.java \
 && rm -rf /root/.m2 && ls lib | grep -c jar
# Independent IEC 61850 / DNP3 peers (conformance/iec61850, conformance/dnp3):
#  - iec61850bean 1.9.0 (Apache-2.0, Fraunhofer ISE/OpenMUC heritage, Java, own ASN.1 codec) with asn1bean 1.12.0 and slf4j 1.7.25, from Maven
#    Central, sha256-pinned; run with `java -cp '/opt/iec61850bean/*' com.beanit.iec61850bean.app.ConsoleClient` (the JDK is installed above).
#  - ffgoiec (ours) on otfabric/go-iec61850 v1.16.2 (MIT), built in the gopeers stage.
#  - dnp3py 0.6.0 (PNNL, MIT, pure Python master), in a venv so Debian's pip policy stays untouched; module `dnp3`.
RUN apt-get update -qq && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends curl python3-venv >/dev/null && rm -rf /var/lib/apt/lists/*
RUN set -e; mkdir -p /opt/iec61850bean && cd /opt/iec61850bean; M=https://repo1.maven.org/maven2; \
    get() { curl -fsSL --retry 3 -o "$(basename "$1")" "$M/$1"; echo "$2  $(basename "$1")" | sha256sum -c -; }; \
    get com/beanit/iec61850bean/1.9.0/iec61850bean-1.9.0.jar df114f512c4f9855ac977e52d204eb954d6e4f68842ac00f28ce9a678e879bcb; \
    get com/beanit/asn1bean/1.12.0/asn1bean-1.12.0.jar 9fa33202dcf1d00f84f289000b0f937aa910b74f771976a625fd638b6f270878; \
    get org/slf4j/slf4j-api/1.7.25/slf4j-api-1.7.25.jar 18c4a0095d5c1da6b817592e767bb23d29dd2f560ad74df75ff3961dbde25b79; \
    get org/slf4j/slf4j-simple/1.7.25/slf4j-simple-1.7.25.jar 0966e86fffa5be52d3d9e7b89dd674d98a03eed0a454fbaf7c1bd9493bd9d874
RUN python3 -m venv /opt/dnp3py && /opt/dnp3py/bin/pip install -q --no-cache-dir dnp3py==0.6.0 \
 && /opt/dnp3py/bin/python -c "import dnp3.master"
COPY --from=gopeers /out/bin/ /usr/local/bin/
COPY --from=gopeers /out/licenses/ /usr/share/doc/ferrofire-testpeers/
COPY --from=build /out/bin/ /usr/local/bin/
COPY --from=build /out/licenses/ /usr/share/doc/ferrofire-testpeers/
CMD ["bash"]
