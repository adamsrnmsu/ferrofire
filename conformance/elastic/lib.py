"""Shared by check.py (in the bennu:arm64 container) and scorch_check.py (in the Python 3.12 one): a tiny HTTP client for
Elasticsearch/OpenSearch (stdlib only), a pass/fail recorder, and the way the provider and the monitor are started."""
import base64
import json
import os
import signal
import ssl
import subprocess
import sys
import time
import urllib.error
import urllib.request

FAILED = []


def check(name, ok, detail=""):
    print(("PASS " if ok else "FAIL ") + name + ("" if ok or not detail else f": {detail}"), flush=True)
    if not ok:
        FAILED.append(name)
    return ok


def finish():
    print(f"\n{len(FAILED)} failed" if FAILED else "\nall checks passed", flush=True)
    sys.exit(1 if FAILED else 0)


class Es:
    """One server: url, optional CA file, and basic or API-key auth."""

    def __init__(self, name, url, user=None, password=None, api_key=None, ca=None):
        self.name, self.url, self.ca = name, url.rstrip("/"), ca
        self.user, self.password, self.api_key = user, password, api_key
        self.ctx = ssl.create_default_context(cafile=ca) if url.startswith("https") else None

    def headers(self):
        if self.api_key:
            return {"Authorization": "ApiKey " + self.api_key}
        if self.user:
            return {"Authorization": "Basic " + base64.b64encode(f"{self.user}:{self.password}".encode()).decode()}
        return {}

    def call(self, method, path, body=None):
        data = None if body is None else json.dumps(body).encode()
        req = urllib.request.Request(self.url + path, data=data, method=method, headers={"Content-Type": "application/json", **self.headers()})
        try:
            with urllib.request.urlopen(req, timeout=30, context=self.ctx) as r:
                return r.status, json.loads(r.read() or b"{}")
        except urllib.error.HTTPError as e:
            return e.code, json.loads(e.read() or b"{}")

    def wait_ready(self, secs=240):
        end = time.time() + secs
        while time.time() < end:
            try:
                if self.call("GET", "/_cluster/health?wait_for_status=yellow&timeout=5s")[0] == 200:
                    return True
            except Exception:
                pass
            time.sleep(3)
        return False

    def docs(self, index):
        self.call("POST", f"/{index}/_refresh")
        code, r = self.call("POST", f"/{index}/_search", {"size": 10000, "sort": [{"@timestamp": "asc"}]})
        return [h["_source"] for h in r["hits"]["hits"]] if code == 200 else []

    def count(self, index):
        self.call("POST", f"/{index}/_refresh")
        code, r = self.call("GET", f"/{index}/_count")
        return r.get("count", 0) if code == 200 else 0

    def delete(self, pattern):
        """Delete the indices matching a wildcard (the servers refuse a wildcard DELETE by default: delete by name)."""
        code, r = self.call("GET", f"/_cat/indices/{pattern}?format=json")
        for i in r if code == 200 else []:
            self.call("DELETE", f"/{i['index']}")


def utc_day():
    return time.strftime("%Y.%m.%d", time.gmtime())


def spawn(argv, log, env=None):
    return subprocess.Popen(argv, stdout=open(log, "w"), stderr=subprocess.STDOUT, env={**os.environ, **(env or {})})


def stop(p):
    """SIGTERM and wait; returns the process's exit code."""
    p.send_signal(signal.SIGTERM)
    try:
        return p.wait(timeout=30)
    except subprocess.TimeoutExpired:
        p.kill()
        return p.wait()


def write(path, text):
    with open(path, "w") as f:
        f.write(text)
    return path


def read(path):
    with open(path) as f:
        return f.read()
