#!/usr/bin/env python3
"""Contract walker controls: selection, source oracle and unavailable-body reporting."""
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from std_native import Contract, discover, first_stop, observation, owner, wrapper


class Walker(unittest.TestCase):
    def test_selection_never_turns_an_empty_filter_green(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            tests = root / 'core/std/tests'
            tests.mkdir(parents=True)
            (tests / 'file.hirsel').write_text('// expect: run f(dir) = 7\nfn f(d:Dir)->u64 waits { 7 }')
            cases = discover(root, ['file:f'])
            self.assertEqual(len(cases), 1)
            self.assertIn('native_dir:Dir', wrapper(cases[0]))
            self.assertIn('f(native_dir) == 7', wrapper(cases[0]))
            self.assertIn(' waits', wrapper(cases[0]))
            with self.assertRaisesRegex(ValueError, 'no expect: run'):
                discover(root, ['missing'])

    def test_expected_failure_is_not_an_arbitrary_process_crash(self):
        case = Contract(Path('test.hirsel'), 1, 'f()', 'fail Bounds', False)
        for status, stderr in [(0, b''), (101, b'Overflow at 3\n'), (-11, b'')]:
            self.assertIsNotNone(observation(case, subprocess.CompletedProcess([], status, b'', stderr)))
        self.assertIsNone(observation(case, subprocess.CompletedProcess([], 101, b'', b'Bounds at 3\n')))
        failure = Contract(case.file, 1, 'f()', 'fail Fail', False)
        self.assertIsNone(observation(failure, subprocess.CompletedProcess([], 101, b'', b'Fail("message") at 3\n')))
        case = Contract(case.file, 1, 'f()', '7', False)
        self.assertIsNotNone(observation(case, subprocess.CompletedProcess([], 1, b'', b'')))
        self.assertIsNotNone(observation(case, subprocess.CompletedProcess([], 0, b'wrong', b'')))

    def test_unavailable_reachable_body_names_its_first_cause_and_owner(self):
        product = {'reachable': {'first_stops': [{'primary': {'module': 'std@1/x', 'span': {'start': 2, 'end': 4}}, 'rule': 'GAP', 'construct': 'Dest values are not built'}]}}
        cause = first_stop(product)
        self.assertIn('std@1/x:2..4', cause)
        self.assertEqual(owner(cause, [{'row': 'M2', 'message': 'Dest values are not built', 'owner': '895'}]), 'M2 core-check-895')
        self.assertEqual(first_stop({'stopped': 'link: missing symbol'}), 'link: missing symbol')


if __name__ == '__main__':
    unittest.main()
