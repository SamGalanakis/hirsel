#!/usr/bin/env python3
"""Contract walker controls: selection, source oracle and unavailable-body reporting."""
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from std_native import Contract, discover, first_stop, observation, owner, wrapper, test_module, manifest


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

    def test_scripted_partial_write_annotation_and_output_are_exact(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            tests = root / 'core/std/tests'
            tests.mkdir(parents=True)
            (tests / 'io.hirsel').write_text('// host: first_write 1\n// expect: stdout "éx"\n// expect: run f(out) = true\nfn f(o:Stdout)->bool waits {true}')
            case = discover(root, ['io'])[0]
            self.assertEqual(case.first_write, 1)
            self.assertEqual(case.stdout, 'éx'.encode())
            self.assertIn('native_out:Stdout', wrapper(case))
            self.assertIsNone(observation(case, subprocess.CompletedProcess([], 0, 'éx'.encode(), b'')))
            self.assertIsNotNone(observation(case, subprocess.CompletedProcess([], 0, 'ééx'.encode(), b'')))

    def test_marked_module_is_test_only_and_rejects_collision(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            library = root / 'core/std'
            tests = library / 'tests'
            tests.mkdir(parents=True)
            (root / 'core/core').mkdir()
            (library / 'pool.hirsel').write_text('const SECRET:u64=7\nfn original()->u64 {SECRET}')
            source = '// test-of: std.pool\n// test-reason: private state cannot be reached publicly\n// expect: run probe() = 7\nfn probe()->u64 {SECRET}'
            file = tests / 'private.hirsel'
            file.write_text(source)
            case = discover(root, ['private'])[0]
            self.assertEqual(case.test_of, 'pool')
            built = manifest(root, case, Path('/tmp/combined.hirsel'))
            sources = built['packages'][1]['sources']
            self.assertEqual(sources, [{'file': '/tmp/combined.hirsel', 'modules': ['pool'], 'signatures': False}])
            with self.assertRaisesRegex(ValueError, 'collision'):
                test_module(root, file, source.replace('probe', 'original'))
            with self.assertRaisesRegex(ValueError, 'test-reason'):
                test_module(root, file, source.replace('// test-reason:', '// note:'))
            with self.assertRaisesRegex(ValueError, 'only private'):
                test_module(root, file, source + '\npub fn exported() {}')
            ordinary = Contract(file, 1, 'probe()', '7', False)
            self.assertEqual(manifest(root, ordinary, Path('/tmp/test.hirsel'))['packages'][1]['sources'][-1]['modules'], ['tests', 'private'])

    def test_unavailable_reachable_body_names_its_first_cause_and_owner(self):
        product = {'reachable': {'first_stops': [{'primary': {'module': 'std@1/x', 'span': {'start': 2, 'end': 4}}, 'rule': 'GAP', 'construct': 'Dest values are not built'}]}}
        cause = first_stop(product)
        self.assertIn('std@1/x:2..4', cause)
        self.assertEqual(owner(cause, [{'row': 'M2', 'message': 'Dest values are not built', 'owner': '895'}]), 'M2 core-check-895')
        self.assertEqual(first_stop({'stopped': 'link: missing symbol'}), 'link: missing symbol')


if __name__ == '__main__':
    unittest.main()
