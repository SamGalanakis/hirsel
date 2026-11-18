"""How a rebase merges the files many lanes edit at once instead of stopping:
two changes that each add to one of them conflict without disagreeing.

Git runs this as the `hirsel` merge driver for the paths .gitattributes names
(bootstrap.py installs the driver in the clone's config).

- third-party/NOTICES.md is append-only: hunks where both sides only add
  lines keep main's lines, then the change's;
- module declarations use a three-way set merge:
  either side's removal wins and both sides' additions survive;

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
DRIVER = 'hirsel'


class Conflict(Exception):
    """Both sides changed the same part differently."""


def line_set(path: str) -> bool:
    return any(fnmatchcase(path, pattern) for pattern in LINE_SETS)


def resolvable(path: str) -> bool:
    return line_set(path) or path in APPEND_ONLY


def resolve(path: str, base: str | None, ours: str | None, theirs: str | None) -> str | None:
    """The merged text of a path, from its merge base and the two sides
    (`ours` is main's side in a rebase, `theirs` the change's), or None when it
    stays a conflict. A missing side (a delete) is never resolved."""
    if ours is None or theirs is None or not resolvable(path):
        return None
    try:
        if line_set(path):
            return merge_line_set(base or '', ours, theirs)
        return line_merge(base or '', ours, theirs, appended)
    except Conflict:
        return None


def merge_line_set(base: str, ours: str, theirs: str) -> str:
    """(base & ours & theirs) | (ours - base) | (theirs - base).

    Merge the whole file so Git's hunk
    boundaries must not decide whether a deleted row returns. Keep ours' order,
    then theirs' new lines; these lists have no ordering semantics.
    """
    ancestor, left, right = (text.splitlines() for text in (base, ours, theirs))
    removed = (set(ancestor) - set(left)) | (set(ancestor) - set(right))
    lines = dict.fromkeys(line for line in left + right if line not in removed)
    return ''.join(line + '\n' for line in lines)


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
