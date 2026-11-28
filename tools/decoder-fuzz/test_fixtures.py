#!/usr/bin/env python3
"""The native cohorts' embedded projections: their format and that each is its bundle's."""
import unittest

import fixtures


class Projections(unittest.TestCase):
    def test_rows_follow_the_documented_format(self):
        # Written out from the bundles by hand: a whole-input case with an empty
        # projection, a streaming case and a selected surface with two schedules.
        self.assertIn("refused-syntax-trailing-comma syntax 0 5b312c5d - -\n", fixtures.native_rows("json"))
        self.assertIn("partial-header-only partial 0 010000ed - 0004:1,00030004:2\n", fixtures.native_rows("quic-tls"))
        self.assertIn("partial-decoder-open-integer partial 1 447f 636f6e73756d65643d31 0002:1,00010002:1\n",
                      fixtures.native_rows("qpack"))
        self.assertIn("refused-two-fields-over-size-limit too-large 3 0000d1d7 - 0004:0\n",
                      fixtures.native_rows("qpack"))

    def test_owner_constants_are_current(self):
        for stem, cohort in fixtures.NATIVE.items():
            with self.subTest(stem):
                lines = (fixtures.ROOT / cohort["source"]).read_text().split("\n")
                self.assertIn(fixtures.project(stem, False), lines,
                              f"stale {cohort['constant']}: run tools/decoder-fuzz/fixtures.py rows {stem} --write")


if __name__ == "__main__":
    unittest.main()
