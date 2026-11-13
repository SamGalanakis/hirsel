#!/usr/bin/env python3
"""Behavioral fixtures for milestone stage and exact-observation reporting."""
import json
from pathlib import Path
import tempfile
import unittest

from m1 import census_table, gap_sites, gap_table, inputs_for, run, table


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
assert args[args.index('--dir')+1] == '.'
assert [args[i+1] for i, value in enumerate(args) if value == '--grant'] == ['Stdout', 'Stderr', 'Tasks']
output=pathlib.Path(args[args.index('-o')+1])
pathlib.Path(__file__).with_suffix('.count').open('a').write('build\\n')
output.write_text("#!/usr/bin/env python3\\nimport os,pathlib,sys\\nprint(pathlib.Path(sys.argv[1]).read_text().strip()+os.environ['M1_CASE'])\\npathlib.Path('result').write_bytes(b'done')\\nsys.exit(7)\\n")
output.chmod(0o755)
print(json.dumps({'accepted':True, 'native':{'executable':str(output)}, 'diagnostics':[], 'incomplete':[], 'reachable':{'bodies': 2, 'declarations': 3}}))
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

    def test_empty_output_and_wrapper_only_closures_are_not_built(self):
        original = self.compiler.read_text()
        # Even exact output cannot qualify a wrapper-only closure as the workload.
        self.compiler.write_text(original.replace("'bodies': 2", "'bodies': 1"))
        row = self.rows()[0]
        self.assertFalse(row['built'])
        self.assertEqual(row['passed'], 0)
        self.assertIn('closure has 1 bodies', row['stopped'])
        self.assertTrue(row['artifact_linked'])
        # An emitted artifact that runs but prints nothing also loses built status.
        # Only the second case is empty, so earlier successes must be discarded.
        emit = "print(pathlib.Path(sys.argv[1]).read_text().strip()+os.environ['M1_CASE'])"
        conditional = emit + " if not sys.argv[1].endswith('second') else None"
        self.compiler.write_text(original.replace(emit, conditional))
        row = self.rows()[0]
        self.assertFalse(row['built'])
        self.assertEqual(row['passed'], 0)
        self.assertEqual(row['first_failure'], 'second')
        self.assertIn('empty stdout against nonempty EXPECTED', row['stopped'])
        self.assertTrue(row['artifact_linked'])
        self.assertIn('executable', row['native'])
        (self.corpus / 'EXPECTED/first.stdout').write_bytes(b'wrong')
        row = self.rows()[0]
        self.assertFalse(row['built'])
        self.assertEqual(row['passed'], 0)
        self.assertEqual(row['first_failure'], 'first')
        self.assertIn('runner assertion at second', row['stopped'])
        # Error cases with an empty oracle remain legitimate observations.
        for name in ['first', 'second']:
            (self.corpus / f'EXPECTED/{name}.stdout').write_bytes(b'')
        self.compiler.write_text(original.replace(emit, 'pass'))
        row = self.rows()[0]
        self.assertTrue(row['built'])
        self.assertEqual(row['passed'], 2)

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
        self.compiler.write_text("#!/usr/bin/env python3\nimport sys\nprint('checker panicked', file=sys.stderr)\nraise SystemExit(101)\n")
        row = self.rows()[0]
        self.assertFalse(row['built'])
        self.assertIn('exit 101', row['stopped'])
        self.assertIn('census unavailable', census_table([row]))
        self.assertIn('| cranelift | ? |', gap_table([row]))
        self.assertNotIn('| 0 |', gap_table([row]))
        selected = inputs_for(self.manifest, self.manifest['entries'][0], self.root)
        self.assertEqual([p['package']['name'] for p in selected['packages']], ['example'])
        self.assertEqual([s['modules'] for s in selected['packages'][0]['sources']], [['main'], ['matched']])

    def test_distinct_gap_sites_include_stops_after_a_definitions_first_stop(self):
        def gap(message, start):
            return dict(message=message, primary=dict(module='example@1/main', span=dict(start=start, end=start+1)))
        first = gap('resource literals are not built', 5)
        later = gap('generic definitions are not built', 12)
        another = gap('generic definitions are not built', 20)
        rows = [dict(program='example', variant='idiomatic', engine='cranelift', checked=False, built=False,
                     passed=0, cases=2, reachable_bodies=2, reachable_declarations=3,
                     census={'resource literals are not built': 1},
                     incomplete=[first, later, later, another], diagnostics=[])]
        self.assertEqual([s['slice'] for s in gap_sites(rows[0]['incomplete'])], ['A1', 'B', 'B'])
        self.assertIn('resource literals are not built | 1 | 1 |', census_table(rows))
        self.assertIn('generic definitions are not built | 0 | 2 |', census_table(rows))
        self.assertIn('| cranelift | 1 | 2 | 0 | 0 | 0 | 0 | 0 | 0 | 3 | 0 |', gap_table(rows))
        # Builtin container and channel gaps need their source callee to route correctly.
        for expression, owner in [('list.len(xs)', 'A1'), ('channel.try_send(tx, x)', 'writers/sequences/regions/tasks')]:
            operation = gap('this generic core operation is not built', 0)
            operation['primary']['span']['end'] = len(expression)
            sites = gap_sites([operation], {'example@1/main': expression.encode()})
            self.assertEqual(sites[0]['slice'], owner)
        for message, owner in [
            ('aggregate constants are not built', 'A1'),
            ('Unique runtime identity binding is not specified (FOR FORM)', 'A1'),
            ('provider extern rows are not built (FOR FORM)', 'writers/sequences/regions/tasks'),
            ('a String view of a Text returned from its function is not built (Q38)', 'writers/sequences/regions/tasks'),
        ]:
            self.assertEqual(gap_sites([gap(message, 0)])[0]['slice'], owner)
        missing = dict(rule='instances', construct='native body is unavailable',
                       primary=dict(module='core@1/string', span=dict(start=410, end=418)))
        self.assertEqual(gap_sites([], first_stops=[missing])[0]['slice'], 'A1')
        missing['primary']['module'] = 'core@1/host'
        self.assertEqual(gap_sites([], first_stops=[missing])[0]['slice'], 'writers/sequences/regions/tasks')
        # Same source span with a different gap message remains one site per slice.
        rows[0]['incomplete'].append(gap('calls to generic functions are not built', 12))
        self.assertEqual(len(gap_sites(rows[0]['incomplete'])), 3)
        self.assertIn('calls to generic functions are not built | 0 | 1 |', census_table(rows))
        self.assertEqual(gap_sites([gap('iteration of this stable or map representation is not built', 40)])[0]['slice'], 'E')
        rows[0]['incomplete'].append(gap('new unassigned construct', 30))
        self.assertEqual(gap_sites(rows[0]['incomplete'])[-1]['slice'], 'unassigned')

    def test_empty_selection_is_not_success(self):
        with self.assertRaisesRegex(ValueError, 'no entries'):
            run(self.compiler, self.root / 'runtime.a', ['llvm'], self.path, self.root, self.root / 'store', only=['absent'])
        (self.corpus / 'cases.json').write_text('[]')
        with self.assertRaisesRegex(ValueError, 'no applicable cases'):
            self.rows()


if __name__ == '__main__':
    unittest.main()
