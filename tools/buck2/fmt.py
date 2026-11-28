#!/usr/bin/env python3
"""rustfmt every first-party crate with the pinned toolchain, as `cargo fmt`.

`--check` reports files that are not formatted and fails, as `cargo fmt --check`.
rustfmt starts from each target's root file and follows its modules.
Changed new-language sources are checked, never rewritten. Documentation references
and the std operation index must match the code. Package law locks must be current:
`hirsel laws check`, with the compiler built from this tree and std from core/, runs on
each lock package a change since origin/main can move (core/tools/law_board.py).
Changed source files must fit
their line limits, production sources must keep test code out, and platform decisions
must be exhaustive on the target with host `cfg`s only in host seams. A Rust change
must pass the Apple target checks (tools/apple_check.py). The Darwin runtime
must link against the shipped link kit when archive or link-kit inputs change.
Use `--darwin-import-law` to run that check by itself during a lint fallback.
The editor parser checks the core and
spec corpus. Explicit
expectation comments exclude refusal/gap fixtures to preserve diagnostic coordinates.
"""

import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile


ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'tools'))
from check_boundaries import check as check_boundaries
from check_file_lengths import check_changed as check_file_lengths
from check_target import check as check_target
from check_test_runners import check as check_runners
import apple_check
import hcfg

BUCK2 = ROOT / '.buck2/bin/buck2'
RUSTFMT = ROOT / 'tools/buck2/toolchains/rust-files/bin/rustfmt'


SOURCE_ROOTS = ('core/core', 'core/std', 'core/front/cases', 'core/programs/m1', 'core/programs/m2b',
                'core/langarena')
PROGRAM_ROOT = 'core/programs'
BYTE_SENSITIVE_FIXTURES = {'core/front/fixtures/slots-needs-drop.hirsel'}
FIXTURE_EXPECTATION = re.compile(r'^\s*//\s*expect:\s*(?:refuse|gap|resource)\b', re.MULTILINE)
DARWIN_IMPORT_INPUTS = (
    '*.rs', '*.c', '*.cc', '*.cpp', '*.cxx', '*.h', '*.hh', '*.hpp', '*.S', '*.s', '*.inc',
    '*.a', 'Cargo.toml', '**/Cargo.toml', 'Cargo.lock', '.cargo', 'rust-toolchain.toml',
    'BUCK', '**/BUCK', '*.bzl', '.buckconfig', '.buckroot',
    'tools/native-toolchain/darwin', 'tools/native-toolchain/apple-targets.json',
    'tools/native-toolchain/llvm-23.lock', 'tools/buck2/native-toolchain', 'tools/buck2/toolchains',
    'tools/buck2/runtime_parts.py', 'tools/buck2/build_script_inputs.py', 'tools/buck2/cargo_units.py',
    'tools/buck2/sync.py', 'tools/buck2/distribution_gates.py', 'tools/buck2/production-features.json',
    'tools/buck2/pins.json', 'tools/buck2/toolchain-lock.json', 'tools/buck2/llvm-cxx-headers-lock.json',
    'tools/buck2/prelude_overlay.py', 'tools/toolchain-dist/product.hcfg',
    ':(exclude)*.md',
)


def is_refusal_fixture(path):
    """Coordinate-bearing refusal/gap fixtures are input bytes, not formatting targets."""
    return bool(FIXTURE_EXPECTATION.search((ROOT / path).read_text(errors='replace')))


def under(path, root):
    parts = Path(path).parts
    root_parts = Path(root).parts
    return parts[:len(root_parts)] == root_parts


def git_output(*args):
    return subprocess.check_output(['git', *args], cwd=ROOT).decode('utf-8', 'surrogateescape')


def check_darwin_import_law():
    base = git_output('merge-base', 'HEAD', 'origin/main').strip()
    changed = (git_output('diff', '--name-only', '--no-renames', '-z', base, '--', *DARWIN_IMPORT_INPUTS)
               or git_output('ls-files', '--others', '--exclude-standard', '-z', '--', *DARWIN_IMPORT_INPUTS))
    if not changed:
        return 0
    return subprocess.run(
        [str(BUCK2), 'test', '//:hirsel-core-rt-archives-darwin-import-law'], cwd=ROOT).returncode


def manifest_paths():
    paths = set(git_output('ls-files', '-z', '--', '**/hirsel.hcfg').split('\0'))
    paths.update(git_output('ls-files', '--others', '--exclude-standard', '-z', '--', '**/hirsel.hcfg').split('\0'))
    return sorted(path for path in paths if path and (ROOT / path).is_file())


def declared_foreign_sources():
    """Every source file its package manifest declares as a foreign module."""
    foreign = set()
    for manifest in manifest_paths():
        projection = hcfg.project(ROOT / manifest)
        document = projection['value']
        if projection['root'] == 'pkg_v1.Workspace':
            document = hcfg.option(document['root_package'])
        if not document:
            continue
        for module, _ in hcfg.option(document['foreign']) or []:
            matches = [source for source, declared in document['sources'] if declared == module]
            if len(matches) == 1:
                foreign.add(str(Path(manifest).parent / matches[0]))
    return foreign


def covered_case(path, foreign):
    return (any(under(path, root) for root in SOURCE_ROOTS) or under(path, PROGRAM_ROOT)
            or path in foreign)


def changed_cases():
    """Branch, index, worktree and new cases; never sweep unrelated main debt."""
    base = git_output('merge-base', 'HEAD', 'origin/main').strip()
    foreign = declared_foreign_sources()
    selected = [*SOURCE_ROOTS, PROGRAM_ROOT, *sorted(foreign)]
    paths = set(git_output('diff', '--name-only', '--diff-filter=ACMR', '-z', base, '--', *selected).split('\0'))
    paths.update(git_output('ls-files', '--others', '--exclude-standard', '-z', '--', *selected).split('\0'))
    return sorted(path for path in paths if Path(path).suffix == '.hirsel'
                  and covered_case(path, foreign)
                  and path not in BYTE_SENSITIVE_FIXTURES
                  and 'reports' not in Path(path).parts and (ROOT / path).is_file()
                  and not is_refusal_fixture(path))


def build_executable(target):
    built = subprocess.check_output([str(BUCK2), 'build', '--show-json-output', target],
                                    cwd=ROOT, text=True)
    return ROOT / json.loads(built)[target]


def check_compiler_metadata():
    compiler = build_executable('root//:hirsel-core-driver-bin-hirsel')
    with tempfile.TemporaryDirectory(prefix='hirsel-api-index-') as work:
        index = subprocess.run([sys.executable, str(ROOT / 'core/tools/std_api_index.py'), '--check'],
                               cwd=work, env=dict(os.environ, HIRSEL=str(compiler)))
    if index.returncode:
        return index.returncode
    recipes = subprocess.run(
        [sys.executable, str(ROOT / 'core/tools/check_std_api_recipes.py'), '--compiler', str(compiler)],
        cwd=ROOT)
    return recipes.returncode or subprocess.run(
        [sys.executable, str(ROOT / 'core/tools/law_board.py'), 'check', '--compiler', str(compiler),
         '--changed-since', 'origin/main'],
        cwd=ROOT).returncode


def check_cases(paths):
    if not paths:
        return 0
    executable = build_executable('root//:hirsel-core-syntax-bin-hirsel-core-syntax')
    foreign = declared_foreign_sources()
    status = 0
    for signatures in (True, False):
        selected = [p for p in paths
                    if (p in foreign or Path(p).parts[:2] == ('core', 'core')) == signatures]
        if selected:
            flags = ['--signatures'] if signatures else []
            status = subprocess.run([str(executable), '--check', *flags, *selected], cwd=ROOT).returncode or status
    return status


def main():
    if sys.argv[1:] == ['--darwin-import-law']:
        return check_darwin_import_law()
    if not check_file_lengths(ROOT) or not check_boundaries(ROOT) or not check_target(ROOT):
        return 1
    if apple_check.rust_changed() and apple_check.check():
        return 1
    if status := check_darwin_import_law():
        return status
    rustfmt_flags = sys.argv[1:]
    query = 'kind("^rust_(library|binary)$", root//:)'
    attributes = json.loads(subprocess.run(
        [str(BUCK2), 'uquery', query,
         '--output-attribute', 'crate_root', '--output-attribute', 'edition'],
        cwd=ROOT, check=True, capture_output=True, text=True).stdout)
    if not attributes:
        raise SystemExit('fmt: empty target query; refusing an unchecked aggregate')
    roots = {}
    for target in attributes.values():
        roots.setdefault(target['edition'], set()).add(target['crate_root'])
        # rustfmt does not follow include!. Check each declared module as a
        # root, with its harness's edition. Declaration order is immaterial.
        registry = Path(target['crate_root']).with_name('modules.rs')
        if (ROOT / registry).is_file() and registry.parts[:1] == ('crates',):
            for line in (ROOT / registry).read_text().splitlines():
                if match := re.fullmatch(r'mod (\w+);', line):
                    source = registry.with_name(match[1] + '.rs')
                    if not (ROOT / source).is_file():
                        source = registry.parent / match[1] / 'mod.rs'
                    roots[target['edition']].add(str(source))
    status = 0
    for edition, files in sorted(roots.items()):
        command = [str(RUSTFMT), '--edition', edition, *rustfmt_flags, *sorted(files)]
        status = subprocess.run(command, cwd=ROOT).returncode or status
    if status:
        return status
    corpus = subprocess.run(
        [sys.executable, str(ROOT / 'editors/tree-sitter-hirsel/check-local')], cwd=ROOT)
    if corpus.returncode:
        return corpus.returncode
    synced = subprocess.run([sys.executable, str(ROOT / 'tools/buck2/sync.py'), '--if-stale'], cwd=ROOT)
    if synced.returncode:
        return synced.returncode
    try:
        docs = subprocess.run([sys.executable, str(ROOT / 'tools/check_doc_refs.py')], cwd=ROOT)
        return docs.returncode or not check_runners(ROOT) or check_compiler_metadata() or check_cases(changed_cases())
    except subprocess.CalledProcessError as error:
        return error.returncode


if __name__ == '__main__':
    sys.exit(main())
