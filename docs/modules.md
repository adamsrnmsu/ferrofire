# External protocol modules (ffmod/1)

A protocol can be a separate program, in any language, that the field device starts, feeds and stops. The device and the program
speak **ffmod/1**: JSON lines over the program's stdin and stdout, or over a socket. bennu programs can instead use bennu's own
channels (a command interface and datagrams). Nothing in the device changes for a new module: drop it in a directory with a
`module.json` and a stock `<comms>` element names it.

The first module is BACnet/IP, server and client, in Python on bacpypes3: [`modules/bacnet/`](../modules/bacnet/README.md). The
design is in [the spec](superpowers/specs/2026-11-21-external-modules-design.md). Built with the `external` Cargo feature (default).

## Configuring a module

Either a stock element that an installed module serves:

```xml
<comms>
  <bacnet-server> ...stock bennu config, unchanged... </bacnet-server>
</comms>
```

or an explicit `<external-module>`:

```xml
<comms>
  <external-module>
    <name>bacnet</name>                                   <!-- for logs; not empty -->
    <command>python3 /opt/ferrofire/modules/bacnet/module.py</command>
    <transport>stdio</transport>                          <!-- stdio (default) | tcp://HOST:PORT | unix:///path | bennu -->
    <config><bacnet-server> ... </bacnet-server></config> <!-- the module's own element, sent verbatim in hello.config -->
  </external-module>
</comms>
```

`<command>` is split on whitespace, with no shell: write `sh -c '...'` yourself if you want one. It is required for `stdio` and
optional for the other transports. A client module's `<command-interface>` (in its element) is served by the device itself.

### Manifests and `--modules-dir`

A module directory holds `module.json`:

```json
{"serves": ["bacnet-server", "bacnet-client"], "command": "python3 module.py"}
```

At load, a `<comms>` child that no compiled module claims is looked up in the manifests under the modules directory:
`ferrofire field-device --modules-dir DIR`, else `$FERROFIRE_MODULES`, else `/opt/ferrofire/modules`. Every `DIR/*/module.json` is
read once, in name order; the first module that serves a tag keeps it, and a bad manifest is a warning naming the file. A hit runs
as if it were an `<external-module>` named after the tag (`module bacnet-server: ...` in the log), with the whole element as
`hello.config` and the stdio transport. Its command runs in the module's directory, so a relative `./run.sh` or `python3 module.py`
resolves against that directory, not the device's. A compiled module wins over a manifest for the same tag. With neither, the load
fails with `<bacnet-server> needs the proto-bacnet feature or an installed module`.

## ffmod/1

One JSON object per line, UTF-8, ending in `\n`. Field `t` is the type. Unknown fields are ignored. A line from a module that is not
JSON, has an unknown `t` or a malformed field, or is over 64 KiB (65536 bytes without the newline) is dropped and logged, and the
session goes on. Values are `true`/`false` for binary tags and numbers for analog ones. An analog that is NaN or infinite has no
JSON form: it is sent as `null`, and a module keeps its previous value.

### Device to module

| `t` | Example | Meaning |
|---|---|---|
| `hello` | `{"t":"hello","v":1,"device":"rtu-1","opts":{"strict":false},"config":"<bacnet-server>...</bacnet-server>","points":[{"tag":"brkr","kind":"binary","value":false},{"tag":"load-power","kind":"analog","value":2.5}]}` | First message of every session. `points` is every tag of the device with its kind and current value; `opts.strict` is `--strict`; `config` the module's element as written. |
| `values` | `{"t":"values","changes":[["load-power",2.6],["brkr",true]]}` | The tags that changed since the last `values`, checked once per scan cycle (`<cycle-time>`, default 1000 ms). No timestamps: use wall time. A module slow to read gets the newest value of each tag that changed, nothing in between. |
| `ack` | `{"t":"ack","id":7}` | `write` 7 was queued (`Store::write`). |
| `reject` | `{"t":"reject","id":7,"err":"unknown tag"}` | `write` 7 was refused: `unknown tag` or `wrong kind`. A module answers its own peer at once and only logs a reject. Up to 64 answers wait for a module that does not read; more are dropped (logged once). |
| `stop` | `{"t":"stop"}` | Shut down. The device then closes the stream (see Lifecycle). |

### Module to device

| `t` | Example | Meaning |
|---|---|---|
| `ready` | `{"t":"ready","role":"server","uses":[{"tag":"brkr","kind":"binary","dir":"out"},{"tag":"load-breaker-toggle","kind":"binary","dir":"in"}]}` | After `hello`, within 10 s. `role` is `server` or `client`. `uses` lists the tags the module maps: `dir` `in` when the module's peer supplies the value (a server's command, a client's polled input), `out` when the device does. An unknown tag or a wrong kind is a warning and the entry is skipped; under `--strict` it ends the module (and the device exits non-zero). |
| `write` | `{"t":"write","id":7,"tag":"load-breaker-toggle","value":true}` | A value from the module's peer. Server role: `Store::write`, as a compiled server's write. Client role: an `in` tag is a polled input; an `out` tag goes through the client echo rule (below). Answered with `ack` or `reject`. A `write` whose value is `null` or a string is malformed: dropped and logged, no answer. |
| `sent` | `{"t":"sent","tag":"load-mw-setpoint","value":3.0,"ok":true}` | Client role only. The module's peer answered (`ok`) or refused the write of `out` tag `load-mw-setpoint` that the device sent in `values`. **A client module must send `sent` for every `out` value it is given**: until it does, the device keeps that write pending (polls of the tag do not overwrite the local change), and after 10 s without it the write counts as failed and is due again. |
| `log` | `{"t":"log","level":"info","msg":"bound 0.0.0.0:47808"}` | Optional. `error`, `warn`, `info`, `debug`, `trace`; anything else is `info`. Lines on stderr are logged at `info` too. |
| `error` | `{"t":"error","msg":"Unknown endpoint protocol (tcp://1.2.3.4)"}` | The module cannot run (a bad config, a bind failure). Logged, and the module is not restarted; under `--strict` the device exits non-zero, as for a load error. |

A server module answers its peers from its own copy of the values (`hello`, then `values`). A client module polls its peer and
reports with `write`. For `role: client` the device runs the compiled clients' rules (`src/fd/proto/client.rs`): an `in` tag is
written as a polled input; an `out` tag starts from its `hello` value, and when the device changes it locally, the change goes down in
`values` and waits for `sent` before polls of that tag count again.

## Transports

**stdio** (default). The device spawns `<command>` in its own process group, with `FERROFIRE_MODULE=<name>` in its environment.
stdin and stdout carry ffmod/1 only; stderr lines are logged with the module's name.

**Socket**, `<transport>tcp://127.0.0.1:PORT</transport>` or `unix:///run/ff/bacnet.sock`. The device listens and the module connects;
the same messages flow over the connection. One connection at a time: a new one ends the running session (the old module is sent
`stop`) and starts a fresh one with a new `hello`. A host that is not loopback is a warning (anyone who can reach it can drive the
device); under `--strict` it is an error. A stale socket file is removed before binding, and the file is removed at stop unless
another device has bound the path since. `<command>` is optional: if present the device spawns it with `FERROFIRE_MODULE_ADDR` set to
the `<transport>` text, stdin and stdout null, and keeps it running on the backoff below; without it the module is started elsewhere.
A bind failure (port taken, bad path) is logged and is permanent: the module stays down for this run of the device, which keeps
running; under `--strict` the device exits non-zero.

**bennu**, for a bennu or pybennu program, no ffmod/1:

```xml
<external-module>
  <name>pyprobe</name>
  <transport>bennu</transport>
  <command-interface>tcp://127.0.0.1:1340</command-interface>   <!-- required -->
  <publish-group>udp://239.0.0.1:40001</publish-group>          <!-- required, udp:// -->
  <command>python3 probe.py</command>                           <!-- optional, stdin null -->
</external-module>
```

The device serves a command interface over every device tag, with the stock wording of `src/fd/cmdif.rs`: `QUERY=` lists every tag,
`READ=tag` reads any tag, `WRITE=tag:value` goes through `Store::write` (errors `no such tag`, `the tag is binary`). Each scan cycle
it multicasts the store's changes (the first cycle: every tag) to the group as datagram v1 bodies (`tag:value,`) framed as a
provider's publish, so a pybennu subscriber reads the device as it reads a provider. Direction: device to module only; the module's
writes come in through `WRITE`. No `hello`, no per-write ack beyond the reply.

## Lifecycle

- A module starts after the store is built, like the compiled modules. A command that is missing, not executable or exits at once
  is logged and restarted on the backoff below; the device keeps running.
- `ready` must come within **10 s** of `hello`; otherwise the module is stopped and restarted.
- Exit or a broken stream: restart after a pause of **1 s, doubling to 30 s**. The pause starts over at 1 s after a session that
  reached `ready`. Each restart is a fresh `hello`. `error` means no restart.
- Writes from a module that is restarting are lost, as a peer's are on a dropped TCP session.
- Stop: the device sends `stop` (its writer gets 1 s to put it on the wire), closes stdin, sends **SIGTERM to the module's process
  group 2 s** after the device's stop began and **SIGKILL at 5 s**. The device's own stop never waits longer than that; a module that
  never reads its stdin does not hold it up either.

## Worked example: the BACnet module from a stock config

`ff-modbac:22.04` is `ff-physics:22.04` with bacpypes3 and the module at `/opt/ferrofire/modules/bacnet` (its `module.json` serves
`bacnet-server` and `bacnet-client`). With our binary copied in, bennu's sample config runs unchanged:

    make modbac-image dut
    docker run -d --name bacnet-rtu ff-modbac:22.04 sleep infinity
    docker cp target-linux/debug/ferrofire bacnet-rtu:/usr/local/bin/ferrofire
    docker cp conformance/stock/bacnet-server.xml bacnet-rtu:/tmp/cfg.xml
    docker exec bacnet-rtu ferrofire field-device --file /tmp/cfg.xml

    Info: module bacnet-server: starting ["python3", "module.py"]
    Info: module bacnet-server: BACnet server up: instance 1234, 4074 objects, 172.17.0.2/16:47808

The module binds the address of `BACNET_IFACE` (default `eth0`) at `BACNET_IP_PORT` (47808), as stock does. To hold it to stock:

    make conformance PROTO=bacnet              # the stock device: probe, nmap, tshark, Zeek
    DUT=rust make conformance PROTO=bacnet     # the same against ferrofire + the module, and its transcript against stock's
    SERVER=rust make conformance PROTO=bacnet-client   # the stock client against our server
    DUT=rust CFG=conformance/bacnet-client/client-tags.xml make conformance PROTO=bacnet-client   # our client, tags declared

**The client is not drop-in yet.** A compiled client creates its tags from its points at load; a manifest-served element is opaque
to the device, so bennu's sample `<bacnet-client>`, which has no `<tags>`, loads but every tag the module names in `ready` is
unknown and skipped, and its command interface has no tags. Until that is fixed, declare the client's tags as `<internal-tag>`s
(as `conformance/bacnet-client/client-tags.xml` does). See [conformance/README.md](../conformance/README.md#bacnetip) for what the
runs compare and the divergences.

## Write your own

A module in `sh`. It serves `<echo-server><tag>NAME</tag></echo-server>`: it maps `NAME` as a tag its peer supplies, writes 1 to it,
logs each change the device sends, and exits on `stop`. `tests/external.rs` (`the_docs_example_runs_from_a_manifest`) installs this
script as `echo/echo.sh` with `{"serves":["echo-server"],"command":"./echo.sh"}` and runs a device with `--modules-dir`.

```sh
#!/bin/sh
# echo.sh: an ffmod/1 module that serves <echo-server><tag>NAME</tag></echo-server>.
# stdout is the session: only ffmod/1 lines go there. stderr is logged by the device.
read -r hello
tag=$(printf '%s\n' "$hello" | sed -n 's/.*<tag>\([^<]*\)<\/tag>.*/\1/p')
if [ -z "$tag" ]; then
  echo '{"t":"error","msg":"<echo-server> has no <tag>"}'
  exit 0
fi
# NAME is ours to supply (dir "in"); the device checks that it exists and is analog
echo "{\"t\":\"ready\",\"role\":\"server\",\"uses\":[{\"tag\":\"$tag\",\"kind\":\"analog\",\"dir\":\"in\"}]}"
echo "{\"t\":\"log\",\"level\":\"info\",\"msg\":\"serving $tag\"}"
# what a peer would send: one write, answered with ack or reject
echo "{\"t\":\"write\",\"id\":1,\"tag\":\"$tag\",\"value\":1}"
while read -r line; do
  case $line in
    *'"t":"values"'*) echo "changed: $line" >&2 ;;
    *'"t":"ack"'*) echo "write accepted" >&2 ;;
    *'"t":"reject"'*) echo "write refused: $line" >&2 ;;
    *'"t":"stop"'*) exit 0 ;;
  esac
done
```

Rules of thumb: never print anything but ffmod/1 on stdout (send a library's chatter to stderr); answer your peer without waiting
for `ack`; read stdin promptly (a module that does not read loses answers, not the session); exit on `stop` or end of stdin; send
`error` for a config you cannot serve rather than exiting, so the device does not restart you.
