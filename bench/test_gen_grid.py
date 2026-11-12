#!/usr/bin/env python3
"""python3 bench/test_gen_grid.py : checks of bench/gen_grid.py (sizes, partition, determinism, connectivity, config consistency)."""
import json, os, pathlib, sys, tempfile, unittest
import xml.etree.ElementTree as ET
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import gen_grid as G

RTUS = [1, 2, 5, 10]


class T(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.g = G.build(25, 10, 1)
        cls.tmp = tempfile.TemporaryDirectory()
        G.emit(cls.g, cls.tmp.name, RTUS)

    @classmethod
    def tearDownClass(cls):
        cls.tmp.cleanup()

    def path(self, *p):
        return os.path.join(self.tmp.name, *p)

    def test_sizes(self):
        g = self.g
        self.assertEqual((g["nbus"], len(g["branches"]), len(g["gen_buses"])), (250, 1250, 10))
        self.assertGreaterEqual(len(g["flip_order"]), 1000)
        self.assertEqual(len({(f, t) for f, t, *_ in g["branches"]}), 1250)

    def test_opening_every_flip_keeps_the_grid_connected(self):
        g, up = self.g, list(range(251))
        def find(a):
            while up[a] != a:
                up[a] = up[up[a]]; a = up[a]
            return a
        flip = set(g["flip_order"])
        for k, (f, t, *_) in enumerate(g["branches"]):
            if k not in flip:
                up[find(f)] = find(t)
        self.assertEqual(len({find(b) for b in range(1, 251)}), 1)

    def test_determinism_and_seed(self):
        a, b, c = G.build(25, 10, 1), G.build(25, 10, 1), G.build(25, 10, 2)
        self.assertEqual(a, b)
        self.assertEqual(G.case_py(a), G.case_py(b))
        self.assertNotEqual(a["flip_order"], c["flip_order"])

    def test_partition_round_robin_and_complete(self):
        for nr in RTUS:
            parts = G.partition(self.g, nr)
            self.assertEqual(sorted(k for p in parts for k in p["breakers"]), list(range(1250)))
            self.assertEqual(sorted(j for p in parts for j in p["gens"]), list(range(10)))
            for r, p in enumerate(parts):
                self.assertTrue(all(k % nr == r for k in p["breakers"]))
                self.assertTrue(all(j % nr == r for j in p["gens"]))
                self.assertLessEqual(max(len(parts[0]["breakers"]) - len(q["breakers"]) for q in parts), 1)

    def test_case_py_rows(self):
        s = pathlib.Path(self.path("case.py")).read_text()
        for name, n in (("bus", 250), ("gen", 11), ("branch", 1250)):
            body = s.split(f'ppc["{name}"] = array([')[1].split("])")[0]
            rows = [r for r in body.split("\n") if r.strip().startswith("[")]
            self.assertEqual(len(rows), n, name)

    def test_fd_configs_match_scada_map(self):
        for nr in RTUS:
            sc = json.loads(pathlib.Path(self.path(f"R{nr}", "scada.json")).read_text())
            self.assertEqual(len(sc["rtus"]), nr)
            self.assertEqual(sum(len(r["breakers"]) for r in sc["rtus"]), 1250)
            for stack in ("ferrofire", "bennu"):
                for m in sc["rtus"]:
                    root = ET.parse(self.path(f"R{nr}", stack, m["name"] + ".xml")).getroot()
                    pts = {}
                    for sec in ("input", "output"):
                        for e in root.find(f"field-device/{sec}"):
                            if e.tag in ("binary", "analog"):
                                pts[(sec, e.findtext("id"))] = e.findtext("name")
                    tag = {t.findtext("name"): pts[("input" if t.findtext("io")[0] == "I" else "output", t.findtext("io"))]
                           for t in root.findall("field-device/tags/external-tag")}
                    addr = {}
                    for e in root.find("field-device/comms/modbus-server"):
                        if e.tag != "endpoint" and e.tag != "event-logging":
                            addr[(e.tag, int(e.findtext("address")))] = tag[e.findtext("tag")]
                    for b in m["breakers"]:
                        self.assertEqual(addr[("coil", b["coil"])], b["point"])
                        self.assertEqual(addr[("discrete-input", b["di"])], b["point"])
                    for e in m["gens"]:
                        self.assertEqual(addr[("coil", e["coil"])], e["status_point"])
                        self.assertEqual(addr[("holding-register", e["hr"])], e["setpoint_point"])
                        self.assertEqual(addr[("input-register", e["ir"])], e["pg_point"])
                    for a in m["aux"]:
                        self.assertEqual(addr[("input-register", a["ir"])], a["point"])

    def test_ferrofire_slices_carry_every_input_point_and_stock_shares_one_group(self):
        for nr in RTUS:
            y = pathlib.Path(self.path(f"R{nr}", "model.yaml")).read_text()
            self.assertEqual(y.count("- endpoint:"), nr)
            for r in range(nr):
                ff = pathlib.Path(self.path(f"R{nr}", "ferrofire", f"rtu-{r + 1}.xml")).read_text()
                st = pathlib.Path(self.path(f"R{nr}", "bennu", f"rtu-{r + 1}.xml")).read_text()
                self.assertIn(f"udp://*;239.0.1.{r + 1}:40000", ff)
                self.assertIn("udp://*;239.0.0.1:40000", st)
                self.assertEqual(ff.replace(f"239.0.1.{r + 1}", "239.0.0.1"), st)
                for pt in ET.fromstring(ff).findall("field-device/input/*/name"):
                    self.assertIn(pt.text, y)


if __name__ == "__main__":
    unittest.main()
