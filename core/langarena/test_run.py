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
    def test_imported_fresh_row_cannot_prime_the_live_build_store(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            tools = {'compiler_build_profile': 'release'}
            sample = {'status': 'ok', 'instructions': 100, 'seconds': 1, 'max_rss_kib': 1}
            counts = dict(hirsel=100, matched=50, idiomatic=500)
            rows = [dict(scenario=scenario, source_lines=counts,
                         samples=dict(hirsel=sample, matched=sample), ratios={})
                    for scenario in ['check-fresh', 'build-fresh']]
            resume = root / 'previous.json'
            resume.write_text(json.dumps(dict(module='base64', backend='llvm', toolchain=tools,
                                              source_lines=counts, rows=rows)))
            args = argparse.Namespace(out=root / 'new', backend='llvm', module='base64', suite=False,
                                      matched_source=run.ROOT / 'core/langarena/rust-matched/base64.rs',
                                      matched_author='langs-numeric-920', compiler=root / 'compiler',
                                      runtime=root / 'runtime', resume=resume, diagnostic=False, toolchain=tools)
            commands = []

            def measure(command, directory):
                commands.append(command)
                directory.mkdir()
                (directory / 'stdout').write_text('{"accepted":true}')
                return dict(sample, directory=directory)

            with patch.object(run.subprocess, 'check_output', return_value='fixture'), patch.object(
                    run._measure, 'sample', side_effect=measure), patch.object(run, 'wait_load'), patch('builtins.print'):
                result = run.build_samples(args)
            self.assertEqual([Path(command[0]).name for command in commands], ['rustc', 'rustc'])
            self.assertEqual(result, 1)
            rows = json.loads((args.out / 'build-report.json').read_text())['rows']
            self.assertEqual([row['samples']['hirsel']['status'] for row in rows],
                             ['ok', 'ok', 'blocked', 'blocked'])

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
    def test_resume_runs_only_sides_whose_inputs_changed(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            config = root / 'config.json'
            config.write_text('[{"name":"Fixture::Run","checksum":7,"iterations":1}]')
            idiomatic = root / 'rust'
            matched = root / 'matched'
            source = root / 'matched.rs'
            idiomatic.write_bytes(b'first Rust binary')
            matched.write_bytes(b'matched binary')
            source.write_text('// Reviewed-by: core-regions-pools-922\n')
            tools = {'runtime_archive_sha256': 'first'}
            calls = []

            def measure(command, directory, task, expected, **kwargs):
                calls.append((directory.name, command[-1]))
                if directory.name == 'matched-run':
                    count = int(command[-1]) * 30
                elif directory.name == 'idiomatic-run':
                    count = 120 if idiomatic.read_bytes().startswith(b'first') else 60
                else:
                    count = 240
                return dict(status='ok', instructions=count, seconds=1, max_rss_kib=1)

            def invoke(name, argument, resume=None):
                argv = ['run.py', '--compiler', str(root / 'compiler'), '--runtime', str(root / 'runtime'),
                        '--module', 'base64', '--task', 'Fixture::Run', '--backend', 'llvm',
                        '--idiomatic', str(idiomatic), '--matched', str(matched), '--matched-source', str(source),
                        '--config', str(config), '--out', str(root / name), '--matched-args', argument]
                if resume:
                    argv += ['--resume', str(resume)]
                with patch.object(sys, 'argv', argv):
                    self.assertEqual(run.main(), 0)
                return root / name / 'report.json'

            with patch.object(run, 'vendor_check'), patch.object(
                    run, 'toolchain_identity', side_effect=lambda *args: dict(tools)), patch.object(
                    run.subprocess, 'check_output', return_value='fixture'), patch.object(
                    run.subprocess, 'run', return_value=SimpleNamespace(returncode=0, stdout=b'', stderr=b'')), patch.object(
                    run, 'observe', side_effect=measure), patch.object(
                    run.os, 'sched_getaffinity', return_value={16}), patch.object(
                    run.os, 'sched_setaffinity'), patch.object(run, 'wait_load'), patch('builtins.print'):
                first = invoke('first', '2')
                calls.clear()
                tools['runtime_archive_sha256'] = 'second'
                second = invoke('second', '2', first)
                self.assertEqual(calls, [('hirsel-run', 'Fixture::Run')])
                calls.clear()
                idiomatic.write_bytes(b'replacement Rust binary')
                updated = invoke('updated', '4', second)
                self.assertEqual(calls, [('idiomatic-run', 'Fixture::Run'), ('matched-run', '4')])
            ratios = json.loads(updated.read_text())['ratios']
            self.assertEqual(ratios['idiomatic']['instructions'], 4)
            self.assertEqual(ratios['matched']['instructions'], 2)

    def test_profile_below_threshold_preserves_primary_samples_even_on_build_failure(self):
        for build_exit in [0, 1]:
            with self.subTest(build_exit=build_exit), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                tools = {'compiler_build_profile': 'release',
                         'runtime_archive_profile': 'shipping-release-fat-lto'}
                config = {'name': 'Fixture::Run', 'checksum': 7, 'iterations': 1}
                (root / 'config.json').write_text(json.dumps([config]))
                sample = {'status': 'ok', 'instructions': 100, 'seconds': 1, 'max_rss_kib': 1}
                (root / 'rust').write_bytes(b'idiomatic binary')
                (root / 'matched').write_bytes(b'matched binary')
                (root / 'matched.rs').write_text('// Reviewed-by: core-regions-pools-922\n')
                argv = ['run.py', '--compiler', str(root / 'compiler'), '--runtime', str(root / 'runtime'),
                        '--module', 'base64', '--task', 'Fixture::Run', '--backend', 'llvm',
                        '--idiomatic', str(root / 'rust'), '--matched', str(root / 'matched'),
                        '--matched-source', str(root / 'matched.rs'), '--config', str(root / 'config.json')]
                calls = []

                def execute(command, **kwargs):
                    calls.append(command)
                    if 'build' in command:
                        output = Path(command[command.index('-o') + 1])
                        status = build_exit if output.parent.name == 'new' else 0
                        if status == 0:
                            output.write_bytes(b'profile fixture')
                        return SimpleNamespace(returncode=status, stdout=b'', stderr=b'')
                    return SimpleNamespace(returncode=0)

                with patch.object(run, 'vendor_check'), patch.object(
                        run, 'toolchain_identity', return_value=tools), patch.object(
                        run.subprocess, 'check_output', return_value='fixture'), patch.object(
                        run.subprocess, 'run', side_effect=execute), patch.object(run, 'observe',
                        side_effect=[dict(sample) for _ in range(3)]) as observe, patch.object(
                        run.os, 'sched_getaffinity', return_value={16}), patch.object(
                        run.os, 'sched_setaffinity'), patch.object(run, 'wait_load'), patch('builtins.print'):
                    previous = root / 'previous/report.json'
                    with patch.object(sys, 'argv', argv + ['--out', str(previous.parent)]):
                        self.assertEqual(run.main(), 0)
                    calls.clear()
                    with patch.object(sys, 'argv', argv + ['--resume', str(previous), '--out', str(root / 'new'), '--profile-always']):
                        self.assertEqual(run.main(), build_exit)
                self.assertEqual(observe.call_count, 3)
                result = json.loads((root / 'new/report.json').read_text())
                self.assertEqual(result['samples'], json.loads(previous.read_text())['samples'])
                self.assertTrue(all(row['instructions'] == 1 for row in result['ratios'].values()))
                self.assertFalse((root / 'new/hirsel-run').exists())
                self.assertEqual(sum('record' in command for command in calls), int(build_exit == 0))


if __name__ == '__main__':
    unittest.main()
