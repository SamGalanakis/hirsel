#!/usr/bin/env python3
"""Behavioral fixtures for milestone stage and exact-observation reporting."""
import json
from pathlib import Path
import tempfile
import unittest

from m1 import census_table, inputs_for, run, table


class Milestone(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.corpus = self.root / 'core/corpus/m1/example'
        (self.corpus / 'inputs').mkdir(parents=True)
        (self.corpus / 'EXPECTED').mkdir()
        self.manifest = {
            'core': {'name': 'core', 'version': '1'},
            'packages': [
                {'package': {'name': 'example', 'version': '1'}, 'dependencies': [], 'sources': [
                    {'file': 'main.hirsel', 'modules': ['main'], 'signatures': False},
                    {'file': 'matched.hirsel', 'modules': ['matched'], 'signatures': False}]},
                {'package': {'name': 'unrelated', 'version': '1'}, 'dependencies': [], 'sources': []}],
            'entries': [{'workload': 'example', 'variant': 'idiomatic',
                         'package': {'name': 'example', 'version': '1'}, 'module': 'main', 'function': 'main'}]}
        self.path = self.root / 'manifest.json'
        self.path.write_text(json.dumps(self.manifest))
        self.compiler = self.root / 'compiler'
        self.compiler.write_text('''#!/usr/bin/env python3
import json, pathlib, sys
args=sys.argv
if args[1]=='image':
    print(json.dumps({'accepted':True, 'image':'canonical'}))
    raise SystemExit(0)
output=pathlib.Path(args[args.index('-o')+1])
pathlib.Path(__file__).with_suffix('.count').open('a').write('build\\n')
output.write_text("#!/usr/bin/env python3\\nimport os,pathlib,sys\\nprint(pathlib.Path(sys.argv[1]).read_text().strip()+os.environ['M1_CASE'])\\npathlib.Path('result').write_bytes(b'done')\\nsys.exit(7)\\n")
output.chmod(0o755)
print(json.dumps({'accepted':True, 'native':{'executable':str(output)}, 'diagnostics':[], 'incomplete':[]}))
''')
        self.compiler.chmod(0o755)
        cases = []
        for name in ['first', 'second']:
            cases.append({'name': name, 'input': f'inputs/{name}', 'variant': 'normal', 'env': {'M1_CASE': '!'}})
            (self.corpus / f'inputs/{name}').write_text(name)
            expected = self.corpus / 'EXPECTED' / name
            expected.with_suffix('.stdout').write_text(name + '!\n')
            expected.with_suffix('.stderr').write_bytes(b'')
            expected.with_suffix('.exit').write_text('7\n')
            expected.with_suffix('.files').mkdir()
            (expected.with_suffix('.files') / 'result').write_bytes(b'done')
        (self.corpus / 'cases.json').write_text(json.dumps(cases))

    def rows(self):
        return run(self.compiler, self.root / 'runtime.a', ['cranelift'], self.path,
                   self.root, self.root / 'store')

    def test_build_once_runs_all_cases_and_compares_every_observation(self):
        rows = self.rows()
        self.assertEqual(rows[0]['passed'], 2)
        self.assertTrue(rows[0]['built'])
        self.assertEqual(len(rows), 2)
        self.assertEqual(rows[1]['variant'], 'libraries')
        self.assertTrue(rows[1]['built'])
        self.assertEqual(self.compiler.with_suffix('.count').read_text(), 'build\n')
        # The oracle is independent fixture data, including status 7 and a file.
        # Each changed observation must turn the same accepted program red.
        for suffix, wrong in [('stdout', b'wrong'), ('stderr', b'error'), ('exit', b'8')]:
            expected = self.corpus / f'EXPECTED/first.{suffix}'
            original = expected.read_bytes()
            expected.write_bytes(wrong)
            row = self.rows()[0]
            self.assertEqual(row['passed'], 1)
            self.assertEqual(row['first_failure'], 'first')
            self.assertIn('no checker stops | 0 |', census_table([row]))
            self.assertIn({'exit': 'exit status'}.get(suffix, suffix), row['stopped'])
            expected.write_bytes(original)
        (self.corpus / 'EXPECTED/first.files/result').write_bytes(b'wrong')
        self.assertIn('files differ', self.rows()[0]['stopped'])

    def test_refused_and_unsupported_entries_remain_failing_rows(self):
        for product, reason in [
            ({'accepted': False, 'diagnostics': [{'rule': 'OWN-1', 'primary': {'module': 'example@1/main', 'span': {'start': 5, 'end': 9}}, 'message': 'moved'}]}, 'OWN-1 example@1/main:5..9'),
            ({'accepted': False, 'incomplete': [{'primary': {'module': 'example@1/main', 'span': {'start': 1, 'end': 2}}, 'message': 'generic body'}]}, 'checker gap'),
            ({'accepted': True, 'stopped': 'emit main: unsupported instruction'}, 'unsupported instruction'),
            ({'accepted': True, 'stopped': 'link: undefined symbol'}, 'undefined symbol')]:
            self.compiler.write_text('#!/usr/bin/env python3\nimport json\nprint(json.dumps(' + repr(product) + '))\nraise SystemExit(2)\n')
            row = self.rows()[0]
            self.assertFalse(row['built'])
            self.assertEqual((row['passed'], row['cases']), (0, 2))
            self.assertIn(reason, table([row]))
        selected = inputs_for(self.manifest, self.manifest['entries'][0], self.root)
        self.assertEqual([p['package']['name'] for p in selected['packages']], ['example'])
        self.assertEqual([s['modules'] for s in selected['packages'][0]['sources']], [['main'], ['matched']])

    def test_empty_selection_is_not_success(self):
        with self.assertRaisesRegex(ValueError, 'no entries'):
            run(self.compiler, self.root / 'runtime.a', ['llvm'], self.path, self.root, self.root / 'store', only=['absent'])
        (self.corpus / 'cases.json').write_text('[]')
        with self.assertRaisesRegex(ValueError, 'no applicable cases'):
            self.rows()


if __name__ == '__main__':
    unittest.main()
