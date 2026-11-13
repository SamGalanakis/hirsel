"""Source-line bookkeeping must also correct reused body-edit samples."""
import argparse
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import run


class BuildDenominatorTests(unittest.TestCase):
    def test_reused_body_edit_counts_inserted_line_without_sampling(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            tools = {'compiler_build_profile': 'release', 'runtime_archive_profile': 'shipping-release-fat-lto'}
            previous = {'module': 'base64', 'backend': 'llvm', 'toolchain': tools,
                        'source_lines': {'hirsel': 100, 'idiomatic': 5317, 'matched': 164}, 'rows': []}
            for scenario in ['check-fresh', 'build-fresh', 'build-reuse', 'build-edit']:
                previous['rows'].append({'scenario': scenario, 'source_lines': dict(previous['source_lines']),
                                        'samples': {side: {'status': 'ok', 'instructions': 1000, 'seconds': 1, 'max_rss_kib': 1}
                                                    for side in ['hirsel', 'matched']}, 'ratios': {}})
            resume = root / 'previous.json'
            resume.write_text(json.dumps(previous))
            args = argparse.Namespace(out=root / 'new', backend='llvm', module='base64', suite=False,
                                      matched_source=run.ROOT / 'core/langarena/rust-matched/base64.rs',
                                      matched_author='langs-numeric-920', compiler=root / 'unused', runtime=root / 'unused',
                                      resume=resume, diagnostic=False, toolchain=tools)
            with patch.object(run.subprocess, 'check_output', return_value='fixture'), patch.object(
                    run._measure, 'sample', side_effect=AssertionError('completed sample repeated')):
                self.assertEqual(run.build_samples(args), 0)
            result = json.loads((args.out / 'build-report.json').read_text())
            edit = result['rows'][-1]
            self.assertEqual(edit['source_lines']['matched'], result['source_lines']['matched'] + 1)
            self.assertEqual(edit['samples']['matched']['instructions'], 1000)
            self.assertEqual(edit['samples']['matched']['k_instructions_per_source_line'],
                             1000 / edit['source_lines']['matched'] / 1000)


class ReviewTests(unittest.TestCase):
    def test_authorized_third_lane_review_and_no_self_review(self):
        with tempfile.TemporaryDirectory() as tmp:
            source = Path(tmp) / 'json.rs'
            source.write_text('// Reviewed-by: core-regions-pools-922 scope Json3\n')
            self.assertTrue(run.reviewed(source, 'langs-text-921'))
            self.assertFalse(run.reviewed(source, 'core-regions-pools-922'))
            source.write_text('// Reviewed-by: unassigned-lane\n')
            self.assertFalse(run.reviewed(source, 'langs-text-921'))


if __name__ == '__main__':
    unittest.main()
