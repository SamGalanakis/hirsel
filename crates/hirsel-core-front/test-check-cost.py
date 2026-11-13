#!/usr/bin/env python3
"""The process ceiling must catch startup work outside the driver meter."""
import runpy
from pathlib import Path
import tempfile
import unittest

GUARD = runpy.run_path(str(Path(__file__).with_name('check-cost.py')))


class ProcessCeiling(unittest.TestCase):
    def test_startup_regression_fails_when_driver_cost_is_unchanged(self):
        metrics = {'instructions': 10000, 'stages': [{'name': 'check/lower', 'instructions': 5000}]}
        ceiling = {'code_lines': 1000, 'check_lower_per_line': 5,
                   'whole_request_per_line': 10, 'whole_process_per_line': 30}
        check = GUARD['check_ceiling']
        self.assertEqual(check(metrics, 1000, ceiling, 30000)[2], [])
        errors = check(metrics, 1000, ceiling, 30001)[2]
        self.assertEqual(len(errors), 1)
        self.assertIn('whole process', errors[0])
        with self.assertRaisesRegex(ValueError, 'does not cover'):
            check(metrics, 1000, ceiling, 9999)

    def test_source_growth_uses_actual_lines_and_keeps_numeric_limits(self):
        metrics = {'instructions': 10000, 'stages': [{'name': 'check/lower', 'instructions': 5000}]}
        ceiling = {'code_lines': 1000, 'check_lower_per_line': 5,
                   'whole_request_per_line': 10, 'whole_process_per_line': 30}
        check = GUARD['check_ceiling']
        self.assertEqual(check(metrics, 1001, ceiling, 30030)[2], [])
        errors = check(metrics, 1001, ceiling, 30031)[2]
        self.assertEqual(len(errors), 1)
        self.assertIn('whole process', errors[0])
        with self.assertRaisesRegex(ValueError, 'no code lines'):
            check(metrics, 0, ceiling, 30000)

    def test_finished_budget_is_independent_of_current_work_ceiling(self):
        forecast = GUARD['finished_forecast']
        projection = {'missing_check_lower_per_line': 1000,
                      'additional_publication_per_line': 500}
        value, failures = forecast(58_500_000, 1000, projection, 60000)
        self.assertEqual(value, 60000)
        self.assertEqual(failures, [])
        # One instruction of additional startup work must reach the forecast too.
        _, failures = forecast(58_500_001, 1000, projection, 60000)
        self.assertEqual(len(failures), 1)
        self.assertIn('budget 60000', failures[0])
        projection['additional_publication_per_line'] = -1
        with self.assertRaisesRegex(ValueError, 'negative missing work'):
            forecast(1, 1000, projection, 60000)

    def test_product_fixed_and_marginal_are_separate_limits(self):
        check = GUARD['product_ceiling']
        self.assertEqual(check(70_000_000, 10_000_000, 1000, 60_000, 60_000), (60_000, []))
        value, errors = check(70_000_001, 10_000_000, 1000, 61_000, 60_000)
        self.assertGreater(value, 60_000)
        self.assertEqual(len(errors), 1)
        self.assertIn('budget 60000', errors[0])
        self.assertEqual(len(check(70_000_001, 10_000_000, 1000, 60_000, 60_000)[1]), 2)
        for process, fixed, lines in [(1, 2, 1), (1, 0, 1), (2, 1, 0)]:
            with self.assertRaisesRegex(ValueError, 'invalid fixed/marginal'):
                check(process, fixed, lines, 60000, 60000)

    def test_named_anchor_residual_does_not_hide_a_new_regression_or_budget_miss(self):
        metrics = {'instructions': 11000, 'stages': [{'name': 'check/lower', 'instructions': 6000}]}
        ceiling = {'check_lower_per_line': 5, 'whole_request_per_line': 10,
                   'whole_process_per_line': 30}
        check = GUARD['check_ceiling']
        self.assertEqual(check(metrics, 1000, ceiling, 31000, 1000)[2], [])
        # The allowance is an absolute frozen-corpus count, not a growing per-line exemption.
        metrics['stages'][0]['instructions'] += 1
        self.assertIn('check/lower', check(metrics, 1000, ceiling, 31000, 1000)[2][0])
        metrics['stages'][0]['instructions'] -= 1
        self.assertIn('whole process', check(metrics, 1000, ceiling, 31001, 1000)[2][0])
        with self.assertRaisesRegex(ValueError, 'negative recorded'):
            check(metrics, 1000, ceiling, 31000, -1)
        projection = {'missing_check_lower_per_line': 0, 'additional_publication_per_line': 0}
        self.assertEqual(len(GUARD['finished_forecast'](60000001, 1000, projection, 60000)[1]), 1)

    def test_a_new_partial_body_cannot_qualify_as_a_cheaper_finished_check(self):
        check = GUARD['completion_errors']
        complete = {'complete_bodies': 10, 'bodies': 10}
        self.assertEqual(check({'diagnostics': [], 'incomplete': []}, complete), [])
        for result in [{'diagnostics': ['refused'], 'incomplete': []},
                       {'diagnostics': [], 'incomplete': ['gap']}]:
            self.assertIn('every body', check(result, complete)[0])
        self.assertEqual(check({'diagnostics': [], 'incomplete': ['gap']},
                               {'complete_bodies': 9, 'bodies': 10}), [])

    def test_process_counter_requires_one_complete_event(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'perf.txt'
            path.write_text('# counter\n30001;;instructions:u;1;100.00\n')
            self.assertEqual(GUARD['process_instructions'](path), 30001)
            for row in ['', '<not counted>;;instructions:u;1;100.00\n',
                        '0;;instructions:u;1;100.00\n',
                        '1;;instructions:u\n2;;instructions:u\n']:
                path.write_text(row)
                with self.assertRaisesRegex(ValueError, 'no complete sample'):
                    GUARD['process_instructions'](path)


if __name__ == '__main__':
    unittest.main()
