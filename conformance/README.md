# Acceptance harness

Judges a field-device protocol the way a blue team sees it: capture the wire, then ask tshark and Zeek what they decoded.

    make testpeers                  # once: build the ferrofire-testpeers image (about 10 min; docker/testpeers/Dockerfile)
    make conformance PROTO=dnp3     # peers vs the stock oracle (bennu:arm64); exit 0 = PASS
    make conformance-selftest       # negative control: a damaged and an empty capture must be rejected

Output lands in `target/conformance/<PROTO>/`: the pcap, `peer.out`, `dut.out`, `zeek/*.log`.

## What it checks (every protocol)
1. The capture holds packets of the protocol (`PROTO_FILTER`); an empty capture fails.
2. `tshark -Y _ws.malformed` finds 0 frames, and `tshark -z expert,warn` has no Error or Warn rows. A bad checksum is only a Warn
   in Wireshark, so Warns count. `EXPERT_ALLOW` (regex, default `IIN Abnormality`) lists legitimate ones.
3. `check.sh`: field assertions (`want_field PCAP FILTER FIELD VALUE`) on what the peers sent and what came back.
4. `zeek.sh`: `zeek -r` with the CISA ICSNPP package, `want_zeek LOG COLUMN VALUE`. If the package is not in the image the step prints
   `SKIP`; `ZEEK_REQUIRED=1` turns that into a failure.

## Layout
    conformance/run.sh        host side: starts the DUT container (ff-conf-dut) and the peers (ff-conf-peers, same network namespace)
    conformance/inside.sh     peers side: tcpdump on lo, run peer.sh, then judge
    conformance/lib.sh        expert_clean, want_field, want_zeek
    conformance/<proto>/      proto.env (DUT config, start command, capture filter), peer.sh, check.sh, zeek.sh

Add a protocol: copy `conformance/dnp3/`, change the four files. Only `ff-conf-dut` and `ff-conf-peers` are created or removed.
Files go in and out with `docker cp` (no bind mounts: the Lima docker daemon cannot see every host path).

## Pointing it at another device
`DUT_IMAGE=<image> DUT_CMD='<command reading /tmp/cfg.xml>' conformance/run.sh dnp3` (the config is `CFG`, default the stock sample from
`BENNU_DATA`, default `~/git/sceptre-bennu/data/configs/ep`). The binary must be in `DUT_IMAGE`. With `DUT` unset the oracle is
`bennu:arm64` (SPEC section 8), so run on the daemon that has it: the Makefile defaults `TP_DOCKER_HOST` to the Lima socket.

## Test peers (docker/testpeers)
| Peer | Source | Licence | Use |
|---|---|---|---|
| `outstation-demo`, `master-demo` | opendnp3 3.1.2 examples | Apache-2.0 | stock DNP3 peers (interactive master) |
| `ff-dnp3-master HOST PORT LOCAL REMOTE [wait_s] [crob IDX CODE\|select-crob IDX CODE\|ao IDX VALUE]...` | ours, on opendnp3 | MIT / Apache-2.0 | scriptable master: integrity poll, commands, poll |
| `server_example_*`, `iec61850_client_example*`, `goose_publisher/subscriber/observer` | libiec61850 v1.6.2 | **GPL-3.0, test peers only, never linked** | IEC 61850 server/client/GOOSE |
| `mms_utility -h HOST -d`, `-r VAR -a DOMAIN` | libiec61850 | GPL-3.0 | generic MMS client for ICCP named-variable reads |
| tshark 4.4, tcpdump, nmap 7.95 (`iec61850-mms`, `iec-identify`, `s7-info`, `bacnet-info`, `enip-info`, `modbus-discover`) | Debian | | analysis |
| Zeek 9.0.1 + icsnpp-dnp3, modbus, bacnet, s7comm, enip, opcua-binary, synchrophasor | CISA | | `/etc/ff-testpeers.zeek-packages` lists what installed |

Not in the image: icsnpp-profinet (no such package), a Zeek IEC 61850/ICCP parser (none exists; those protocols are judged by tshark `mms`/`goose`
and the nmap scripts). nmap has no `dnp3-info` script. Licence texts: `/usr/share/doc/ferrofire-testpeers/`.

## Publishing
`.github/workflows/testpeers.yml` builds linux/amd64 + arm64 and pushes `ghcr.io/<owner>/ferrofire-testpeers` on a `v*` tag (or by hand).
It does nothing until a tag exists.
