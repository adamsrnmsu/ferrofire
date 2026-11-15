"""Tests for bench/maxrate/gen.py (ww-1df.43): python3 -m unittest bench/test_maxrate_gen.py (from the repo root: python3 -m unittest discover -s bench -p 'test_maxrate*.py')."""
import os, re, sys, tempfile, unittest

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "maxrate"))
import gen  # noqa: E402


def cell(model, r, dt=5, pub=5):
    d = tempfile.mkdtemp()
    gen.main(model, r, dt, pub, d)
    return d, open(f"{d}/model.yaml").read()


class Gen(unittest.TestCase):
    def test_every_tag_once_and_counter_only_on_rtu1(self):
        for model, n in (("S", 15), ("M", 800)):
            d, m = cell(model, 3)
            names = re.findall(r"^  (bench\.[\w.]+):", m.split("outputs:")[1].split("publish_slices:")[0], re.M)
            self.assertEqual(len(names), n)
            slices = [re.findall(r"bench\.\w+", t) for t in re.findall(r"tags: \[(.*)\]", m)]
            self.assertEqual(sorted(sum(slices, [])), sorted(names))
            self.assertIn("bench.cnt", slices[0])
            self.assertNotIn("bench.cnt", sum(slices[1:], []))
            self.assertEqual(open(f"{d}/rtu-1.xml").read().count("<address>39000</address>"), 1)
            self.assertNotIn("39000", open(f"{d}/rtu-2.xml").read())

    def test_tick_counter_steps_one_per_tick(self):
        _, m = cell("S", 1, dt=2, pub=10)
        self.assertIn('bench.k: {ode: "500", init: 0}', m)
        self.assertIn("dt: 0.002", m)
        self.assertIn("publish_rate: 0.01", m)

    def test_grid_has_counter_and_all_devices(self):
        d, m = cell("G", 2)
        self.assertIn("bench.cnt", re.findall(r"tags: \[(.*?)\]", m)[0])
        self.assertTrue(os.path.exists(f"{d}/rtu-1.xml") and os.path.exists(f"{d}/rtu-2.xml"))
        self.assertIn("<address>39000</address>", open(f"{d}/rtu-1.xml").read())


if __name__ == "__main__":
    unittest.main()
