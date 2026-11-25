"""Run with python3 core/tools/test_vopr_failure_groups.py."""
import unittest

from vopr_failure_groups import group


def red(stderr, law="store@1/store/law", status=101):
    return {"law": law, "seed": 7, "status": status, "stdout": "", "stderr": stderr}


class FailureGroups(unittest.TestCase):
    def test_partition(self):
        # Each case: rows that must share one group, or rows that must split.
        same = [
            # One Fail site whose message embeds a seed and an offset, plus a
            # quoted value and " at " inside the message.
            [red('Fail("seed 41 offset 3 key \\"a1\\" at x.hirsel:1:1") at store/journal.hirsel:88:9\n'),
             red('Fail("seed 977 offset 12 key \\"b22\\" at x.hirsel:1:1") at store/journal.hirsel:88:9\n')],
            # A non-cause diagnostic whose numbers vary.
            [red("bounded drain: 12 frames left after 300 ticks\n", status=1),
             red("bounded drain: 4 frames left after 1000 ticks\n", status=1)],
        ]
        split = [
            # Bare causes carry no message: the line is the only thing telling them apart.
            [red("Bounds at std@1/object.hirsel:2283:22\n"), red("Bounds at std@1/object.hirsel:2309:22\n")],
            # Two Fail sites in one file that share their text.
            [red('Fail("short read") at store/journal.hirsel:88:9\n'),
             red('Fail("short read") at store/journal.hirsel:140:9\n')],
            # Same site, different kind or law.
            [red("Overflow at a.hirsel:3:4\n"), red("Bounds at a.hirsel:3:4\n")],
            [red("Unwrap at a.hirsel:3:4\n", law="x"), red("Unwrap at a.hirsel:3:4\n", law="y")],
        ]
        for rows in same:
            self.assertEqual(len({group(row) for row in rows}), 1, rows)
        for rows in split:
            self.assertEqual(len({group(row) for row in rows}), len(rows), rows)


if __name__ == "__main__":
    unittest.main()
