#!/usr/bin/env python3
"""python3 bench/test_control_join.py : checks of bench/control_join.py."""
import os, sys, unittest
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import control_join as J

LOG = """Info: ferrofire run: x
Applied: t_apply_ns=100 t_solved_ns=110 tag=b1.active val=false
Applied: t_apply_ns=100 t_solved_ns=110 tag=b2.active val=false
Applied: t_apply_ns=300 t_solved_ns=312 tag=b1.active val=true
Applied: t_apply_ns=500 t_solved_ns=509 tag=b1.active val=false
"""


def row(tag, to, t_cmd):
    return {"tag": tag, "to": str(to), "t_cmd": str(t_cmd)}


class T(unittest.TestCase):
    def test_each_command_takes_the_first_unused_stamp_of_its_value_after_it_was_sent(self):
        rows = J.join([row("b1.active", 0, 90), row("b1.active", 1, 200), row("b1.active", 0, 400), row("b2.active", 0, 90)], J.stamps(LOG.splitlines()))
        self.assertEqual([(r["t_apply_ns"], r["t_solved_ns"]) for r in rows], [(100, 110), (300, 312), (500, 509), (100, 110)])

    def test_no_stamp_or_unsent_means_empty_and_a_stamp_is_not_used_twice(self):
        st = J.stamps(LOG.splitlines())
        rows = J.join([row("b1.active", 0, 90), row("b1.active", 0, 91), row("b1.active", 0, 0), row("nope", 0, 5), row("b2.active", 1, 5)], st)
        got = {(r["tag"], r["to"], r["t_cmd"]): r["t_apply_ns"] for r in rows}
        self.assertEqual(got[("b1.active", "0", "90")], 100)
        self.assertEqual(got[("b1.active", "0", "91")], 500)  # the 100 stamp is taken; the next false is 500
        self.assertEqual((got[("b1.active", "0", "0")], got[("nope", "0", "5")], got[("b2.active", "1", "5")]), ("", "", ""))

    def test_a_stamp_long_after_the_command_belongs_to_a_later_one(self):
        st = J.stamps(["Applied: t_apply_ns=1000 t_solved_ns=1001 tag=b.active val=false"])
        self.assertEqual(J.join([row("b.active", 0, 10)], st, cap=500)[0]["t_apply_ns"], "")
        self.assertEqual(J.join([row("b.active", 0, 10)], st, cap=990)[0]["t_apply_ns"], 1000)


if __name__ == "__main__":
    unittest.main()
