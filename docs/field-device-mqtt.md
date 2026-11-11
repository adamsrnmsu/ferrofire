# MQTT and Sparkplug B (`<mqtt-client>`)

Feature `proto-mqtt` (on by default). The module is a client: it connects to a broker, publishes the tags it is given and takes
writes from command topics. Bennu has no MQTT, so there is no stock behaviour to match; the config keys below are ferrofire's.
It uses `rumqttc` (Apache-2.0) for MQTT 3.1.1 and `prost` (Apache-2.0) for Sparkplug B payloads; the schema is
[sparkplug_b.proto](sparkplug_b.proto), written from the public Eclipse Sparkplug specification (Eclipse Tahu and sparkplug-rs are
EPL-2.0 and are used only as test peers).

## Config

```xml
<comms>
  <mqtt-client>
    <endpoint>tcp://10.1.1.60:1883</endpoint>   <!-- tcp:// (default port 1883) or ssl:// (8883) -->
    <point><tag>level</tag></point>
    <point><tag>pump</tag><writable>true</writable></point>
    <point><tag>setpoint</tag><writable>true</writable><min-value>0</min-value><max-value>100</max-value></point>
  </mqtt-client>
</comms>
```

| Key | Default | Meaning |
|---|---|---|
| `<endpoint>` | required | `tcp://host[:1883]` or `ssl://host[:8883]` |
| `<point>` | none | a tag to publish: `<tag>` (must exist; an unknown one is warned about and skipped, an error under `--strict`), `<writable>` (false), `<min-value>` / `<max-value>` (limits for analog commands) |
| `<device>` | the `<field-device>` `<name>` | topic level for this device |
| `<client-id>` | `ferrofire-<device>` | MQTT client id; two `<mqtt-client>` elements in one device need different ones |
| `<topic-prefix>` | `ferrofire` | plain scheme only |
| `<qos>` | 1 | plain-scheme publish QoS (0-2). Commands are subscribed at QoS 1. Sparkplug uses 0 for NBIRTH/NDATA and 1 for the will, as the specification says |
| `<retain>` | true | retain the plain-scheme values (the status topic is always retained) |
| `<refresh>` | 60 | seconds between full republications of every point; 0 = only changes |
| `<queue>` | 256 | bound of the client's request queue; when it is full a publication is dropped and a new birth (everything again) follows |
| `<username>`, `<password>` | none | |
| `<tls-ca>` | required for `ssl://` | PEM file that signed the broker's certificate |
| `<tls-cert>`, `<tls-key>` | none | client certificate and key (PEM) for brokers that ask for one; both or neither |
| `<sparkplug>` | absent = plain | `<group-id>` (required) and `<edge-node-id>` (default the device) switch to Sparkplug B |
| `<event-logging>` | `mqtt-client.log` | the usual per-module CSV event log |

Topic levels (`<device>`, `<group-id>`, `<edge-node-id>`) may not contain `/`, `+` or `#`.

## Plain scheme

| Topic | Payload |
|---|---|
| `<prefix>/<device>/status` | `online` (after connect), `offline` (the will, and a clean stop); retained, QoS 1 |
| `<prefix>/<device>/<tag>` | analog `250.5`, binary `true` / `false`; on change and every `<refresh>` s |
| `<prefix>/<device>/<tag>/set` | command in: binary `true false 1 0 on off`, analog a finite number |

A command goes through the same checks as a Modbus or DNP3 write: the point must be `<writable>`, the value must have the tag's
type, an analog must be finite and within `<min-value>` / `<max-value>`, and what passes is queued in the datastore's write queue
(it takes effect when the provider acknowledges it, exactly like any protocol's). A refused command is a line in the event log.
A tag name containing `/` can be published but not commanded (the subscription is `<prefix>/<device>/+/set`).

## Sparkplug B

`spBv1.0/<group>/<type>/<edge node>`. NBIRTH (seq 0): `bdSeq` (Int64), `Node Control/Rebirth` (Boolean false), then every point with
name, alias (its position in the `<point>` list, from 1), Double or Boolean value. NDATA: changed points by alias, `seq` 1..255
wrapping; everything every `<refresh>` seconds. NDEATH: the will, with the same `bdSeq` as the NBIRTH of that session; a clean stop
publishes it too. `bdSeq` counts connection attempts from 0. NCMD: a metric named `Node Control/Rebirth` = true makes the node
publish a new NBIRTH (same `bdSeq`); any other metric, by name or alias, is a write with the checks above.
Not done: the primary-host `STATE` topic handling, DBIRTH / DDATA (devices below the node), Int/Float metrics on output, datasets,
templates and properties.

## Lab defaults

The range's Mosquitto image (phenix-waterway-range, `docs/mqtt-broker.md`, bead ww-ko6) listens on 1883 anonymously, and on 8883 with
TLS when certificates are injected; its address in the generated topology is `10.1.1.60`.

```xml
<mqtt-client><endpoint>tcp://10.1.1.60:1883</endpoint><topic-prefix>waterway</topic-prefix>...</mqtt-client>
<mqtt-client><endpoint>ssl://10.1.1.60:8883</endpoint><tls-ca>/etc/ferrofire/ca.crt</tls-ca><username>ff</username><password>ffpass</password>...</mqtt-client>
```

Read it with `mosquitto_sub -h 10.1.1.60 -v -t 'waterway/#'`, command it with `mosquitto_pub -h 10.1.1.60 -q 1 -t waterway/rtu-1/setpoint/set -m 42`.

## Connection behaviour

One task per element; it only reads the datastore and queues writes, so a dead broker never delays the scan cycle. A failed or lost
connection is retried after 1 s, doubling to 30 s while the broker stays away (back to 1 s after a session that connected). Every
session registers its will in CONNECT, subscribes the command topics, then publishes the birth. Packets above 1 MiB are refused in
both directions; the client queue is bounded by `<queue>`.
