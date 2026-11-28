#!/usr/bin/env python3
"""Keep tests and references to test-only modules out of production sources."""

import argparse
from bisect import bisect_right
from pathlib import Path
import re
import sys
import subprocess

from check_file_lengths import git_paths, snapshot


sys.path.insert(0, str(Path(__file__).resolve().parent / 'toolchain-dist'))
import hcfg

PROGRAMS = Path('core/programs')
TEST_ONLY = {'store.fixture', 'store.signup', 'runner.fixture'}
# Test helpers other packages' tests name, which another package's [tests.sources] cannot
# reach: runner L33 (`serve_bounded_and_settled`) runs store.law_call_reserve over test_db, and
# the runner, signup and harness laws open stores with runner.triggers.law_options.
ALLOW_LIST = {
    ('runner/triggers.hirsel', 'test-function'): (
        'law_options'
    ),
    ('store/store.hirsel', 'test-function'): (
        'law_call_reserve test_db'
    ),
}

IDENT = r'[A-Za-z_][A-Za-z_0-9]*'
IGNORED = re.compile(r'(?P<comment>//[^\n]*)|\b(?:br|r)(?P<hashes>\#*)"[^\n]*?"(?P=hashes)'
                     r'''|"(?:\\[^\n]|[^"\\\n])*"|'(?:\\[^\n]|[^'\\\n])*' ''', re.VERBOSE)
FUNCTION = re.compile(rf'\bfn\s+((?:test_|law_)[A-Za-z_0-9]*)\b')
ATTRIBUTE = re.compile(rf'@\s*test\b(?:\s+(?:pub\s+)?fn\s+({IDENT}))?')
REFERENCE = re.compile(rf'(?P<field>(?<!\.)\.(?!\.)\s*)?\b{IDENT}(?:\s*\.\s*{IDENT})+')


def source_violations(source, modules):
    # Literal bodies are opaque tokens. Comments are whitespace, even inside a dotted path.
    source = IGNORED.sub(lambda match: (' ' if match['comment'] else '\0') * len(match[0]), source)
    newlines = [match.start() for match in re.finditer('\n', source)]
    for match in FUNCTION.finditer(source):
        yield bisect_right(newlines, match.start()) + 1, 'test-function', match[1]
    for match in ATTRIBUTE.finditer(source):
        yield bisect_right(newlines, match.start()) + 1, 'test-attribute', match[1] or '@test'
    for match in REFERENCE.finditer(source):
        if match['field']:
            continue
        path = tuple(re.split(r'\s*\.\s*', match[0]))
        for size in range(len(path), 0, -1):
            module = path[:size]
            if module in modules:
                if modules[module]:
                    yield bisect_right(newlines, match.start()) + 1, 'test-only-module', '.'.join(module)
                break


def violations(root, staged=False):
    if staged:
        paths = git_paths(root, 'ls-files', '-z', '--', str(PROGRAMS))
        manifest_paths = {path for path in paths if Path(path).parent.parent == PROGRAMS
                          and Path(path).name == 'hirsel.hcfg'}
    else:
        manifest_paths = {str(path.relative_to(root)) for path in (root / PROGRAMS).glob('*/hirsel.hcfg')}
    revision = ':0' if staged else None
    manifests = []
    qualified = {}
    source_paths = set()
    for path, data in sorted(snapshot(root, revision, manifest_paths).items()):
        manifest = hcfg.project('/dev/stdin', 'pkg_v1.Package', input_bytes=data)['value']
        package = manifest['name']
        sources = dict(manifest['sources'])
        test_sources = dict((hcfg.option(manifest['tests']) or {}).get('sources', []))
        local = {tuple(module.split('.')): True for module in test_sources.values()}
        local.update((tuple(module.split('.')), False) for module in sources.values())
        crash_package = Path(path).parent.name == 'store-crash'
        if crash_package:
            qualified[package,] = True
        for module, test_only in local.items():
            external = module if module == (package,) else (package, *module)
            qualified[external] = test_only or crash_package
        paths = {str(Path(path).parent / source) for source in sources}
        manifests.append((package, paths, local))
        source_paths.update(paths)

    qualified.update((tuple(module.split('.')), True) for module in TEST_ONLY)
    sources = snapshot(root, revision, source_paths)
    for package, paths, local in manifests:
        modules = qualified | {(package,): False} | local
        modules.update((tuple(module.split('.')[1:]), True) for module in TEST_ONLY
                       if module.startswith(f'{package}.'))
        for path in sorted(paths):
            if path not in sources:
                raise ValueError(f'{path}: missing [sources] file')
            for line, rule, symbol in source_violations(sources[path].decode(), modules):
                yield path, line, rule, symbol


def check(root, staged=False):
    remaining = {(str(PROGRAMS / path), rule, symbol)
                 for (path, rule), symbols in ALLOW_LIST.items() for symbol in symbols.split()}
    ok = True
    try:
        for path, line, rule, symbol in sorted(violations(root, staged)):
            key = path, rule, symbol
            if key in remaining:
                remaining.remove(key)
                continue
            print(f'{path}:{line}: {rule}: {symbol} in [sources]. Move to [tests.sources].',
                  file=sys.stderr)
            ok = False
    except (ValueError, KeyError, OSError) as error:
        print(f'check_boundaries: {error}', file=sys.stderr)
        return False
    for path, rule, symbol in sorted(remaining):
        print(f'{path}: {rule}: {symbol} no longer violates the boundary. '
              'Remove it from ALLOW_LIST in tools/check_boundaries.py.', file=sys.stderr)
    return ok and not remaining


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--staged', action='store_true', help='check the Git index')
    sys.exit(0 if check(Path(__file__).resolve().parents[1], parser.parse_args().staged) else 1)
