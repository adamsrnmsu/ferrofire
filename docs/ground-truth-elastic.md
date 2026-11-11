# Ground truth in Elasticsearch / OpenSearch

Two ways for ferrofire to put a range's ground truth (the provider's tag values, not what the RTUs report) into Elasticsearch or
OpenSearch, so SCEPTRE experiments, Malcolm dashboards and SCORCH checks see the same data they see from pybennu:

| | Replaces | Reads | Writes |
|---|---|---|---|
| `ferrofire groundtruth` | pybennu's `groundtruth_monitor` (`pybennu-groundtruth-monitor`) | a provider's UDP datagram stream | one document per published point, into `bennu-YYYY.MM.DD` |
| the provider's `[elastic]` export (`ferrofire run --config config.ini`, `pybennu-power-solver` from `ferrofire pypower`) | the `[elastic]` settings of pybennu's providers (`elastic.py`) | the provider's own tag snapshots | one document per tag per publish, into `<index_basename>-YYYY.MM.DD`, with an index template |

Both talk to Elasticsearch 7.8+ / 8 and OpenSearch 1+ / 2 through the `_bulk` API (no `_type`, so 8 and 2 are the same code), over
plain HTTP or TLS (rustls), with no auth, basic auth or an API key. Neither needs a client library: the HTTP/1.1 client is about
270 lines in `src/elastic/http.rs` over `rustls` (ring), which is already in the tree, so the static musl binary is unchanged and
`cargo deny` sees no new crate. Both are in the default build (`elastic` feature); `--no-default-features` leaves them out.

## `ferrofire groundtruth`

    ferrofire groundtruth [-d] [-e ENV] [-c config.ini] start|stop|restart|status

The same command line, config.ini sections and daemon files as the stock monitor (pidfile
`/var/run/bennu-ground-truth-mon-<env>.pid`, default config `/etc/sceptre/<env>.ini`, `-d` to daemonize). It listens on the
`publish-endpoint` the way a DISH socket does: the datagram's group must be the one the endpoint hashes to
([datagram-v1.md](datagram-v1.md)); other groups and garbage are ignored.

```ini
[power-groundtruth-monitor]          ; or [groundtruth-monitor]; the first one present is used
publish-endpoint = udp://239.0.0.1:40000      ; default, as in stock
elasticsearch-ip = 127.0.0.1                  ; default; a host, host:port, or http(s)://host:port
filter           = /                          ; default: everything
; added by ferrofire, all optional:
elasticsearch-user     = elastic              ; basic auth (with -password)
elasticsearch-password = changeme
elasticsearch-api-key  = VnVhQ2ZHY0JD...      ; the "encoded" value of an Elasticsearch API key (instead of user/password)
elasticsearch-ca-file  = /etc/ssl/es-ca.pem   ; PEM CA for https; without it the system roots are used
index-basename         = bennu                ; the index is <basename>-YYYY.MM.DD
queue-size             = 100000               ; points held while the server is slow; past it the oldest are dropped
threads                = 3                    ; sender threads
```

A bare host is `http://host:9200`, as in stock. Keys with `_` and `-` are interchangeable.

**Filter.** Entries separated by single spaces, each `/provider/device/field`; a point passes when, for any entry, the provider
string (the `publish-endpoint` as written), the device and the field each contain the entry's part as a substring. As in stock, an
entry that does not have all three parts matches everything, and so does the default `/` (a warning at start names such entries).

**Documents.** Each point `device.field:value` of a published message becomes

```json
{"@timestamp": "2026-11-11T16:55:44.237Z", "provider": "udp://239.0.0.1:40000", "device": "pump", "field": "on", "status": "true"}
{"@timestamp": "2026-11-11T16:55:44.237Z", "provider": "udp://239.0.0.1:40000", "device": "tank", "field": "level", "value": 2.0}
```

`value` is a float when the text parses as a finite one, otherwise `status` carries the text (`true`, `false`, anything else). A tag
with no dot, or with more than one, has the part before the first dot as `device` and an empty `field`. No template is installed
and none is needed: the mapping is Elasticsearch's dynamic one, as with the stock script.

## The provider's `[elastic]` export

```ini
[elastic]
enabled        = true                       ; default false
host           = http://localhost:9200      ; URL (default as in pybennu)
index_basename = opalrt-clean               ; default as in pybennu: default-index-basename-changeme
threads        = 3
; added by ferrofire, all optional:
user = elastic            ; basic auth, with password
password = changeme
api-key  = ...            ; instead of user/password
ca-file  = /etc/ssl/es-ca.pem
queue-size = 600          ; snapshots (a document per tag each) held while the server is slow; past it the oldest are dropped
frequency  = grid.freq    ; a number (60) or the name of a published tag: measurement.frequency on every document
experiment = e1           ; sceptre.experiment / topology / scenario
topology   = t1
scenario   = s1
hostname   = power-provider   ; observer.name and observer.hostname (default: the machine's name)
```

`ferrofire run model.yaml --config config.ini` reads the `[elastic]` section of any config.ini; `pybennu-power-solver` /
`ferrofire pypower` read it from the config.ini they already take. With no section, or `enabled = false`, nothing changes.

**Documents** follow `elastic.py`, in the shape the OPAL-RT provider writes for each Modbus read, one per tag per publish:

```json
{"@timestamp": "2026-11-11T16:55:34.236Z", "sceptre_time": "2026-11-11T16:55:34.236Z", "simulator_time": "2026-11-11T16:55:34.236Z",
 "time_drift": 0.0, "event": {"ingested": "2026-11-11T16:55:34.255Z"}, "ecs": {"version": "8.11.0"},
 "agent": {"type": "sceptre-provider", "version": "0.1.0"},
 "observer": {"hostname": "power-provider", "name": "power-provider", "geo": {"timezone": "UTC"}},
 "sceptre": {"experiment": "e1", "topology": "", "scenario": "", "provider": "ferrofire",
             "server_endpoint": "tcp://0.0.0.0:5555", "publish_endpoint": "udp://*;239.0.0.1:40000"},
 "groundtruth": {"tag": "tank.level", "value": "2", "type": "float", "float": 2.0, "description": ""},
 "measurement": {"frequency": 60.0}}
```

- `@timestamp` and `sceptre_time` are the wall clock (UTC) at the publish. `simulator_time` is the simulator's clock: the engine's
  model time added to the wall time of the first publish, so it starts in step with the wall clock. `time_drift` is the gap
  between the two in milliseconds (`abs(sceptre - simulator) * 1000`, as `rtds.py` computes it): how far the real-time engine has
  fallen behind. The power solver has no model time, so there the two clocks are the same.
- `groundtruth.type` is `float` (with `float`), `bool` (with `bool`, for a tag published as `true`/`false`) or `string`;
  `groundtruth.value` is always the published text, as in the OPAL-RT provider.
- `measurement.frequency` is on **every** document when `frequency` is set, because the SCORCH check reads it from whichever
  document is newest. Neither the waterway model nor a power-flow solution has a grid frequency, so nothing is assumed: name a
  published tag that carries it (the model declares one as an output) or give a number. Without `frequency`, documents carry no
  `measurement`.
- Not exported: `observer.ip` (the provider's own address is not known to the engine), `network.*`, `pmu.*`, `modbus.*` (they
  describe pybennu's PMU and Modbus pollers). The fields are in the template for compatibility.
- The `_ff.*` trailer of datagram v1 is not data and is not exported.

**Index template.** Before the first document the export puts `PUT _index_template/<index_basename>` with the pattern
`<index_basename>-*` and pybennu's field mapping (`ES_BASE_TYPE_MAPPING`: `@timestamp`, `sceptre_time`, `simulator_time` as
`date`, `time_drift` and `measurement.frequency` as `double`, `observer.ip` and `pmu.ip` as `ip`, `groundtruth.tag` and
`.value` as `keyword`, `.float` `double`, `.bool` `boolean`, and the rest) and `refresh_interval: 1s`, which pybennu does not set: with the server's default an index nobody searches stops refreshing, so
the document count SCORCH's `providerdata` reads from `_stats` stands still while documents arrive. pybennu creates each day's index with that mapping
itself; a template does the same for every daily index and for several providers that share a basename, whoever writes first. If
the server refuses the template it is retried with backoff and documents wait behind it; at shutdown it is skipped and dynamic
mapping takes over.

## Never in the engine's way

The engine tick (and the monitor's receive loop) only copies the message into a bounded queue; building documents, JSON and all
network I/O happen on worker threads. A full queue drops the **oldest** item and counts it, so a dead or slow server costs
memory up to `queue-size` and then data, never time: the unit test pushes 100 000 items into a stuck queue in well under a second.

- Requests are bounded: at most 1000 documents and 8 MB of source each, however many a backlog holds (a provider snapshot is a
  document per tag, so after an outage the backlog is cut into requests as it is sent). `queue-size` counts what the producer queues:
  documents for the monitor (about 150 bytes each, default 100 000), snapshots for the provider (default 600: a case with thousands of tags
  is megabytes a snapshot, so the bound is small).
- A failed request (connection refused, timeout, TLS error, HTTP 401/429/5xx, an unreadable response) is retried with backoff,
  0.5 s doubling to 30 s, while the queue keeps filling. Each failure is logged with the cause, and the queue's drops are logged
  when they happen (`elastic: queue full, dropped the oldest N items`).
- A bulk response that reports per-document errors is read: documents refused with 429 or 5xx are sent again, the others (a
  mapping conflict, say) are counted as refused and the first reason is logged. One bad document does not lose or hold up the batch.
- A request refused whole as malformed (400) or too large (413) is not sent again as it was: it is split in halves, down to single
  documents; a document refused on its own is counted as lost and logged. Every other failure (401, 403, 5xx) is retried unchanged.
- At shutdown (SIGTERM, or the provider stopping) the workers send what is queued for at most 10 s, then give up on the rest and
  count it. The exit line reads `sent N documents, refused N, retried N requests, dropped N (queue full)`.

## Differences from the stock script

The stock `groundtruth_monitor.py` and `elastic.py` are the references. Where ferrofire deliberately differs:

| Stock | ferrofire | Why |
|---|---|---|
| index date and `@timestamp` from `datetime.now()`: the local time of the machine, no zone | UTC, `@timestamp` as `...Z` with milliseconds | on a host not set to UTC the stock monitor writes local time that Elasticsearch reads as UTC, and puts documents of one moment in different indices on two machines. Shown live: under `TZ=UTC+13` the stock `@timestamp` is 13 h ahead and the day is the next one |
| an unbounded list drained every 0.5 s; a failed push is logged and its messages **lost** | bounded queue, retry with backoff, partial bulk failures resent, drops counted and logged | a server restart lost every message of that second; a long outage grew memory without limit |
| plain HTTP, no auth, port 9200 only | `http(s)`, a port in the URL, basic auth or API key, a CA file | Elasticsearch 8 and Malcolm's OpenSearch run with security on |
| a point whose tag contains `alert` or `batch` inside the 30 s window: `return`, which drops **the rest of the message** (every point after it, alert or not); the window starts at start-up | only those points are skipped; the first message after a window opens lets all of its alert/batch points through; the first window is open at start | live: `gt.before:1,alert.high:2,gt.after:3,batch.n:4,gt.last:5` gives stock `gt.before`, ferrofire all five |
| indexes the datagram trailer (`_ff.seq`, `_ff.cyc`, `_ff.t`) as `device=_ff` | skips it | it is transport bookkeeping, not ground truth; stock bennu ignores it too (363 such documents in the differential run, against 605 data documents) |
| `NaN` and `inf` become a `value` (Python writes `NaN` into the JSON, which is not JSON) | `status` holds the text | Elasticsearch rejects the document |
| a point with no `:` raises `IndexError` in the handler, which nothing in the receive loop catches (from reading the code) | skipped | one malformed point should not end the monitor |
| the stock client is the Python `elasticsearch` library, which since version 8 refuses OpenSearch | works with both | Malcolm is OpenSearch |

Identical by test: the document fields and types, the tag/field split, the filter semantics, the value-or-status rule, the index
name's shape (`bennu-YYYY.MM.DD`, apart from the zone), and for the provider the field names and mapping of `elastic.py`.

Also different, not a deliberate fix: stock's SIGTERM handler calls `self.disconnect()`, which its `Subscriber` base class does not
have; ferrofire stops cleanly on SIGINT and SIGTERM. The monitor does not drop duplicate datagrams by `_ff.seq` (neither does stock),
so a duplicated datagram becomes duplicated documents, and documents have no `_id`, so a request the server applied but whose
answer was lost is sent again and duplicates its documents (bulk is at-least-once).

## Tests

See [testing.md](testing.md#ground-truth-to-elasticsearch-and-opensearch). In short: unit tests of the pure pieces (document
building, index naming, the filter, the limiter, the bulk body and response reading, backoff, the template, the HTTP framing),
integration tests against a fake server (`tests/elastic.rs`), and `make conformance PROTO=elastic`, which runs the lot against real
Elasticsearch 8, OpenSearch 2 and a secured Elasticsearch 8 (TLS, basic auth, API key), compares the stock monitor field by field,
and runs the SCORCH `providerdata` component's own check against the provider export.
