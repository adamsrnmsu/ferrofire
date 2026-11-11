#!/usr/bin/env python3
"""Run the SCORCH `providerdata` component's own check (sceptre-phenix-apps, ProviderData.start()) against ferrofire's provider export.

Its Elasticsearch part: today's index must hold `min_documents`; `min_new_documents` must arrive within `doc_count_wait`;
`measurement.frequency` of the newest document of the last 5 s must be within `frequency_tolerance` of `expected_frequency`. The class
is the real one (imported from the phenix_apps source copied to /phenix_apps); only the two things that need a phenix VM are stood
in for: the "is the provider process running" check. For OpenSearch and the secured Elasticsearch the elasticsearch-py client the
component builds cannot be used (it refuses OpenSearch, and the component passes it no credentials), so those get a client with the
same two calls the component makes (`indices.stats` and `search`) over HTTP: the component's code and queries run unchanged.
Negative controls prove the check can fail: a wrong frequency, too few documents, too few new documents, a stopped provider.
"""
import sys
import time

sys.path.insert(0, "/")
sys.path.insert(0, "/conf")
from lib import Es, check, finish, spawn, stop, write  # noqa: E402
from loguru import logger  # noqa: E402

logger.remove()  # the component logs every step at INFO; keep the verdicts readable
logger.add(sys.stderr, level="ERROR")
from phenix_apps.apps.scorch.providerdata.providerdata import ProviderData, ProviderDataMetadata  # noqa: E402

FERROFIRE = "/usr/local/bin/ferrofire"
CA = "/conf/http_ca.crt"
PASSWORD = "ffconf-Pass-1"


class Indices:
    def __init__(self, es):
        self.es = es

    def stats(self, index):
        code, r = self.es.call("GET", f"/{index}/_stats")
        if code != 200:
            raise RuntimeError(f"_stats {index}: HTTP {code} {r}")
        return r


class HttpClient:
    """The two calls providerdata makes, over HTTP, returning the JSON the elasticsearch-py client would."""

    def __init__(self, es):
        self.es, self.indices = es, Indices(es)

    def search(self, index, size, sort, query):
        code, r = self.es.call("POST", f"/{index}/_search", {"size": size, "sort": sort, "query": query})
        if code != 200:
            raise RuntimeError(f"_search {index}: HTTP {code} {r}")
        return r


def component(es, index, client, **elastic):
    c = ProviderData.__new__(ProviderData)
    c.name = "providerdata"
    c._es = client
    c.metadata = {"elasticsearch": {"server": es.url}}
    c.check_process_running = lambda host, proc: True  # needs a phenix VM
    c.config = ProviderDataMetadata.model_validate(
        {"hostname": "provider", "elasticsearch": {"verify": True, "server": es.url, "index": index, **elastic}}
    )
    return c


def outcome(c):
    try:
        c.start()
        return None
    except Exception as e:  # noqa: BLE001
        return str(e)


def target(name, es, auth_keys, client):
    index = f"scorch-{name}"
    es.delete(f"{index}-*")  # a fresh index each run, created by the provider's template
    ini = write(
        f"/tmp/scorch-{name}.ini",
        f"[elastic]\nenabled = true\nhost = {es.url}\nindex_basename = {index}\nthreads = 2\nfrequency = grid.freq\n{auth_keys}",
    )
    log = f"/tmp/scorch-{name}.log"
    p = spawn([FERROFIRE, "run", "/conf/grid.yaml", "--server", "tcp://127.0.0.1:5555", "--publish", "udp://*;239.0.0.1:40010", "--rate", "0.1", "--config", ini], log)
    time.sleep(8)
    ok = dict(min_documents=50, min_new_documents=20, doc_count_wait=3, expected_frequency=60)
    check(f"{name}: providerdata start() passes against ferrofire", (err := outcome(component(es, index, client, **ok))) is None, err)
    bad = outcome(component(es, index, client, **{**ok, "expected_frequency": 50}))
    check(f"{name}: a wrong expected_frequency fails", bad is not None and "frequency out of bounds, expected 50" in bad, bad)
    bad = outcome(component(es, index, client, **{**ok, "min_documents": 10**9}))
    check(f"{name}: too few documents fails", bad is not None and "documents in" in bad and "expected at least" in bad, bad)
    bad = outcome(component(es, index, client, **{**ok, "min_new_documents": 10**8}))
    check(f"{name}: too few new documents fails", bad is not None and "new documents" in bad, bad)
    stop(p)
    time.sleep(6)  # nothing is arriving any more
    bad = outcome(component(es, index, client, **{**ok, "min_new_documents": 1}))
    check(f"{name}: a stopped provider fails", bad is not None and "new documents" in bad, bad)
    bad = outcome(component(es, index, client, **{"min_documents": 50, "expected_frequency": 60}))
    check(f"{name}: no documents in the last 5 s fails the frequency check", bad is not None and "last 5 seconds" in bad, bad)


def main():
    es8 = Es("es8", "http://es8:9200")
    os2 = Es("os2", "http://os2:9200")
    basic = Es("essec", "https://essec:9200", user="elastic", password=PASSWORD, ca=CA)
    for e in (es8, os2, basic):
        check(f"{e.name} is up", e.wait_ready())
    code, key = basic.call("POST", "/_security/api_key", {"name": "ferrofire-scorch"})
    keyed = Es("essec-key", "https://essec:9200", api_key=key["encoded"], ca=CA)
    target("es8", es8, "", None)  # None: the component builds its own elasticsearch-py client (utils.connect_elastic)
    target("os2", os2, "", HttpClient(os2))
    target("essec-basic", basic, f"user = elastic\npassword = {PASSWORD}\nca-file = {CA}\n", HttpClient(basic))
    target("essec-key", keyed, f"api-key = {key['encoded']}\nca-file = {CA}\n", HttpClient(keyed))
    finish()


main()
