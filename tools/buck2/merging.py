"""How a rebase merges the files many lanes edit at once instead of stopping:
two changes that each add to one of them conflict without disagreeing.

Git runs this as the `hirsel` merge driver for the paths .gitattributes names
(bootstrap.py installs the driver in the clone's config), so a lane's own
rebase and tools/land's batch rebase merge them alike; tools/land also
resolves a conflict in one with `resolve` where the driver is not installed.

- third-party/NOTICES.md is append-only: hunks where both sides only add
  lines keep main's lines, then the change's;
- module declarations use a three-way set merge:
  either side's removal wins and both sides' additions survive;
- known-failures rows merge by identity, keeping the newer row.

Anything else, or a hunk that both sides edit differently, stays a conflict:
the driver then leaves git's usual conflict markers.
"""

from __future__ import annotations

from fnmatch import fnmatchcase
import os
import subprocess
import sys
import tempfile

APPEND_ONLY = ('third-party/NOTICES.md',)
# Whole-line identities, without key or ordering semantics. Keep this in sync
# with the independent-line patterns in .gitattributes.
LINE_SETS = ('crates/*/tests/*/modules.rs',)
LEDGER = 'tools/known-failures.tsv'
DRIVER = 'hirsel'


class Conflict(Exception):
    """Both sides changed the same part differently."""


def line_set(path: str) -> bool:
    return any(fnmatchcase(path, pattern) for pattern in LINE_SETS)


def resolvable(path: str) -> bool:
    return line_set(path) or path == LEDGER or path in APPEND_ONLY


def resolve(path: str, base: str | None, ours: str | None, theirs: str | None) -> str | None:
    """The merged text of a path, from its merge base and the two sides
    (`ours` is main's side in a rebase, `theirs` the change's), or None when it
    stays a conflict. A missing side (a delete) is never resolved."""
    if ours is None or theirs is None or not resolvable(path):
        return None
    try:
        if path == LEDGER:
            return merge_ledger(base or '', ours, theirs)
        if line_set(path):
            return merge_line_set(base or '', ours, theirs)
        return line_merge(base or '', ours, theirs, appended)
    except Conflict:
        return None


def merge_line_set(base: str, ours: str, theirs: str) -> str:
    """(base & ours & theirs) | (ours - base) | (theirs - base).

    Use the removal rule from sorted_stretch over the whole file: Git's hunk
    boundaries must not decide whether a deleted row returns. Keep ours' order,
    then theirs' new lines; these lists have no ordering semantics.
    """
    ancestor, left, right = (text.splitlines() for text in (base, ours, theirs))
    removed = (set(ancestor) - set(left)) | (set(ancestor) - set(right))
    lines = dict.fromkeys(line for line in left + right if line not in removed)
    return ''.join(line + '\n' for line in lines)


def merge_ledger(base: str, ours: str, theirs: str) -> str:
    """Merge known-failure rows by their exact first tab-separated field."""
    def rows(text):
        entries = {}
        for line in text.splitlines():
            if not line.strip() or line.startswith('#'):
                continue
            identity = line.split('\t', 1)[0]
            if identity in entries:
                raise Conflict('duplicate ledger identity')
            entries[identity] = line
        return entries

    def row_date(row):
        fields = row.split('\t')
        return fields[3] if len(fields) > 3 else ''

    ancestor, left, right = (rows(text) for text in (base, ours, theirs))
    comments = [line for line in ours.splitlines() if line.startswith('#')]
    merged = []
    for identity in sorted(ancestor.keys() | left.keys() | right.keys()):
        old, a, b = (entries.get(identity) for entries in (ancestor, left, right))
        if a == b:
            value = a
        elif a == old:
            value = b
        elif b == old:
            value = a
        elif a is None:
            value = b
        elif b is None:
            value = a
        else:
            # Two different rows for one identity: the newer date wins, and a
            # tie takes theirs, the change's side in a rebase — the later of
            # the two.
            value = b if row_date(b) >= row_date(a) else a
        if value is not None:
            merged.append(value)
    return ''.join(line + '\n' for line in comments + merged)


def line_merge(base: str, ours: str, theirs: str, stretch) -> str:
    """A three-way line merge whose conflicting hunks `stretch` resolves from
    main's lines, the base's and the change's, and the lines around the hunk."""
    with tempfile.TemporaryDirectory() as scratch:
        names = []
        for name, text in (('ours', ours), ('base', base), ('theirs', theirs)):
            path = os.path.join(scratch, name)
            with open(path, 'w', encoding='utf-8', errors='surrogateescape') as file:
                file.write(text)
            names.append(path)
        merge = subprocess.run(['git', 'merge-file', '-p', '--diff3', '-L', 'ours', '-L', 'base', '-L', 'theirs',
                                *names], capture_output=True, encoding='utf-8', errors='surrogateescape')
    if merge.returncode < 0 or merge.returncode > 127:
        raise Conflict(merge.stderr)
    # Clean runs of lines (lists) and conflicting hunks (dicts), in order.
    parts, part, sides = [], None, {'ours': [], 'base': [], 'theirs': []}
    for line in merge.stdout.splitlines(keepends=True):
        if line.startswith('<<<<<<< ours'):
            part = 'ours'
        elif part == 'ours' and line.startswith('||||||| base'):
            part = 'base'
        elif part == 'base' and line.startswith('======='):
            part = 'theirs'
        elif part == 'theirs' and line.startswith('>>>>>>> theirs'):
            parts.append(sides)
            part, sides = None, {'ours': [], 'base': [], 'theirs': []}
        elif part is None:
            if not parts or isinstance(parts[-1], dict):
                parts.append([])
            parts[-1].append(line)
        else:
            sides[part].append(line)
    if part is not None:
        raise Conflict('unterminated conflict')
    out = []
    for index, chunk in enumerate(parts):
        if isinstance(chunk, list):
            out += chunk
            continue
        following = parts[index + 1] if index + 1 < len(parts) else None
        after = following[0] if isinstance(following, list) else None
        out += stretch(chunk['ours'], chunk['base'], chunk['theirs'], out[-1] if out else None, after)
    return ''.join(out)


def appended(ours, base, theirs, before, after):
    if base:
        raise Conflict('both sides edit the same lines')
    return ours + theirs


def install(root) -> None:
    """Make this file the clone's `hirsel` merge driver (.gitattributes)."""
    for name, value in (('name', 'files many lanes add to (tools/buck2/merging.py)'),
                        ('driver', 'python3 tools/buck2/merging.py %O %A %B %P')):
        subprocess.run(['git', 'config', f'merge.{DRIVER}.{name}', value], cwd=root, check=True)


def main(base_file: str, ours_file: str, theirs_file: str, path: str) -> int:
    """Git's merge driver: the merge into `ours_file`, or git's conflict
    markers there and exit status 1 when it stays a conflict."""
    def read(name):
        with open(name, encoding='utf-8', errors='surrogateescape') as file:
            return file.read()
    text = resolve(path, read(base_file), read(ours_file), read(theirs_file))
    if text is None:
        return 1 if subprocess.run(['git', 'merge-file', '-L', 'ours', '-L', 'base', '-L', 'theirs',
                                    ours_file, base_file, theirs_file]).returncode else 0
    with open(ours_file, 'w', encoding='utf-8', errors='surrogateescape') as file:
        file.write(text)
    return 0


if __name__ == '__main__':
    sys.exit(main(*sys.argv[1:]))
