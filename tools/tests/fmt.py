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
        self.write('conformance/LANG/case.hir', 'fn f() -> u8 {\n  1u8\n}\n')
        self.write('conformance/LANG/deleted.hir', 'fn f() {}\n')
        self.write('conformance/LANG/legacy.hir', 'fn old(){1u8}')
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
 print(json.dumps({'root//:hirsel-conformance-bin-hirsel-conformance': FORMATTER}))
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
        case = self.write('conformance/LANG/case.hir', 'fn f(){1u8}')
        before = case.read_bytes()
        for args in [[], ['--check', '--base', self.base]]:
            with self.subTest(args=args):
                self.assertEqual(self.run_fmt(*args), 1)
                self.assertEqual(case.read_bytes(), before, 'case diagnostics must not move during a check')

    def test_changed_canonical_case_does_not_check_unchanged_noncanonical_neighbor(self):
        self.write('conformance/LANG/case.hir', 'fn g() -> u8 {\n  1u8\n}\n')
        self.assertEqual(self.run_fmt('--check'), 0)
        self.assertEqual((self.root / 'conformance/LANG/legacy.hir').read_text(), 'fn old(){1u8}')

    def test_selection_includes_branch_index_worktree_and_untracked_cases_only(self):
        self.write('conformance/LANG/branch.hir', 'fn f(){1u8}')
        self.git('add', 'conformance/LANG/branch.hir')
        self.git('commit', '-qm', 'branch case')
        self.write('src/lib.rs', '// later unrelated commit\n')
        self.git('add', 'src/lib.rs')
        self.git('commit', '-qm', 'later unrelated commit')
        self.write('conformance/LANG/case.hir', 'fn f(){2u8}')
        self.write('conformance/LANG/staged.hir', 'fn f(){3u8}')
        self.git('add', 'conformance/LANG/staged.hir')
        self.write('conformance/LANG/untracked.hir', 'fn f(){4u8}')
        (self.root / 'conformance/LANG/deleted.hir').unlink()
        for file in ['conformance/LANG/reports/old.hir', 'stdlib/std@1/string.hir',
                     'native-tests/library/format-fixture.hir']:
            self.write(file, 'fn old(){1u8}')
        self.assertEqual(fmt.changed_cases(), [
            'conformance/LANG/branch.hir', 'conformance/LANG/case.hir',
            'conformance/LANG/staged.hir', 'conformance/LANG/untracked.hir'])

    def test_no_changed_cases_do_not_build_a_formatter(self):
        self.assertEqual(self.run_fmt('--check'), 0)

    def test_exact_expected_syntax_rejection_remains_exempt(self):
        self.write('conformance/old-spec/language.md', '### LANG-1: Syntax\n\n### LANG-7: Types\n')
        self.write('conformance/LANG/case.hir',
                   '// rule: LANG-1\n// expect: reject LANG-1 at line 3\nfn f<T>() {}\n')
        self.assertEqual(self.run_fmt('--check'), 0)
        self.write('conformance/LANG/case.hir',
                   '// rule: LANG-1\n// expect: reject LANG-7 at line 3\nfn f<T>() {}\n')
        self.assertEqual(self.run_fmt('--check'), 1)


if __name__ == '__main__':
    unittest.main()
