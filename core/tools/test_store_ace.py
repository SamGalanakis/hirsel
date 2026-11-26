"""Unit tests for M #4 ACE/B3 <=3-op bounded-exhaustive workload enumeration."""
import json
from pathlib import Path
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parent))
from store_ace_tests import OPS, PreState, apply_precondition, enumerate_workloads
from vopr_nightly_receipt import summarize


class StoreAceTests(unittest.TestCase):
    def test_bounded_exhaustive_counts_and_well_formedness(self):
        workloads = enumerate_workloads(3)
        by_len = {}
        for seq in workloads:
            by_len[len(seq)] = by_len.get(len(seq), 0) + 1
            st = PreState()
            for op in seq:
                self.assertIn(op, OPS)
                st = apply_precondition(st, op)
                self.assertIsNotNone(st)
        self.assertEqual(len(OPS), 15)
        self.assertEqual(by_len, {1: 8, 2: 72, 3: 688})
        self.assertEqual(len(workloads), 768)
        # No duplicates in the enumeration.
        self.assertEqual(len(set(workloads)), len(workloads))
        # Key symmetry: no workload starts with put_b, del_a, unpin, put_branch, del_branch, trim, or sync.
        first_ops = {seq[0] for seq in workloads}
        self.assertEqual(first_ops, {'put_a', 'blob', 'cut', 'pin', 'fork', 'cell', 'log', 'schema'})

    def test_receipt_records_ace_enumeration_summary(self):
        with tempfile.TemporaryDirectory() as tmp:
            run = Path(tmp)
            manifest = run / 'manifest.json'
            manifest.write_text('[]')
            (run / 'ace.json').write_text(json.dumps({
                'alphabet_size': 15,
                'max_len': 3,
                'total_workloads': 785,
                'clean_cuts_run': 10000,
                'sector_cuts_run': 5000,
                'total_cuts_run': 15000,
                'failed': 1,
                'wall_seconds': 42.5,
            }))
            summary = summarize(run, manifest, 'abc')
            self.assertEqual(summary['ace_enumeration']['total_workloads'], 785)
            self.assertEqual(summary['ace_enumeration']['failed'], 1)


if __name__ == '__main__':
    unittest.main()
