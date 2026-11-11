#!/usr/bin/env python3
"""Elasticsearch/OpenSearch conformance for ferrofire (runs in the bennu:arm64 container, next to stock pybennu).

1. Per server (Elasticsearch 8, OpenSearch 2, Elasticsearch 8 with TLS + basic auth, the same with an API key): the provider's
   [elastic] export (template, mapping, document fields, frequency on every document) and `ferrofire groundtruth`
   (documents, index name) fed by the same stream.
2. Differential against the stock groundtruth_monitor.py (Elasticsearch 8 only: the stock script's elasticsearch-py client refuses
   OpenSearch): the same datagram stream to both, separate indices, compared document by document.
3. The intended differences, shown live: UTC (the stock monitor under TZ=NZT-13, UTC+13), the alert/batch limit.
4. TLS and auth are enforced: no CA file or a wrong password indexes nothing and says why.
"""
import json
import os
import socket
import struct
import sys
import time
from collections import defaultdict
from datetime import datetime, timezone

sys.path.insert(0, "/conf")
from lib import Es, check, finish, read, spawn, stop, utc_day, write  # noqa: E402
from stock_monitor import group  # noqa: E402  (the endpoint hash; importing it only defines the shims)

FERROFIRE = "/usr/local/bin/ferrofire"
CA = "/conf/http_ca.crt"
PASSWORD = "ffconf-Pass-1"
TAGS = ["pump.flow", "valve.flow", "tank.level", "grid.freq", "pump.on"]
OUT = "/out"


def run_provider(args, log="/tmp/provider.log"):
    return spawn([FERROFIRE, "run", "/conf/grid.yaml", "--server", "tcp://127.0.0.1:5555", "--rate", "0.1", *args], log)


def mapping_type(m, path):
    for k in path.split("."):
        m = m["properties"][k] if "properties" in m else m[k]
    return m["type"]


def leg(es, auth_prov, auth_gt):
    n = es.name
    day = utc_day()
    es.delete(f"ffprov-{n}-*")
    es.delete("bennu-*")
    prov_ini = write(f"/tmp/{n}-prov.ini", f"[elastic]\nenabled = true\nhost = {es.url}\nindex_basename = ffprov-{n}\nthreads = 2\nfrequency = grid.freq\nexperiment = conf\n{auth_prov}")
    gt_ini = write(f"/tmp/{n}-gt.ini", f"[power-groundtruth-monitor]\npublish-endpoint = udp://239.0.0.1:40000\nelasticsearch-ip = {es.url}\n{auth_gt}")
    gt = spawn([FERROFIRE, "groundtruth", "-c", gt_ini, "start"], f"/tmp/{n}-gt.log")
    time.sleep(1)
    pv = run_provider(["--publish", "udp://*;239.0.0.1:40000", "--config", prov_ini], f"/tmp/{n}-prov.log")
    time.sleep(10)
    check(f"{n}: provider and monitor stop cleanly on SIGTERM", (stop(pv), stop(gt)) == (0, 0), read(f"/tmp/{n}-prov.log")[-300:] + read(f"/tmp/{n}-gt.log")[-300:])

    # provider export
    index = f"ffprov-{n}-{day}"
    docs = es.docs(index)
    check(f"{n}: provider export wrote documents to {index}", len(docs) >= 100, f"{len(docs)} docs")
    code, tpl = es.call("GET", f"/_index_template/ffprov-{n}")
    check(f"{n}: the index template is installed", code == 200 and tpl["index_templates"][0]["index_template"]["index_patterns"] == [f"ffprov-{n}-*"], str(tpl)[:200])
    code, mp = es.call("GET", f"/{index}/_mapping")
    props = mp[index]["mappings"]
    types = {p: mapping_type(props, p) for p in ["@timestamp", "sceptre_time", "simulator_time", "time_drift", "event.ingested", "measurement.frequency", "groundtruth.tag", "groundtruth.float", "groundtruth.bool", "observer.name", "sceptre.provider"]}
    want = {"@timestamp": "date", "sceptre_time": "date", "simulator_time": "date", "time_drift": "double", "event.ingested": "date", "measurement.frequency": "double", "groundtruth.tag": "keyword", "groundtruth.float": "double", "groundtruth.bool": "boolean", "observer.name": "keyword", "sceptre.provider": "keyword"}
    check(f"{n}: the template's mapping applied to the daily index", types == want, str(types))
    d = next(x for x in docs if x["groundtruth"]["tag"] == "tank.level")
    need = ["@timestamp", "sceptre_time", "simulator_time", "time_drift", "event", "ecs", "agent", "observer", "sceptre", "groundtruth", "measurement"]
    check(f"{n}: a provider document has the pybennu fields", all(k in d for k in need) and d["ecs"]["version"] == "8.11.0" and d["sceptre"]["experiment"] == "conf", json.dumps(d)[:300])
    check(f"{n}: measurement.frequency is on every provider document", all(x.get("measurement", {}).get("frequency") == 60.0 for x in docs))
    seen = {x["groundtruth"]["tag"] for x in docs}
    check(f"{n}: every published tag is exported", seen == set(TAGS), str(seen))
    bools = [x["groundtruth"] for x in docs if x["groundtruth"]["tag"] == "pump.on"]
    check(f"{n}: a binary tag is typed bool", all(g["bool"] is True and g["type"] == "bool" for g in bools))
    drifts = [x["time_drift"] for x in docs]
    check(f"{n}: time_drift is small and non-negative (ms)", min(drifts) >= 0 and max(drifts) < 1000, f"max {max(drifts)}")

    # groundtruth monitor
    g = es.docs(f"bennu-{day}")
    check(f"{n}: groundtruth monitor wrote documents to bennu-{day}", len(g) >= 100, f"{len(g)} docs")
    check(f"{n}: groundtruth documents have exactly the stock fields", all({"@timestamp", "provider", "device", "field"} < set(x) <= {"@timestamp", "provider", "device", "field", "value", "status"} and ("value" in x) != ("status" in x) for x in g))
    check(f"{n}: the datagram trailer is not indexed", not any(x["device"].startswith("_ff") for x in g))
    devs = {(x["device"], x["field"]) for x in g}
    check(f"{n}: groundtruth saw every tag as device.field", devs == {("pump", "flow"), ("valve", "flow"), ("tank", "level"), ("grid", "freq"), ("pump", "on")}, str(devs))
    check(f"{n}: a binary tag is a status string, a number a float", all(x["status"] == "true" for x in g if x["device"] == "pump" and x["field"] == "on") and all(isinstance(x["value"], float) for x in g if x["device"] == "tank"))
    check(f"{n}: groundtruth provider field is the publish endpoint", {x["provider"] for x in g} == {"udp://239.0.0.1:40000"})
    ts = datetime.strptime(g[-1]["@timestamp"], "%Y-%m-%dT%H:%M:%S.%fZ").replace(tzinfo=timezone.utc)
    check(f"{n}: @timestamp is UTC and current", abs((datetime.now(timezone.utc) - ts).total_seconds()) < 60, g[-1]["@timestamp"])
    os.makedirs(OUT, exist_ok=True)
    write(f"{OUT}/{n}-provider-doc.json", json.dumps(d, indent=1))
    write(f"{OUT}/{n}-groundtruth-doc.json", json.dumps(g[-1], indent=1))


def count_by_tag(docs):
    out = defaultdict(list)
    for x in docs:
        out[(x["device"], x["field"])].append(x)
    return out


def parse_ts(s):
    return datetime.fromisoformat(s.replace("Z", "+00:00")).replace(tzinfo=None) if s.endswith("Z") else datetime.fromisoformat(s)


def datagram(endpoint, body):
    g = group(endpoint)
    return bytes([len(g)]) + g.encode() + body.encode()


def send(endpoint, body, n=1):
    host, port = endpoint.removeprefix("udp://").split(":")
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_LOOP, 1)
    s.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_TTL, 1)
    for _ in range(n):
        s.sendto(datagram(endpoint, body), (host, int(port)))
        time.sleep(0.1)


def differential(es):
    day, es_ = utc_day(), es
    es.delete("bennu-*")
    es.delete("ff-bennu-*")
    stock_ep, ours_ep = "udp://239.0.0.1:40001", "udp://239.0.0.1:40002"
    stock_ini = write("/tmp/stock.ini", f"[power-groundtruth-monitor]\npublish-endpoint = {stock_ep}\nelasticsearch-ip = es8\n")
    ours_ini = write("/tmp/ours.ini", f"[power-groundtruth-monitor]\npublish-endpoint = {ours_ep}\nelasticsearch-ip = {es.url}\nindex-basename = ff-bennu\n")
    stock = spawn(["python3", "/conf/stock_monitor.py", "-c", stock_ini, "start"], "/tmp/stock.log")
    ours = spawn([FERROFIRE, "groundtruth", "-c", ours_ini, "start"], "/tmp/ours.log")
    time.sleep(3)
    pv = run_provider(["--publish", stock_ep, "--publish-tags", f"{ours_ep}={','.join(TAGS)}"], "/tmp/dprov.log")
    time.sleep(12)
    stop(pv)
    # the intended difference in the alert/batch limit: one message, an alert point in the middle (both are in their first 30 s)
    for ep in (stock_ep, ours_ep):
        send(ep, "gt.before:1,alert.high:2,gt.after:3,batch.n:4,gt.last:5,\0")
    time.sleep(3)
    stop(ours)
    stock.terminate()
    stock.wait(timeout=10)
    check("differential: both monitors ran without a traceback", "Traceback" not in read("/tmp/stock.log") and "Traceback" not in read("/tmp/ours.log"), read("/tmp/stock.log")[-400:])

    a, b = es.docs(f"bennu-{day}"), es.docs(f"ff-bennu-{day}")
    check("differential: both wrote documents", len(a) > 200 and len(b) > 200, f"stock {len(a)}, ferrofire {len(b)}")
    trailer = [x for x in a if x["device"] == "_ff"]
    a_data = [x for x in a if x["device"] != "_ff" and not x["device"].startswith(("gt", "alert", "batch"))]
    b_data = [x for x in b if not x["device"].startswith(("gt", "alert", "batch"))]
    # intended difference 1: the stock monitor indexes the datagram v1 trailer (_ff.seq/_ff.cyc/_ff.t) as data
    check("difference: stock indexes the _ff trailer, ferrofire does not", len(trailer) > 0 and not any(x["device"] == "_ff" for x in b), f"{len(trailer)} stock _ff docs")
    A, B = count_by_tag(a_data), count_by_tag(b_data)
    check("differential: the same tags", set(A) == set(B) and len(A) == len(TAGS), f"{sorted(A)} vs {sorted(B)}")
    problems, compared = [], 0
    for tag in sorted(A):
        xs, ys = A[tag], B[tag]
        if len(xs) != len(ys):
            problems.append(f"{tag}: {len(xs)} stock docs vs {len(ys)} ferrofire docs")
        for x, y in zip(xs, ys):
            compared += 1
            if set(x) != set(y):
                problems.append(f"{tag}: fields {sorted(x)} vs {sorted(y)}")
            elif {k: v for k, v in x.items() if k not in ("@timestamp", "provider")} != {k: v for k, v in y.items() if k not in ("@timestamp", "provider")}:
                problems.append(f"{tag}: {x} vs {y}")
            elif abs((parse_ts(x["@timestamp"]) - parse_ts(y["@timestamp"])).total_seconds()) > 0.5:
                problems.append(f"{tag}: timestamps {x['@timestamp']} vs {y['@timestamp']}")
            if x["provider"] != stock_ep or y["provider"] != ours_ep:
                problems.append(f"{tag}: provider {x['provider']} vs {y['provider']}")
    check(f"differential: {compared} documents equal field by field (value, type, device, field; timestamps within 0.5 s)", compared > 200 and not problems, "; ".join(problems[:5]))
    # intended difference 2: the alert/batch limit
    sa = sorted(x["device"] for x in a if x["device"].startswith(("gt", "alert", "batch")))
    sb = sorted(x["device"] for x in b if x["device"].startswith(("gt", "alert", "batch")))
    check("difference: stock drops the rest of the message from the first alert/batch point", sa == ["gt"], str(sa))
    check("difference: ferrofire drops only the alert/batch points that exceed the limit", sb == ["alert", "batch", "gt", "gt", "gt"], str(sb))
    write(f"{OUT}/differential.txt", f"stock docs {len(a)} (of which _ff trailer {len(trailer)}), ferrofire docs {len(b)}\ncompared {compared} data documents: {'identical' if not problems else problems[:20]}\nstock alert/batch message -> {sa}\nferrofire alert/batch message -> {sb}\n")
    write(f"{OUT}/stock-doc.json", json.dumps(a_data[-1], indent=1))
    write(f"{OUT}/ferrofire-doc.json", json.dumps(b_data[-1], indent=1))


def utc_difference(es):
    es.delete("bennu-*")
    es.delete("ff-bennu-*")
    ep, ep2 = "udp://239.0.0.1:40003", "udp://239.0.0.1:40004"
    ini = write("/tmp/tz.ini", f"[power-groundtruth-monitor]\npublish-endpoint = {ep}\nelasticsearch-ip = es8\n")
    ini2 = write("/tmp/tz2.ini", f"[power-groundtruth-monitor]\npublish-endpoint = {ep2}\nelasticsearch-ip = {es.url}\nindex-basename = ff-bennu\n")
    env = {"TZ": "NZT-13"}  # POSIX TZ string, UTC+13 (the image has no tzdata for Pacific/Auckland)
    stock = spawn(["python3", "/conf/stock_monitor.py", "-c", ini, "start"], "/tmp/tzs.log", env)
    ours = spawn([FERROFIRE, "groundtruth", "-c", ini2, "start"], "/tmp/tzo.log", env)
    time.sleep(2)
    send(ep, "tz.a:1,", 5)
    send(ep2, "tz.a:1,", 5)
    time.sleep(3)
    stop(ours)
    stock.terminate()
    stock.wait(timeout=10)
    now = datetime.now(timezone.utc).replace(tzinfo=None)
    idx = es.call("GET", "/_cat/indices/bennu-*,ff-bennu-*?format=json")[1]
    names = sorted(i["index"] for i in idx)
    stock_docs = [d for i in names if i.startswith("bennu-") for d in es.docs(i)]
    ours_docs = [d for i in names if i.startswith("ff-bennu-") for d in es.docs(i)]
    if check("utc: both monitors wrote under TZ=NZT-13", stock_docs and ours_docs, str(names)):
        s_off = (parse_ts(stock_docs[-1]["@timestamp"]) - now).total_seconds() / 3600
        o_off = (parse_ts(ours_docs[-1]["@timestamp"]) - now).total_seconds() / 3600
        check("difference: the stock @timestamp is local time (about +13 h from UTC)", 12 < s_off < 14, f"{s_off:.2f} h")
        check("difference: ferrofire @timestamp is UTC whatever TZ is", abs(o_off) < 0.1, f"{o_off:.2f} h")
        want = f"ff-bennu-{utc_day()}"
        check("difference: ferrofire's index day is the UTC day", want in names, f"{names}, wanted {want}")


def enforced(secure, good_user):
    es_ = secure
    day = utc_day()
    es_.delete("bennu-*")
    cases = {
        "no CA file": f"elasticsearch-ip = {es_.url}\nelasticsearch-user = {good_user}\nelasticsearch-password = {PASSWORD}\n",
        "wrong password": f"elasticsearch-ip = {es_.url}\nelasticsearch-user = {good_user}\nelasticsearch-password = nope\nelasticsearch-ca-file = {CA}\n",
        "no credentials": f"elasticsearch-ip = {es_.url}\nelasticsearch-ca-file = {CA}\n",
    }
    for name, keys in cases.items():
        ep = "udp://239.0.0.1:40005"
        ini = write("/tmp/bad.ini", f"[groundtruth-monitor]\npublish-endpoint = {ep}\n{keys}")
        gt = spawn([FERROFIRE, "groundtruth", "-c", ini, "start"], "/tmp/bad.log")
        time.sleep(1)
        send(ep, "bad.a:1,", 8)
        time.sleep(2)
        stop(gt)
        log = read("/tmp/bad.log")
        check(f"enforced: {name}: nothing is indexed", es_.count(f"bennu-{day}") == 0)
        check(f"enforced: {name}: the failure is logged", "bulk request failed" in log, log[-300:])


def main():
    es8, os2 = Es("es8", "http://es8:9200"), Es("os2", "http://os2:9200")
    basic = Es("essec-basic", "https://essec:9200", user="elastic", password=PASSWORD, ca=CA)
    for e in (es8, os2, basic):
        check(f"{e.name} is up", e.wait_ready())
    code, key = basic.call("POST", "/_security/api_key", {"name": "ferrofire"})
    keyed = Es("essec-key", "https://essec:9200", api_key=key["encoded"], ca=CA)
    leg(es8, "", "")
    leg(os2, "", "")
    leg(basic, f"user = elastic\npassword = {PASSWORD}\nca-file = {CA}\n", f"elasticsearch-user = elastic\nelasticsearch-password = {PASSWORD}\nelasticsearch-ca-file = {CA}\n")
    leg(keyed, f"api-key = {key['encoded']}\nca-file = {CA}\n", f"elasticsearch-api-key = {key['encoded']}\nelasticsearch-ca-file = {CA}\n")
    differential(es8)
    utc_difference(es8)
    enforced(basic, "elastic")
    finish()


main()
