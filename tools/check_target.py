#!/usr/bin/env python3
"""Keep platform decisions target-driven and exhaustive.

Two rules over every first-party Rust source under crates/:

* host-cfg: `target_os` in a `cfg`, `cfg!` or `cfg_attr`, or any `cfg!(target_...)`,
  appears only in the host-seam directories (HOST_SEAMS) and the files in HOST_FILES.
  What Hirsel emits is a function of `--target`; only host mechanisms read the host.
* target-match: a decision on `hirsel_core_layout::Target` or a fact derived from it
  (`Arch`, `Os`, `ObjectFormat`, `CAbi`), or on a target triple string, is an
  exhaustive `match`. Comparing with `==`/`!=`, `matches!`, `if let`/`let else` on a
  variant, and a `match` with a `_` arm all let a new target fall through silently.
"""

import argparse
from bisect import bisect_right
from pathlib import Path
import re
import sys

from check_file_lengths import git_paths, snapshot


CRATES = 'crates/'
# Directories whose whole purpose is one host OS (one file or directory per host).
HOST_SEAMS = (
    'crates/hirsel-core-rt/src/host/',
    'crates/hirsel-core-driver/src/dry_run/host/',
    'crates/hirsel-core-net/src/host/',
    'crates/hirsel-core-net/src/reactor/',
    'crates/hirsel-core-net/src/own_address/',
    'crates/hirsel-prof/src/linux/',
    'crates/hirsel-llvm-lazy/src/host/',
)
# Host-OS code that predates its seam. Each entry names the slice of the macOS design
# (research-1124/macos-arm64, §9) that moves it behind a seam; remove an entry when it
# no longer reads the host. A stale entry fails the check.
HOST_FILES = {
    # S6/S7: the runtime host seam and its Darwin provider.
    'crates/hirsel-core-net/src/reactor.rs': 'S6',
    'crates/hirsel-core-rt/src/fiber.rs': 'S6',
    'crates/hirsel-core-rt/src/fiber_stop_tests.rs': 'S6',
    'crates/hirsel-core-rt/src/provider_tests.rs': 'S6',
    'crates/hirsel-core-rt/src/secret/key.rs': 'S6',
    'crates/hirsel-core-rt/src/task_heap.rs': 'S6',
    'crates/hirsel-core-rt/src/tasks.rs': 'S6',
    'crates/hirsel-core-rt/src/openat2_required.rs': 'S7',
    'crates/hirsel-core-rt/src/test_process.rs': 'S7',
    'crates/hirsel-core-rt/src/tests.rs': 'S6',
    # S12: the driver on a Mac host (sealed files, launcher, compiler identity).
    'crates/hirsel-core-driver/src/foreign_archive.rs': 'S12',
    'crates/hirsel-core-driver/src/link_input.rs': 'S12',
    'crates/hirsel-core-driver/src/package_files.rs': 'S12',
    'crates/hirsel-core-driver/src/package_workspace.rs': 'S12',
    'crates/hirsel-core-driver/src/reuse.rs': 'S12',
    'crates/hirsel-core-driver/src/startup_native.rs': 'S12',
    'crates/hirsel-core-ids/src/launch.rs': 'S12',
    'crates/hirsel-intern/src/instructions.rs': 'S12',
    # S13: hirsel-prof on Darwin.
    'crates/hirsel-prof/src/lib.rs': 'S13',
}

TARGET_VARIANT = (r'(?:\bTarget::(?:X86_64Linux|Aarch64Linux|Aarch64Darwin)'
                  r'|\bArch::(?:X86_64|Aarch64)\b|\bOs::(?:Linux|Macos)\b'
                  r'|\bObjectFormat::(?:Elf|MachO)\b|\bCAbi::(?:SysV64|Aapcs64|DarwinPcs)\b'
                  r'|"(?:x86_64|aarch64)-[a-z0-9_]+-[a-z0-9_]+(?:-[a-z0-9_]+)?")')
VARIANT = re.compile(TARGET_VARIANT)
COMPARE = re.compile(rf'[!=]=\s*(?:Some\s*\(\s*)?(?:[\w:]*::)?{TARGET_VARIANT}'
                     rf'|{TARGET_VARIANT}\s*\)?\s*[!=]=')
MATCHES = re.compile(r'\bmatches!\s*\(')
LET_PATTERN = re.compile(rf'\blet\b[^=;{{}}]*?{TARGET_VARIANT}[^=;{{}}]*?=[^=>]')

# A comment, or a string or char literal. Triple literals are kept because a triple is a
# target; every other literal is blanked so its braces and commas are inert.
LEXEME = re.compile(r'(?P<comment>//[^\n]*|/\*.*?\*/)'
                    r'|(?P<raw>\b(?:br|r)(?P<hashes>\#*)".*?"(?P=hashes))'
                    r'|(?P<string>b?"(?:\\.|[^"\\])*")'
                    r"|(?P<char>b?'(?:\\.|[^'\\\n])')", re.DOTALL)


def blank(text):
    return re.sub(r'[^\n]', ' ', text)


def scrub(source):
    """Comments and non-triple literals become spaces; newlines stay for line numbers."""
    def replace(match):
        if match['string'] and VARIANT.fullmatch(match['string']):
            return match['string']
        if match['comment']:
            return blank(match[0])
        return '"' + blank(match[0][2:]) + '"' if len(match[0]) >= 2 else blank(match[0])
    return LEXEME.sub(replace, source)


def closing(text, start):
    """Index just past the bracket that closes the one at `start`."""
    pairs = {'(': ')', '[': ']', '{': '}'}
    stack = []
    for at in range(start, len(text)):
        char = text[at]
        if char in pairs:
            stack.append(pairs[char])
        elif stack and char == stack[-1]:
            stack.pop()
            if not stack:
                return at + 1
    return len(text)


def arms(body):
    """The pattern text of each top-level arm of a `match` body (braces excluded)."""
    patterns = []
    start = at = 0
    while at < len(body):
        char = body[at]
        if char in '([{':
            at = closing(body, at)
            continue
        if body.startswith('=>', at):
            patterns.append(body[start:at])
            at += 2
            while at < len(body) and body[at].isspace():
                at += 1
            if at < len(body) and body[at] == '{':
                at = closing(body, at)
                while at < len(body) and body[at].isspace():
                    at += 1
                if at < len(body) and body[at] == ',':
                    at += 1
            else:
                while at < len(body) and body[at] != ',':
                    at = closing(body, at) if body[at] in '([{' else at + 1
                at += 1
            start = at
            continue
        at += 1
    return patterns


MATCH = re.compile(r'\bmatch\b')
WILDCARD = re.compile(r'^\s*(?:\|\s*)?_\s*(?:if\b.*)?$', re.DOTALL)


def wildcard_matches(source):
    for found in MATCH.finditer(source):
        brace = source.find('{', found.end())
        if brace < 0:
            continue
        # The scrutinee ends at the first `{` outside brackets.
        at = found.end()
        while at < brace:
            if source[at] in '([':
                at = closing(source, at)
                brace = source.find('{', at)
                if brace < 0:
                    break
                continue
            at += 1
        if brace < 0:
            continue
        patterns = arms(source[brace + 1:closing(source, brace) - 1])
        if any(VARIANT.search(p) for p in patterns) and any(WILDCARD.match(p) for p in patterns):
            yield found.start()


CFG = re.compile(r'\b(?P<macro>cfg!|cfg_attr|cfg)\s*\(')


def host_cfgs(source):
    for found in CFG.finditer(source):
        inside = source[found.end() - 1:closing(source, found.end() - 1)]
        if found['macro'] == 'cfg_attr':
            # Only the predicate (before the first top-level comma) reads the target.
            depth = 0
            for at, char in enumerate(inside):
                depth += char in '([{'
                depth -= char in ')]}'
                if char == ',' and depth == 1:
                    inside = inside[:at]
                    break
        if re.search(r'\btarget_os\b', inside) or (
                found['macro'] == 'cfg!' and re.search(r'\btarget_\w+', inside)):
            yield found.start()


def source_violations(path, source):
    source = scrub(source)
    newlines = [match.start() for match in re.finditer('\n', source)]

    def line(offset):
        return bisect_right(newlines, offset) + 1

    if not path.startswith(HOST_SEAMS) and path not in HOST_FILES:
        for offset in host_cfgs(source):
            yield line(offset), 'host-cfg'
    for pattern in (COMPARE, LET_PATTERN):
        for found in pattern.finditer(source):
            yield line(found.start()), 'target-match'
    for found in MATCHES.finditer(source):
        if VARIANT.search(source, found.end(), closing(source, found.end() - 1)):
            yield line(found.start()), 'target-match'
    for offset in wildcard_matches(source):
        yield line(offset), 'target-match'


def sources(root, staged):
    if staged:
        paths = [p for p in git_paths(root, 'ls-files', '-z', '--', CRATES) if p.endswith('.rs')]
        return snapshot(root, ':0', paths)
    paths = [p for p in git_paths(root, 'ls-files', '-z', '--cached', '--others',
                                  '--exclude-standard', '--', CRATES) if p.endswith('.rs')]
    return snapshot(root, None, paths)


MESSAGES = {
    'host-cfg': 'host cfg outside a host seam; decide on the target (hirsel_core_layout::Target) '
                'or move the code behind a host seam',
    'target-match': 'non-exhaustive decision on a target; use an exhaustive match on the '
                    'Target or one of its facts, with no `_` arm',
}


def check(root, staged=False):
    ok = True
    try:
        files = sources(root, staged)
    except OSError as error:
        print(f'check_target: {error}', file=sys.stderr)
        return False
    host = set()
    for path, data in sorted(files.items()):
        text = data.decode('utf-8', 'surrogateescape')
        if path in HOST_FILES and any(True for _ in host_cfgs(scrub(text))):
            host.add(path)
        for line, rule in sorted(set(source_violations(path, text))):
            print(f'{path}:{line}: {rule}: {MESSAGES[rule]}', file=sys.stderr)
            ok = False
    for path in sorted(set(HOST_FILES) - host):
        print(f'{path}: no longer reads the host. Remove it from HOST_FILES in '
              'tools/check_target.py.', file=sys.stderr)
        ok = False
    return ok


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--staged', action='store_true', help='check the Git index')
    sys.exit(0 if check(Path(__file__).resolve().parents[1], parser.parse_args().staged) else 1)
