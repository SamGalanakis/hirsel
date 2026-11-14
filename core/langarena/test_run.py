"""Source-line bookkeeping must also correct reused body-edit samples."""
import argparse
import json
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
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


class ProfileResumeTests(unittest.TestCase):
    def test_profile_below_threshold_preserves_primary_samples_even_on_build_failure(self):
        for build_exit in [0, 1]:
            with self.subTest(build_exit=build_exit), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                tools = {'compiler_build_profile': 'release',
                         'runtime_archive_profile': 'shipping-release-fat-lto'}
                config = {'name': 'Fixture::Run', 'checksum': 7, 'iterations': 1}
                (root / 'config.json').write_text(json.dumps([config]))
                sample = {'status': 'ok', 'instructions': 100, 'seconds': 1, 'max_rss_kib': 1}
                samples = {side: dict(sample) for side in ['hirsel', 'idiomatic', 'matched']}
                previous = {'task': 'Fixture::Run', 'backend': 'llvm', 'configuration': config,
                            'toolchain': tools, 'root_bytes': 67108864, 'samples': samples}
                (root / 'previous.json').write_text(json.dumps(previous))
                argv = ['run.py', '--compiler', str(root / 'compiler'), '--runtime', str(root / 'runtime'),
                        '--module', 'base64', '--task', 'Fixture::Run', '--backend', 'llvm',
                        '--idiomatic', str(root / 'rust'), '--config', str(root / 'config.json'),
                        '--resume', str(root / 'previous.json'), '--out', str(root / 'new'),
                        '--profile-always']
                calls = []

                def execute(command, **kwargs):
                    calls.append(command)
                    if 'build' in command:
                        if build_exit == 0:
                            Path(command[command.index('-o') + 1]).write_bytes(b'profile fixture')
                        return SimpleNamespace(returncode=build_exit, stdout=b'', stderr=b'')
                    return SimpleNamespace(returncode=0)

                with patch.object(sys, 'argv', argv), patch.object(run, 'vendor_check'), patch.object(
                        run, 'toolchain_identity', return_value=tools), patch.object(
                        run.subprocess, 'check_output', return_value='fixture'), patch.object(
                        run.subprocess, 'run', side_effect=execute), patch.object(
                        run._measure, 'sample', side_effect=AssertionError('primary sample repeated')), patch.object(
                        run.os, 'sched_getaffinity', return_value={16}), patch.object(
                        run.os, 'sched_setaffinity'), patch.object(run, 'wait_load'), patch('builtins.print'):
                    self.assertEqual(run.main(), build_exit)
                result = json.loads((root / 'new/report.json').read_text())
                self.assertEqual(result['samples'], samples)
                self.assertTrue(all(row['instructions'] == 1 for row in result['ratios'].values()))
                self.assertFalse((root / 'new/hirsel-run').exists())
                self.assertEqual(sum('record' in command for command in calls), int(build_exit == 0))


if __name__ == '__main__':
    unittest.main()
