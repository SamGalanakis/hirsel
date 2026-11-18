#!/usr/bin/env python3
"""The merge driver keeps both lanes' additions and either side's removals."""
from pathlib import Path
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'buck2'))
import merging


class LineSets(unittest.TestCase):
    def test_every_row_obeys_three_way_set_membership(self):
        # Exhaust all eight base/ours/theirs membership combinations, including
        # deletion against unchanged, shared additions and both deletions.
        from itertools import product
        combinations = list(product([False, True], repeat=3))
        texts = [''.join(f'row{i}\n' for i, flags in enumerate(combinations) if flags[side])
                 for side in range(3)]
        expected = {f'row{i}' for i, (base, ours, theirs) in enumerate(combinations)
                    if (base and ours and theirs) or (ours and not base) or (theirs and not base)}
        self.assertEqual(set(merging.resolve('crates/example/tests/it/modules.rs', *texts).splitlines()), expected)

    def test_unsorted_lines_keep_order_and_normalize_missing_final_newline(self):
        self.assertEqual(merging.resolve('crates/example/tests/it/modules.rs', 'R\nkeep',
                                         'keep\nzed', 'R\nkeep\nalpha'), 'keep\nzed\nalpha\n')


if __name__ == '__main__':
    unittest.main()
