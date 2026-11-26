"""Unit tests for T7 cross-build W2 snapshot retention, disk bounds, and receipts."""
import json
from pathlib import Path
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parent))
from store_cross_build import promote, select_cuts
from vopr_nightly_receipt import summarize


class StoreCrossBuildTests(unittest.TestCase):
    def test_select_cuts_keeps_boundaries_and_bounds_count(self):
        self.assertEqual(select_cuts(5, 16), [0, 1, 2, 3, 4, 5])
        picked = select_cuts(100, 10)
        self.assertEqual(len(picked), 10)
        self.assertEqual(picked[0], 0)
        self.assertEqual(picked[1], 1)
        self.assertEqual(picked[-2], 99)
        self.assertEqual(picked[-1], 100)
        self.assertEqual(picked, sorted(set(picked)))

    def test_promote_replaces_last_green_and_enforces_cap(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            target = root / 'last-green-w2'
            stage1 = root / 'stage1'
            stage1.mkdir()
            (stage1 / 'manifest.json').write_text(json.dumps({'commit': 'aaa', 'cases': []}))
            (stage1 / 'payload.bin').write_bytes(b'x' * 100)
            self.assertEqual(promote(stage1, target, max_bytes=1024), 0)
            self.assertEqual(json.loads((target / 'manifest.json').read_text())['commit'], 'aaa')

            stage2 = root / 'stage2'
            stage2.mkdir()
            (stage2 / 'manifest.json').write_text(json.dumps({'commit': 'bbb', 'cases': []}))
            (stage2 / 'payload.bin').write_bytes(b'y' * 200)
            self.assertEqual(promote(stage2, target, max_bytes=1024), 0)
            self.assertEqual(json.loads((target / 'manifest.json').read_text())['commit'], 'bbb')
            # Only the last green set remains; no leftover backups.
            leftovers = [p.name for p in root.iterdir() if p.name.startswith('.last-green-w2')]
            self.assertEqual(leftovers, [])

            oversize = root / 'stage3'
            oversize.mkdir()
            (oversize / 'manifest.json').write_text(json.dumps({'commit': 'ccc', 'cases': []}))
            (oversize / 'big.bin').write_bytes(b'z' * 2048)
            with self.assertRaises(ValueError):
                promote(oversize, target, max_bytes=1024)
            # Failed promotion keeps the prior last-green set untouched.
            self.assertEqual(json.loads((target / 'manifest.json').read_text())['commit'], 'bbb')

    def test_receipt_records_cross_build_summary(self):
        with tempfile.TemporaryDirectory() as tmp:
            run = Path(tmp)
            manifest = run / 'manifest.json'
            manifest.write_text('[]')
            (run / 'cross-build.jsonl').write_text(
                json.dumps({'kind': 'case', 'id': 'disk-install-cut1', 'passed': False}) + '\n'
                + json.dumps({
                    'kind': 'summary', 'status': 'failed', 'saved_commit': 'abc',
                    'saved_format': 10, 'current_format': 10, 'checked': 1, 'failed': 1,
                }) + '\n'
            )
            summary = summarize(run, manifest, 'def')
            self.assertEqual(summary['cross_build_reopen']['failed'], 1)
            self.assertEqual(summary['cross_build_reopen']['saved_commit'], 'abc')


if __name__ == '__main__':
    unittest.main()
