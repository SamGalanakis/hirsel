#!/usr/bin/env python3
"""The scheduled sweep fails closed on a red, empty or missing selection."""
import contextlib
import io
import json
from pathlib import Path
import runpy
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
SWEEP = runpy.run_path(str(ROOT / 'tools/sweep'))
RUST = '---- STDOUT ----\ntest native::tests::one ... ok\n---- STDERR ----\n'
PYTHON = '---- STDERR ----\nRan 72 tests in 1.0s\n\nOK\n'


def result(details, status=1):
    return dict(status=status, details=details, seconds=1.0)


class Sweep(unittest.TestCase):
    def test_completed_rust_and_python_bodies_are_counted(self):
        self.assertEqual(SWEEP['bodies'](RUST), 1)
        self.assertEqual(SWEEP['bodies'](PYTHON, python=True), 72)

    def test_arm_observations_count_only_the_completed_outer_sweep(self):
        self.assertEqual(SWEEP['bodies']('AARCH64_EXECUTED 40\n'), 40)
        self.assertEqual(SWEEP['bodies']('AARCH64_EXECUTED 0\n'), 0)
        self.assertEqual(SWEEP['bodies']('---- STDERR ----\nAARCH64_EXECUTED 40\n'), 0)

    def test_nested_stderr_is_not_an_outer_rust_body(self):
        self.assertEqual(SWEEP['bodies']('---- STDERR ----\ntest nested ... ok\n'), 0)

    def test_zero_and_ignored_only_selections_are_red(self):
        for name, details in (
            ('core-driver-native', 'test skipped ... ignored\n'),
            ('test-land', 'Ran 0 tests in 0.0s\nOK\n'),
            ('test-land', 'Ran 1 test in 0.0s\nOK (skipped=1)\n'),
        ):
            with self.subTest(name=name, details=details):
                row = SWEEP['summarize']([(name, result(details))], name)
                self.assertEqual(row['bodies'], 0)
                self.assertIn(f'{name}: no completed test bodies', row['errors'])

    def test_a_red_keeps_its_body_and_full_diagnostic(self):
        details = 'test native::tests::one ... FAILED\nfirst failing line\n'
        row = SWEEP['summarize']([('core-driver-native-pool', result(details, 2))], 'core-driver-native')
        self.assertEqual(row['bodies'], 1)
        self.assertTrue(row['errors'])
        self.assertEqual(row['targets'][0]['details'], details)

    def test_missing_results_and_wrong_suite_are_red(self):
        self.assertEqual(SWEEP['summarize']([], 'test-land')['errors'], ['test-land: no test results'])
        self.assertTrue(SWEEP['summarize']([('other', result(RUST))], 'core-driver-native')['errors'])

    def test_only_native_receives_the_serial_libtest_argument(self):
        execute = SWEEP['execute']
        with tempfile.TemporaryDirectory() as directory, patch.dict(execute.__globals__, wait_load=lambda: 0), \
             patch('subprocess.run', return_value=SimpleNamespace(returncode=0)):
            for suite in SWEEP['SUITES']:
                _, target, _ = suite
                logger = SimpleNamespace(NoTestHistory=lambda: None, test_results=lambda *a, **kw:
                                         [(target, result(PYTHON if target == 'test-land' else RUST))])
                row = execute(suite, Path(directory) / 'result', logger)
                self.assertEqual(row['errors'], [])
                self.assertEqual('--test-arg=--test-threads=1' in row['command'], target in ('core-driver-native', 'core-llvm-aarch64-runtime'))
                self.assertIn('//:' + target, row['command'])

    def test_a_red_does_not_prevent_the_other_suite_and_the_dated_report(self):
        main = SWEEP['main']
        for red in (False, True):
            with self.subTest(red=red), tempfile.TemporaryDirectory() as directory:
                directory = Path(directory)
                descriptor, path = tempfile.mkstemp(suffix='.json', dir=directory)
                def execute(suite, base, logger):
                    return dict(suite=suite[1], bodies=1, duration_seconds=1.0, log='log',
                                errors=['native red'] if red and suite[0] == 'native' else [])
                with patch.dict(main.__globals__, ROOT=directory, wait_load=lambda: 0, execute=execute), \
                     patch.dict(sys.modules, event_logs=SimpleNamespace()), \
                     patch.object(sys, 'argv', ['sweep']), \
                     patch('subprocess.check_output', return_value='main\n'), \
                     patch('subprocess.run', return_value=SimpleNamespace(returncode=0)), \
                     patch('tempfile.mkstemp', return_value=(descriptor, path)), \
                     contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                    self.assertEqual(main(), int(red))
                report = json.loads(Path(path).read_text())
                self.assertEqual({row['suite'] for row in report['suites']}, {'core-driver-native', 'core-llvm-aarch64-runtime', 'core-llvm-aarch64-front', 'core-llvm-aarch64-std', 'core-llvm-aarch64-m1', 'test-land'})
                self.assertEqual(report['status'], 'failed' if red else 'passed')
                self.assertEqual(report['bodies'], 6)
                self.assertEqual(report['source'], 'main')


if __name__ == '__main__':
    unittest.main()
