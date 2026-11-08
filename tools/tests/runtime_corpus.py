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
                 hirsel=[dict(instructions=100 * ratio, seconds=ratio / 100) for _ in range(repeats)],
                 rust=[dict(instructions=100, seconds=0.01) for _ in range(repeats)])
            for case, ratio in zip(corpus.CASES, ratios)]


class Acceptance(unittest.TestCase):
    def test_wall_time_comes_from_elapsed_summary_not_duration_event(self):
        # The actual first run reported 1,272 ns for a process doing 6M instructions.
        # Never treat that event as wall time or accept it without a summary.
        raw = '6213737 instructions:u\n1272 ns duration_time\n0.001234567 seconds time elapsed\n'
        result = corpus.perf_result(raw)
        self.assertEqual(result['instructions'], 6213737)
        self.assertEqual(result['seconds'], 0.001234567)
        with self.assertRaises(ValueError):
            corpus.perf_result('6213737 instructions:u\n1272 ns duration_time\n')
        with self.assertRaises(ValueError):
            corpus.perf_result('<not counted> instructions:u\n0.01 seconds time elapsed\n')

    def test_geomean_is_not_arithmetic_mean_and_max_is_independent(self):
        # Four 1/4 and four 4 ratios have geometric mean 1, arithmetic mean 2.125.
        # The per-row ceiling still refuses this otherwise acceptable geomean.
        result = corpus.summarize(rows([0.25] * 4 + [4] * 4), 1)
        self.assertAlmostEqual(result['instructions']['geomean'], 1)
        self.assertEqual(result['instructions']['max'], 4)
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
        repeated[0]['hirsel'] = [dict(instructions=n, seconds=n / 100) for n in (90, 100, 110)]
        corpus.summarize(repeated, 3)
        self.assertEqual(repeated[0]['instructions_ratio'], 1)
        self.assertEqual(repeated[0]['instructions_spread_pct']['hirsel'], 20)
        self.assertAlmostEqual(repeated[0]['seconds_spread_pct']['hirsel'], 20)

    def test_partial_repetition_is_not_accepted(self):
        data = rows([1] * 8, 2)
        data[-1]['rust'].pop()
        with self.assertRaises(ValueError):
            corpus.summarize(data, 2)


if __name__ == '__main__':
    unittest.main()
