#!/usr/bin/env python3
"""The Darwin durability ban (STORE-SPEC §3.3c, macOS decisions D3 and D5).

No member of a Darwin runtime archive references a name in link_kit.py's
DARWIN_BANNED (`fsync`, `fdatasync`). On Darwin those return before the drive
has flushed its cache; every Store durability point is `fcntl(F_FULLFSYNC)`.
The kit's libSystem.tbd leaves them out, so a link that reaches one is refused;
this law reads every member, reached or not, so a durability path that forgets
F_FULLFSYNC fails on Linux before any link.

    darwin_sync_ban.py LINK_KIT LLVM ARCHIVE...

LINK_KIT is link_kit.py, whose DARWIN_BANNED this law enforces. The pinned
LLVM's llvm-nm reads the archive. Any diagnostic from it other than
an empty member's fails the law: a member it cannot read is one it did not check.
"""

from pathlib import Path
import importlib.util
import subprocess
import sys


def banned_names(kit):
    spec = importlib.util.spec_from_file_location('link_kit', kit)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module.DARWIN_BANNED


def banned_references(nm, archive, banned):
    run = subprocess.run(
        [str(nm), '--undefined-only', '--print-file-name', '--no-sort', str(archive)],
        capture_output=True, text=True,
        env={'LD_LIBRARY_PATH': str(nm.parent.parent / 'lib'), 'PATH': '/usr/bin:/bin'})
    # llvm-nm notes a member with an empty symbol table; anything else is a
    # member it could not read.
    errors = [line for line in run.stderr.splitlines()
              if line.strip() and not line.endswith(': no symbols')]
    if run.returncode or errors:
        raise SystemExit(f'darwin sync ban: llvm-nm could not read {archive}:\n'
                         + '\n'.join(errors))
    found = []
    for line in run.stdout.splitlines():
        member, _, rest = line.rpartition(': ')
        name = rest.split()[-1] if rest.split() else ''
        if name in banned:
            found.append((member, name))
    return found


def main():
    if len(sys.argv) < 4:
        raise SystemExit('usage: darwin_sync_ban.py LINK_KIT LLVM ARCHIVE...')
    banned = banned_names(Path(sys.argv[1]).resolve())
    nm = Path(sys.argv[2]).resolve() / 'bin' / 'llvm-nm'
    failed = False
    for archive in (Path(arg).resolve() for arg in sys.argv[3:]):
        if not archive.is_file() or archive.stat().st_size == 0:
            raise SystemExit(f'darwin sync ban: no archive at {archive}')
        found = banned_references(nm, archive, banned)
        for member, name in found:
            print(f'{member}: references {name}; Darwin durability is {banned[name]}',
                  file=sys.stderr)
        failed |= bool(found)
        if not found:
            print(f'darwin sync ban: no member of {archive} references '
                  + ' or '.join(sorted(banned)))
    return 1 if failed else 0


if __name__ == '__main__':
    sys.exit(main())
