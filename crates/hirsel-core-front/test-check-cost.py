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
