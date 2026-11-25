#!/usr/bin/env python3
"""Every ignored test has a registered executable home.

An ignored test is a release-only case (`cfg_attr(debug_assertions, ignore ...)`, run by the release
tests), an AArch64 runtime case of the LLVM backend (//:core-llvm-aarch64-runtime),
or a body of the driver's native tests module (//:core-driver-native).
A registered subprocess worker runs through its ordinary bounded parent test.
"""
from pathlib import Path
import argparse
import re
import unittest

ROOT = Path(__file__).resolve().parents[2]
# These workers require parent-supplied input and cannot run alone in a gate.
# The aarch64 core::tests bodies need the aarch64 runtime archive; their one home
# is //:core-llvm-aarch64-runtime's exact selection in tools/buck2/sync.py.
AARCH64_RUNTIME = set(re.findall(r'"core::tests::(aarch64_\w+)"', next(
    line for line in (ROOT / 'tools/buck2/sync.py').read_text().splitlines() if 'name="core-llvm-aarch64-runtime"' in line)))
# //:core-driver-native (its shard-1 and openat2 shards) runs every ignored body under
# native::tests::, which is native.rs's tests module and the files it include!s.
_DRIVER = ROOT / 'crates/hirsel-core-driver/src'
_DRIVER_TESTS = (_DRIVER / 'native.rs').read_text().split('\nmod tests {', 1)[1]
assert any('name="core-driver-native", data=' in line for line in (ROOT / 'tools/buck2/sync.py').read_text().splitlines())
DRIVER_NATIVE = {'crates/hirsel-core-driver/src/native.rs'} | {
    f'crates/hirsel-core-driver/src/{name}' for name in re.findall(r'include!\("(\w+\.rs)"\)', _DRIVER_TESTS)}

ATTRIBUTE = re.compile(r'^[ \t]*#\[(ignore\b[^\]]*)\]|^[ \t]*#\[cfg_attr\(\s*([^,]+),\s*ignore\b', re.M)
FUNCTION = re.compile(r'\bfn\s+(\w+)')


def arguments(text, start):
    """The text between an opened macro delimiter at `start` and its matching close."""
    depth = 1
    for at in range(start, len(text)):
        depth += (text[at] in '([{') - (text[at] in ')]}')
        if depth == 0:
            return text[start:at]
    raise ValueError('unclosed macro invocation')


def ignored_declarations(text):
    """Resolve simple fn-name macro templates to their actual invocation names."""
    for attribute in ATTRIBUTE.finditer(text):
        function = re.search(r'\bfn\s+(\$?\w+)', text[attribute.end():])
        name = function[1]
        if name.startswith('$') and attribute[1] is not None:
            definition = list(re.finditer(r'macro_rules!\s+(\w+)\s*\{\s*\((.*?)\)\s*=>', text[:attribute.start()], re.S))[-1]
            macro, matcher = definition[1], definition[2]
            # The template's name is the k-th metavariable of the matcher. An invocation
            # supplies it as its k-th argument, or as the k-th field of each `(...)` group
            # when the matcher repeats `$((...)),+`.
            k = re.findall(r'\$(\w+):\w+', matcher).index(name[1:])
            grouped = matcher.lstrip().startswith('$((')
            names = []
            for invocation in re.finditer(r'\b' + re.escape(macro) + r'!\s*[(\[{]', text):
                body = arguments(text, invocation.end())
                for group in re.findall(r'\(([^()]*)\)', body) if grouped else [body]:
                    fields = [field.strip() for field in group.split(',')]
                    if len(fields) > k and re.fullmatch(r'\w+', fields[k]):
                        names.append(fields[k])
            if not names:
                raise ValueError(f'ignored macro has no registered invocations: {macro}')
            for name in names:
                yield attribute, name
        else:
            yield attribute, name


def spawned(sources, path, name):
    """A subprocess child: another body of its crate runs it by its qualified libtest name."""
    crate = path.split('/src/', 1)[0] + '/src/'
    return any(re.search(r'"(?:\w+::)+' + re.escape(name) + '"', text)
               for other, text in sources.items() if other.startswith(crate))


def orphans(sources):
    """`(path, test)` for each ignored test of `{path: text}` without a home."""
    found = []
    for path, text in sorted(sources.items()):
        for attribute, name in ignored_declarations(text):
            if attribute[1] is None and attribute[2].strip() == 'debug_assertions':
                continue
            if path == 'crates/hirsel-llvm/src/core/tests.rs' and name in AARCH64_RUNTIME:
                continue
            if path == 'crates/hirsel-core-program-tests/tests/llvm_source.rs' and name.startswith('aarch64_'):
                continue
            if path in DRIVER_NATIVE:
                continue
            if spawned(sources, path, name):
                continue
            found.append((path, name))
    return found


class IgnoredTests(unittest.TestCase):


    def test_every_ignored_test_in_the_workspace_runs_in_a_gate(self):
        sources = {str(p.relative_to(ROOT)): p.read_text() for p in ROOT.glob('crates/**/*.rs')}
        self.assertEqual(orphans(sources), [])



    def test_ignored_macro_templates_name_invocations_instead_of_the_next_helper(self):
        text = '\n'.join([
            'macro_rules! controls { ($name:ident) => {',
            '#[test]', '#[ignore = "manual"]', 'fn $name() {}', '} }',
            'controls!(first);', 'controls!(second);', 'fn ordinary_helper() {}'])
        self.assertEqual([name for _, name in ignored_declarations(text)], ['first', 'second'])
        self.assertEqual(orphans({'crates/example/controls.rs': text}), [
            ('crates/example/controls.rs', 'first'), ('crates/example/controls.rs', 'second')])

    def test_orphans_are_named_and_homes_are_not(self):
        sources = {
            'crates/a/tests/it/x.rs': '#[test]\n#[ignore = "manual"]\nfn misnamed() {}\n'
                                      '#[test]\n#[ignore]\nfn tool() {}\n'
                                      '#[test]\n#[cfg_attr(debug_assertions, ignore = "release")]\nfn release() {}\n'
                                      '#[test]\n#[cfg_attr(miri, ignore)]\nfn miri() {}\n'
        }
        self.assertEqual(orphans(sources), [('crates/a/tests/it/x.rs', 'misnamed'),
                                            ('crates/a/tests/it/x.rs', 'tool'),
                                            ('crates/a/tests/it/x.rs', 'miri')])


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--test-threads', type=int, choices=[1], default=1)
    parser.parse_args()
    unittest.main(argv=[__file__], verbosity=2)
