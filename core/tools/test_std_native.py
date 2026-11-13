#!/usr/bin/env python3
"""Contract walker controls: selection, source oracle and unavailable-body reporting."""
import contextlib
import io
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from std_native import Contract, discover, first_stop, observation, owner, wrapper, test_module, manifest, evaluator_observation, run


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

    def test_core_marked_and_public_contracts_use_core_without_publishing_tests(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            library = root / 'core/core'
            tests = library / 'tests'
            tests.mkdir(parents=True)
            (root / 'core/std').mkdir()
            (library / 'slots.hirsel').write_text('fn hidden()->u64 {7}')
            marked = tests / 'slots_internal.hirsel'
            marked.write_text('// test-of: core.slots\n// test-reason: only private slots can express this law\n// expect: run law() = 7\nfn law()->u64 {hidden()}')
            case = discover(root, ['core.slots_internal'])[0]
            built = manifest(root, case, Path('/tmp/spliced.hirsel'))
            self.assertEqual(built['packages'][0]['sources'], [dict(file='/tmp/spliced.hirsel', modules=['slots'], signatures=True)])
            self.assertEqual(built['packages'][1]['sources'], [])
            ordinary = tests / 'slots_public.hirsel'
            ordinary.write_text('// expect: run public_law() = 7\nfn public_law()->u64 {7}')
            case = discover(root, ['core.slots_public'])[0]
            built = manifest(root, case, Path('/tmp/public.hirsel'))
            self.assertEqual(built['packages'][0]['sources'][-1], dict(file='/tmp/public.hirsel', modules=['tests', 'slots_public'], signatures=False))
            with self.assertRaisesRegex(ValueError, 'collision'):
                test_module(root, marked, marked.read_text().replace('law', 'hidden'))
            with self.assertRaisesRegex(ValueError, 'must have bodies'):
                test_module(root, marked, marked.read_text() + '\nfn external()->u64')
            with self.assertRaisesRegex(ValueError, 'own library package'):
                test_module(root, marked, marked.read_text().replace('core.slots', 'std.slots'))

    def test_evaluator_fault_is_exact_and_native_is_explicitly_inapplicable(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            tests = root / 'core/core/tests'
            tests.mkdir(parents=True)
            (root / 'core/std').mkdir()
            file = tests / 'misuse.hirsel'
            file.write_text('// test-backend: evaluator\n// test-reason: native slot operations deliberately have no misuse guards\n// expect: run bad() = fault "take of the empty slot at +0"\nfn bad()->u64 {7}')
            cases = discover(root, ['core.misuse'])
            evaluator = root / 'eval'
            evaluator.write_text('#!/usr/bin/env python3\nprint(\'{"outcome":{"kind":"fault","message":"take of the empty slot at +0"}}\')\n')
            evaluator.chmod(0o755)
            output = root / 'rows.json'
            with contextlib.redirect_stdout(io.StringIO()) as printed:
                rows = run(root, root / 'must-not-build', root / 'unused-runtime', cases,
                           ['cranelift', 'llvm'], output, [], evaluator=evaluator)
            self.assertEqual([(r['engine'], r['ran'], r['passed']) for r in rows],
                             [('evaluator', True, True), ('cranelift', False, None), ('llvm', False, None)])
            self.assertEqual(sum(r['applicable'] and not r['passed'] for r in rows), 0)
            self.assertIn('0 ran, 0 passed, 0 not run, 1 not applicable', printed.getvalue())
            self.assertEqual(json.loads(output.read_text()), rows)
            for result in [subprocess.CompletedProcess([], 101, b'', b'take of the empty slot at +0'),
                           subprocess.CompletedProcess([], 0, b'{"outcome":{"kind":"fault","message":"wrong"}}', b'')]:
                self.assertIsNotNone(evaluator_observation(cases[0], result)[1])
            with self.assertRaisesRegex(ValueError, 'require --evaluator'):
                run(root, root / 'unused', root / 'unused', cases, ['llvm'], None, [])
            with self.assertRaisesRegex(ValueError, 'fault oracle requires'):
                file.write_text(file.read_text().replace('// test-backend: evaluator\n', ''))
                discover(root, ['core.misuse'])

    def test_unavailable_reachable_body_names_its_first_cause_and_owner(self):
        product = {'reachable': {'first_stops': [{'primary': {'module': 'std@1/x', 'span': {'start': 2, 'end': 4}}, 'rule': 'GAP', 'construct': 'Dest values are not built'}]}}
        cause = first_stop(product)
        self.assertIn('std@1/x:2..4', cause)
        self.assertEqual(owner(cause, [{'row': 'M2', 'message': 'Dest values are not built', 'owner': '895'}]), 'M2 core-check-895')
        self.assertEqual(first_stop({'stopped': 'link: missing symbol'}), 'link: missing symbol')


if __name__ == '__main__':
    unittest.main()
