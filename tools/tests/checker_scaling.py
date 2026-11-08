#!/usr/bin/env python3
"""Measurement verdicts must distinguish tolerated N log N from cubic work."""
import importlib.util
import math
import os
import subprocess
import sys
from pathlib import Path
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('checker_scaling', Path(__file__).resolve().parents[1] / 'checker-scaling.py')
scaling = importlib.util.module_from_spec(spec)
spec.loader.exec_module(scaling)
BASELINE = {'status': 0, 'instructions': 1000, 'rss_kb': 1000}


def samples(work, status=0):
    return {n: {'status': status, 'instructions': 1000 + work(n), 'rss_kb': 1000 + work(n), 'detail': ''}
            for n in scaling.SIZES}


class Growth(unittest.TestCase):
    def test_linearithmic_work_passes_and_cubic_work_fails(self):
        self.assertEqual(scaling.judge(samples(lambda n: 100 * n * math.log2(n)), BASELINE), [])
        failures = scaling.judge(samples(lambda n: n**3), BASELINE)
        self.assertTrue(any('instructions' in error for error in failures), failures)
        self.assertTrue(any('rss_kb' in error for error in failures), failures)

    @patch.dict(scaling.EXPECTED, {'match_try_fields': 'owner'})
    def test_expected_failures_must_fail_by_growth_and_unexpected_passes_fail(self):
        family = 'match_try_fields'
        self.assertEqual(scaling.verdict(family, samples(lambda n: n**3), BASELINE)[0], True)
        self.assertEqual(scaling.verdict(family, samples(lambda n: n), BASELINE)[0], False)
        broken = samples(lambda n: n**3)
        broken[10] = {'status': 1, 'instructions': None, 'rss_kb': 1, 'detail': 'type error'}
        self.assertEqual(scaling.verdict(family, broken, BASELINE)[0], False)

    @patch.object(scaling.subprocess, 'check_output', return_value='''{"products":"/installed/products"}''')
    @patch.dict(os.environ, {'HOME': '/home/checker', 'XDG_CACHE_HOME': '/cache/checker',
                             'HIRSEL_PRODUCTS': '/installed/products', 'HIRSEL_DAEMON': 'on'}, clear=True)
    def test_checker_keeps_product_discovery_and_runs_in_the_measured_process(self, discover):
        child = subprocess.run(
            [sys.executable, '-c',
             'import os,json; print(json.dumps([os.environ.get(k) for k in '
             '["HOME", "XDG_CACHE_HOME", "HIRSEL_PRODUCTS", "HIRSEL_DAEMON"]]))'],
            env=scaling.environment("/compiler/hirsel"), capture_output=True, text=True, check=True)
        self.assertEqual(discover.call_args.args[0], ['/compiler/hirsel', 'env', '--json'])
        self.assertEqual(child.stdout.strip(),
                         '["/home/checker", "/cache/checker", "/installed/products", "off"]')

    @patch.dict(os.environ, {'HOME': '/home/checker'}, clear=True)
    @patch.object(scaling.subprocess, 'check_output', return_value='{"products":"/toolchain/hirsel-std.products"}')
    def test_cachegrind_uses_the_compilers_discovered_products(self, discover):
        env = scaling.environment('/toolchain/hirsel')
        self.assertEqual(env['HIRSEL_PRODUCTS'], '/toolchain/hirsel-std.products')

    @patch.dict(os.environ, {'HIRSEL_RESULTS': 'dir@1', 'HIRSEL_RESULTS_DIR': '/results'}, clear=True)
    @patch.object(scaling.subprocess, 'check_output', return_value='{"products":"/products"}')
    def test_explicit_result_store_policy_is_preserved(self, _discover):
        env = scaling.environment('/toolchain/hirsel')
        self.assertEqual((env['HIRSEL_RESULTS'], env['HIRSEL_RESULTS_DIR']), ('dir@1', '/results'))

    def test_all_families_have_all_required_sizes(self):
        for family in scaling.FAMILIES:
            programs = [scaling.program(family, n) for n in scaling.SIZES]
            self.assertEqual(len(set(programs)), 3, family)
            self.assertTrue(all('fn probe' in program for program in programs))


if __name__ == '__main__':
    unittest.main()
