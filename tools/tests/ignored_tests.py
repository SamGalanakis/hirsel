#!/usr/bin/env python3
"""Runner audit regressions with the same Buck and source inputs as the land gate."""
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import check_test_runners as runners


class IgnoredTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        subprocess.run(['git', 'init', '-q', str(self.root)], check=True)
        self.write('BUCK', 'hirsel_test(name="example", crate_root="crates/example/src/lib.rs")\n')
        self.write('crates/example/src/lib.rs', '')

    def write(self, name, text):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)
        return path

    def test_nested_includes_use_the_enclosing_module_path(self):
        self.write('crates/example/src/lib.rs', '''pub(crate) mod outer {
            mod inner;
        }
''')
        self.write('crates/example/src/outer/inner.rs', 'include!("entry.rs");\n')
        self.write('crates/example/src/outer/entry.rs', 'include!("laws/cases.rs");\n')
        self.write('crates/example/src/outer/laws/cases.rs', '''#[test]
#[ignore = "native"]
fn shared_name() {}
''')
        self.write('tools/buck2/defs.bzl', '''def hirsel_core_llvm_native_test(selection):
    _rust_test(args=selection)
def hirsel_test():
    pass
''')
        with (self.root / 'BUCK').open('a') as buck:
            buck.write('hirsel_core_llvm_native_test(binary="example", selection=["outer::other::shared_name", "--ignored", "--exact"])\n')
        self.assertEqual(runners.problems(self.root), [
            'crates/example/src/outer/laws/cases.rs::outer::inner::shared_name (ignored, no runner passes --ignored)',
        ])
        buck = self.root / 'BUCK'
        buck.write_text(buck.read_text().replace('outer::other', 'outer::inner'))
        self.assertEqual(runners.problems(self.root), [])

    def test_an_ignored_child_needs_a_caller_that_passes_the_flag(self):
        path = self.write('crates/example/src/lib.rs', '''mod laws {
    #[test]
    #[ignore = "needs a scope"]
    fn child() {
        const LAW: &str = "laws::child";
        Command::new(exe).args([LAW, "--ignored", "--exact"]);
    }
}
''')
        self.assertEqual(runners.problems(self.root), [
            'crates/example/src/lib.rs::laws::child (ignored, no runner passes --ignored)',
        ])
        with path.open('a') as source:
            source.write('fn parent() { Command::new(exe).args(["laws::child", "--ignored", "--exact"]); }\n')
        self.assertEqual(runners.problems(self.root), [])

    def test_ignored_macro_templates_name_invocations_instead_of_the_next_helper(self):
        text = '\n'.join([
            'macro_rules! controls { ($name:ident) => {',
            '#[test]', '#[ignore = "manual"]', 'fn $name() {}', '} }',
            'controls!(first);', 'controls!(second);', 'fn ordinary_helper() {}'])
        found = runners.ignored_declarations(runners.blank_rust(text))
        self.assertEqual([name for name, _ in found], ['first', 'second'])

    def test_python_data_and_uncompiled_rust_files_do_not_count_as_runners(self):
        self.write('tools/stray.py', 'def test_stray(): pass\n')
        self.write('tools/runner.py', 'print("hello")\n')
        self.write('crates/example/src/stray.rs', '#[test]\nfn stray() {}\n')
        self.write('test_stray.hirsel', '@test\nfn stray() {}\n')
        with (self.root / 'BUCK').open('a') as buck:
            buck.write('hirsel_script_test(script="tools/runner.py", data=["tools/stray.py"])\n')
        self.assertEqual(runners.problems(self.root), [
            'crates/example/src/stray.rs (#[test] file no harness compiles)',
            'test_stray.hirsel (@test outside [tests.sources])',
            'tools/stray.py (Python tests without a runner)',
        ])

    def test_a_subpackage_script_label_names_its_runner(self):
        self.write('tools/fuzz/test_cases.py', 'def test_case(): pass\n')
        with (self.root / 'BUCK').open('a') as buck:
            buck.write('hirsel_script_test(script="//tools/fuzz:test_cases.py", data=[])\n')
        self.assertEqual(runners.problems(self.root), [])


if __name__ == '__main__':
    unittest.main()
