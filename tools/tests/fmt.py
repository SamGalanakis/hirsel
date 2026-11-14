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
        self.git('add', '.')
        self.git('commit', '-qm', 'base')
        self.base = self.git('rev-parse', 'HEAD').strip()
        self.git('update-ref', 'refs/remotes/origin/main', self.base)
        formatter = str(Path(os.environ.get('HIRSEL_TEST_CASE_FORMATTER', '/formatter-not-built')).resolve())
        buck = self.write('.buck2/bin/buck2', '''#!/usr/bin/env python3
import json, sys
if sys.argv[1] == 'uquery':
 print(json.dumps({'root//:test': {'crate_root': 'src/lib.rs', 'edition': '2024'}}))
else:
 print(json.dumps({'root//:hirsel-core-syntax-bin-hirsel-core-syntax': FORMATTER}))
'''.replace('FORMATTER', repr(formatter)))
        self.write('tools/buck2/sync.py', '')
        rustfmt = self.write('.buck2/bin/rustfmt', '#!/bin/sh\nexit 0\n')
        buck.chmod(0o755)
        rustfmt.chmod(0o755)
        for active in [patch.object(fmt, 'ROOT', self.root), patch.object(fmt, 'BUCK2', buck),
                       patch.object(fmt, 'RUSTFMT', rustfmt), patch.object(fmt, 'CONFIG', [])]:
            active.start()
            self.addCleanup(active.stop)

    def git(self, *args):
        return subprocess.check_output(['git', *args], cwd=self.root, text=True)

    def write(self, path, text):
        file = self.root / path
        file.parent.mkdir(parents=True, exist_ok=True)
        file.write_text(text)
        return file

    def run_fmt(self, *args):
        with patch.object(sys, 'argv', ['fmt.py', *args]), contextlib.redirect_stdout(io.StringIO()):
            return fmt.main()

    def test_noncanonical_changed_case_fails_both_lane_and_land_check_modes(self):
        case = self.write('core/front/cases/case.hirsel', 'fn f(){1}')
        before = case.read_bytes()
        for args in [[], ['--check', '--base', self.base]]:
            with self.subTest(args=args):
                self.assertEqual(self.run_fmt(*args), 1)
                self.assertEqual(case.read_bytes(), before, 'case diagnostics must not move during a check')

    def test_changed_canonical_case_does_not_check_unchanged_noncanonical_neighbor(self):
        self.write('core/front/cases/case.hirsel', 'fn g() -> u64 {\n    1\n}\n')
        self.assertEqual(self.run_fmt('--check'), 0)
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

    def test_core_headers_use_signature_mode_and_all_new_source_families_are_selected(self):
        paths = ['core/core/int.hirsel', 'core/std/int.hirsel',
                 'core/programs/m1/files/main.hirsel', 'core/langarena/sort.hirsel']
        for path in paths:
            self.write(path, 'pub fn f(x: u64) -> u64\n' if path.startswith('core/core/') else 'fn f() {}\n')
        self.assertEqual(fmt.changed_cases(), sorted(paths))
        self.assertEqual(self.run_fmt('--check'), 0)


if __name__ == '__main__':
    unittest.main()
