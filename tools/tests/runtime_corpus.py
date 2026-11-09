#!/usr/bin/env python3
"""Fixed-corpus acceptance must retain failures and every workload's weight."""
import importlib.machinery
import importlib.util
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[2]
loader = importlib.machinery.SourceFileLoader('runtime_corpus', str(ROOT / 'tools/runtime-corpus'))
corpus = importlib.util.module_from_spec(importlib.util.spec_from_loader(loader.name, loader))
loader.exec_module(corpus)


def rows(ratios, repeats=1):
    return [dict(name=case[0], status='OK',
                 hirsel=[dict(instructions=100, user_instructions=90, cpu_seconds=ratio / 100,
                              context_switches=2, seconds=ratio / 100) for _ in range(repeats)],
                 rust=[dict(instructions=100, user_instructions=90, cpu_seconds=0.01,
                           context_switches=2, seconds=0.01) for _ in range(repeats)])
            for case, ratio in zip(corpus.CASES, ratios)]


class Acceptance(unittest.TestCase):
    def test_perf_reports_diagnostics_without_using_its_elapsed_clock(self):
        # perf's elapsed timer can start after a short child finishes. Only the
        # independent release/teardown timer supplies seconds to the aggregate.
        raw = ('1000 instructions\n900 instructions:u\n20 context-switches\n'
               '0.04 seconds user\n0.06 seconds sys\n0.000001 seconds time elapsed\n')
        result = corpus.perf_result(raw)
        self.assertEqual(result['instructions'], 1000)
        self.assertEqual(result['user_instructions'], 900)
        self.assertAlmostEqual(result['cpu_seconds'], 0.1)
        self.assertEqual(result['context_switches'], 20)
        self.assertNotIn('seconds', result)
        with self.assertRaises(ValueError):
            corpus.perf_result(raw.replace('1000 instructions', '<not counted> instructions'))

    def test_only_wall_time_controls_budget(self):
        data = rows([1] * 8)
        for row in data:
            row['hirsel'][0]['instructions'] = 100000
            row['hirsel'][0]['user_instructions'] = 100000
            row['hirsel'][0]['cpu_seconds'] = 100
        self.assertTrue(corpus.summarize(data, 1)['within_budget'])
        for row in data:
            row['hirsel'][0]['instructions'] = 1
            row['hirsel'][0]['seconds'] = 0.02
        self.assertFalse(corpus.summarize(data, 1)['within_budget'])

    def test_geomean_is_not_arithmetic_mean_and_max_is_independent(self):
        # Four 1/4 and four 4 ratios have geometric mean 1, arithmetic mean 2.125.
        # The per-row ceiling still refuses this otherwise acceptable geomean.
        result = corpus.summarize(rows([0.25] * 4 + [4] * 4), 1)
        self.assertAlmostEqual(result['seconds']['geomean'], 1)
        self.assertEqual(result['seconds']['max'], 4)
        self.assertFalse(result['within_budget'])

    def test_failed_or_missing_row_cannot_turn_into_a_passing_subset(self):
        for problem in ('failed', 'missing', 'duplicate'):
            data = rows([1] * 8)
            if problem == 'failed':
                data[-1]['status'] = 'FAILED'
            elif problem == 'missing':
                data.pop()
            else:
                data[-1]['name'] = data[0]['name']
            result = corpus.summarize(data, 1)
            self.assertFalse(result['complete'])
            self.assertFalse(result['within_budget'])
            self.assertIsNone(result['instructions'])
            self.assertIsNone(result['seconds'])

    def test_one_sample_has_unknown_spread_and_repeats_report_min_max(self):
        one = rows([1] * 8)
        corpus.summarize(one, 1)
        self.assertIsNone(one[0]['instructions_spread_pct']['hirsel'])
        repeated = rows([1] * 8, 3)
        for sample, n in zip(repeated[0]['hirsel'], (0.009, 0.01, 0.011)):
            sample['seconds'] = n
        corpus.summarize(repeated, 3)
        self.assertEqual(repeated[0]['seconds_ratio'], 1)
        self.assertEqual(repeated[0]['instructions_spread_pct']['hirsel'], 0)
        self.assertAlmostEqual(repeated[0]['seconds_spread_pct']['hirsel'], 20)

    def test_partial_repetition_is_not_accepted(self):
        data = rows([1] * 8, 2)
        data[-1]['rust'].pop()
        with self.assertRaises(ValueError):
            corpus.summarize(data, 2)


if __name__ == '__main__':
    unittest.main()
