#!/usr/bin/env python3
"""Changed corpus cases share the formatter gate without sweeping old debt."""
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from types import SimpleNamespace
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location('buck2_fmt', ROOT / 'tools/buck2/fmt.py')
fmt = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(fmt)


class ChangedCases(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.git('init', '-q', '-b', 'main')
        self.git('config', 'user.name', 'test')
        self.git('config', 'user.email', 'test@example.invalid')
        self.write('.gitignore', '.buck2/\n')
        self.write('src/lib.rs', '')
        self.write('core/front/cases/case.hirsel', 'fn f() -> u64 {\n    1\n}\n')
        self.write('core/front/cases/deleted.hirsel', 'fn f() {}\n')
        self.write('core/front/cases/legacy.hirsel', 'fn old(){1}')
        fake = self.write('.buck2/bin/fake-case-formatter', '''#!/usr/bin/env python3
# The fixture's formatter oracle: canonical hirsel ends in a newline and carries no
# compact `){` or mid-line doubled spaces. Selection logic is what these tests check.
import re
import sys
status = 0
for path in [arg for arg in sys.argv[1:] if not arg.startswith('--')]:
    text = open(path).read()
    if not text.endswith('\\n') or re.search(r'\\S  ', text) or '){' in text:
        print(path)
        status = 1
sys.exit(status)
''')
        fake.chmod(0o755)
        formatter = str(Path(os.environ.get('HIRSEL_TEST_CASE_FORMATTER', str(fake))).resolve())
        self.formatter_log = self.root / 'formatter-invocations.jsonl'
        wrapper = self.write('.buck2/bin/case-formatter', f'''#!/usr/bin/env python3
import json, os, sys
with open({str(self.formatter_log)!r}, 'a') as log:
 log.write(json.dumps(sys.argv[1:]) + '\\n')
os.execv({formatter!r}, [{formatter!r}, *sys.argv[1:]])
''')
        wrapper.chmod(0o755)
        compiler = self.write('.buck2/bin/hirsel', '#!/bin/sh\nexit 0\n')
        compiler.chmod(0o755)
        buck = self.write('.buck2/bin/buck2', '''#!/usr/bin/env python3
import json, os, sys
if sys.argv[1] == 'uquery':
 print(json.dumps({'root//:test': {'crate_root': 'src/lib.rs', 'edition': '2024'}}))
elif sys.argv[1] == 'test':
 assert sys.argv[2:] == ['//:hirsel-core-rt-archives-darwin-import-law']
 print('Darwin archive import law')
 sys.exit(int(os.environ.get('FIXTURE_IMPORT_STATUS', '0')))
else:
 target = sys.argv[-1]
 executables = {'root//:hirsel-core-syntax-bin-hirsel-core-syntax': FORMATTER,
                'root//:hirsel-core-driver-bin-hirsel': COMPILER}
 print(json.dumps({target: executables[target]}))
'''.replace('FORMATTER', repr(str(wrapper))).replace('COMPILER', repr(str(compiler))))
        self.write('tools/buck2/sync.py', '')
        self.write('tools/check_doc_refs.py', '')
        self.write('editors/tree-sitter-hirsel/check-local', '')
        self.write('core/std/API-INDEX.md', 'current')
        self.write('core/tools/law_board.py', '')
        self.write('core/tools/check_std_api_recipes.py', '')
        self.write('core/tools/std_api_index.py', '''import os, subprocess, sys
from pathlib import Path

assert sys.argv[1:] == ['--check']
subprocess.run([os.environ['HIRSEL']], check=True)
index = Path(__file__).resolve().parents[1] / 'std/API-INDEX.md'
sys.exit(index.read_text() != 'current')
''')
        rustfmt = self.write('.buck2/bin/rustfmt', '#!/bin/sh\nexit 0\n')
        buck.chmod(0o755)
        rustfmt.chmod(0o755)
        self.git('add', '.')
        self.git('commit', '-qm', 'base')
        self.base = self.git('rev-parse', 'HEAD').strip()
        self.git('update-ref', 'refs/remotes/origin/main', self.base)
        self.preflights = dict(check_file_lengths=lambda root: True, check_boundaries=lambda root: True,
                               check_target=lambda root: True, check_runners=lambda root: True,
                               apple_check=SimpleNamespace(rust_changed=lambda: True, check=lambda: 0))
        for active in [patch.multiple(fmt, **self.preflights),
                       patch.multiple(fmt, ROOT=self.root, BUCK2=buck, RUSTFMT=rustfmt)]:
            active.start()
            self.addCleanup(active.stop)

    def git(self, *args):
        return subprocess.check_output(['git', *args], cwd=self.root, text=True)

    def write(self, path, text):
        file = self.root / path
        file.parent.mkdir(parents=True, exist_ok=True)
        file.write_text(text)
        return file

    def run_fmt_result(self, *args):
        output = io.StringIO()
        read, write = os.pipe()
        duplicate = os.dup(1)
        try:
            os.dup2(write, 1)
            with patch.object(sys, 'argv', ['fmt.py', *args]), contextlib.redirect_stdout(output):
                status = fmt.main()
        finally:
            os.dup2(duplicate, 1)
            os.close(duplicate)
            os.close(write)
        chunks = []
        while chunk := os.read(read, 65536):
            chunks.append(chunk)
        os.close(read)
        return status, output.getvalue() + b''.join(chunks).decode(errors='replace')

    def run_fmt(self, *args):
        return self.run_fmt_result(*args)[0]

    def formatter_invocations(self):
        if not self.formatter_log.exists():
            return []
        return [json.loads(line) for line in self.formatter_log.read_text().splitlines()]

    def formatter_invocations_with(self, path):
        return [args for args in self.formatter_invocations() if path in args]


    def test_documentation_runs_parser_doc_refs_and_metadata_without_darwin_import_law(self):
        self.write('tools/check_doc_refs.py', 'print("doc references")\n')
        self.write('editors/tree-sitter-hirsel/check-local', 'print("parser corpus")\n')
        self.write('core/tools/check_std_api_recipes.py', 'print("API signatures")\n')
        self.git('add', '.')
        self.git('commit', '-qm', 'checks')
        self.git('update-ref', 'refs/remotes/origin/main', 'HEAD')
        self.write('docs/spec/memory.md', 'Updated specification\n')
        self.write('tools/buck2/toolchains/README.md', 'Updated toolchain documentation\n')
        self.git('add', '.')
        status, output = self.run_fmt_result('--check')
        self.assertEqual(status, 0)
        for check in ('doc references', 'parser corpus', 'API signatures'):
            self.assertIn(check, output)
        self.assertNotIn('Darwin archive import law', output)

    def test_archive_inputs_run_darwin_import_law_and_propagate_its_failure(self):
        paths = ('src/lib.rs', 'crates/hirsel-core-rt/src/runtime.c',
                 'tools/buck2/runtime_parts.py', 'Cargo.toml', 'tools/buck2/defs.bzl',
                 'tools/native-toolchain/darwin/libSystem.symbols')
        for index, path in enumerate(paths):
            with self.subTest(path=path):
                self.write(path, 'changed\n')
                args = ('--check',) if index % 2 == 0 else ('--darwin-import-law',)
                with patch.dict(os.environ, {'FIXTURE_IMPORT_STATUS': '9'}):
                    status, output = self.run_fmt_result(*args)
                self.assertEqual((status, output.strip()), (9, 'Darwin archive import law'))
                if path == 'src/lib.rs':
                    self.git('checkout', 'HEAD', '--', path)
                else:
                    (self.root / path).unlink()

    def test_noncanonical_changed_case_fails_with_and_without_check(self):
        case = self.write('core/front/cases/case.hirsel', 'fn f(){1}')
        before = case.read_bytes()
        for args in [[], ['--check']]:
            with self.subTest(args=args):
                self.assertEqual(self.run_fmt(*args), 1)
                self.assertEqual(case.read_bytes(), before, 'case diagnostics must not move during a check')

    def test_changed_canonical_case_does_not_check_unchanged_noncanonical_neighbor(self):
        self.write('core/front/cases/case.hirsel', 'fn g() -> u64 {\n    1\n}\n')
        self.assertEqual(self.run_fmt('--check'), 0)
        self.assertEqual(self.formatter_invocations(), [['--check', 'core/front/cases/case.hirsel']])
        self.assertEqual((self.root / 'core/front/cases/legacy.hirsel').read_text(), 'fn old(){1}')

    def test_selection_includes_branch_index_worktree_and_untracked_cases_only(self):
        self.write('core/front/cases/branch.hirsel', 'fn f(){1}')
        self.git('add', 'core/front/cases/branch.hirsel')
        self.git('commit', '-qm', 'branch case')
        self.write('src/lib.rs', '// later unrelated commit\n')
        self.git('add', 'src/lib.rs')
        self.git('commit', '-qm', 'later unrelated commit')
        self.write('core/front/cases/case.hirsel', 'fn f(){2}')
        self.write('core/front/cases/staged.hirsel', 'fn f(){3}')
        self.git('add', 'core/front/cases/staged.hirsel')
        self.write('core/front/cases/untracked.hirsel', 'fn f(){4}')
        (self.root / 'core/front/cases/deleted.hirsel').unlink()
        for file in ['core/front/cases/reports/old.hirsel', 'stdlib/std@1/string.hir',
                     'native-tests/library/format-fixture.hirsel']:
            self.write(file, 'fn old(){1}')
        self.assertEqual(fmt.changed_cases(), [
            'core/front/cases/branch.hirsel', 'core/front/cases/case.hirsel',
            'core/front/cases/staged.hirsel', 'core/front/cases/untracked.hirsel'])

    def test_no_changed_cases_do_not_build_a_formatter(self):
        self.assertEqual(self.run_fmt('--check'), 0)
        self.assertEqual(self.formatter_invocations(), [])

    def test_stale_std_index_fails_without_changing_it(self):
        index = self.write('core/std/API-INDEX.md', 'stale')
        self.assertEqual(self.run_fmt('--check'), 1)
        self.assertEqual(index.read_text(), 'stale')

    def test_refusal_fixtures_are_preserved_even_when_their_syntax_parses(self):
        for text in ['// expect: refuse LANG-5 at 2:7\nfn f(\n',
                     '// expect: refuse LANG-15 at 2:16\nfn f(){continue}\n',
                     '// expect: gap OWN-1 at 2:7\nfn f(){}\n']:
            with self.subTest(text=text):
                path = self.write('core/front/cases/case.hirsel', text)
                self.assertEqual(self.run_fmt('--check'), 0)
                self.assertEqual(path.read_text(), text)

    def test_unmarked_syntax_error_is_refused_without_modification(self):
        path = self.write('core/std/broken.hirsel', 'fn broken(')
        self.assertEqual(self.run_fmt('--check'), 1)
        self.assertEqual(path.read_text(), 'fn broken(')

    def test_modified_program_source_is_reported(self):
        path = 'core/programs/store/cache.hirsel'
        self.write(path, 'fn f(){1}')
        status, output = self.run_fmt_result('--check')
        self.assertEqual(status, 1)
        self.assertIn(path, output)
        self.assertTrue(any('--signatures' not in args for args in self.formatter_invocations_with(path)))

    def test_modified_program_refusal_fixture_is_skipped(self):
        path = 'core/programs/store/refusal.hirsel'
        self.write(path, '// expect: refuse LANG-5 at 2:7\nfn f(\n')
        status, output = self.run_fmt_result('--check')
        self.assertEqual((status, output), (0, ''))
        self.assertEqual(self.formatter_invocations_with(path), [])

    def test_modified_foreign_source_is_checked_in_signature_mode(self):
        path = 'core/programs/av1/native.hirsel'
        self.write('core/programs/av1/hirsel.hcfg', '''pkg_v1.Package(
    name: "hirsel.ai/av1",
    version: "1.0.0",
    charge: ChargeV1,
    sources: [("native.hirsel", "native")],
    dependencies: [],
    foreign: Some([("native", Native(symbols: "hfl_", archives: []))]),
)
''')
        self.write(path, 'pub  fn  f(x:u64)->u64\n')
        status, output = self.run_fmt_result('--check')
        self.assertEqual(status, 1)
        self.assertIn(path, output)
        invocations = self.formatter_invocations_with(path)
        self.assertTrue(any('--signatures' in args for args in invocations), invocations)
        self.assertFalse(any('--signatures' not in args for args in invocations), invocations)

    def test_core_headers_use_signature_mode_and_all_new_source_families_are_selected(self):
        paths = ['core/core/int.hirsel', 'core/std/int.hirsel',
                 'core/programs/m1/files/main.hirsel', 'core/langarena/sort.hirsel']
        for path in paths:
            self.write(path, 'pub fn f(x: u64) -> u64\n' if path.startswith('core/core/') else 'fn f() {}\n')
        self.assertEqual(fmt.changed_cases(), sorted(paths))
        self.assertEqual(self.run_fmt('--check'), 0)


if __name__ == '__main__':
    unittest.main()
