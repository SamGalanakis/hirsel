#!/usr/bin/env python3
"""Own launcher scratch, including shared compile-time CARGO_TARGET_TMPDIR.

HIRSEL_KEEP_TEST_SCRATCH retains both test scratch and private native inputs.
Directory flock serializes registration/removal without leaving lock files.
"""
import fcntl
import os
from pathlib import Path
import shutil
import sys
import tempfile


def start_time(pid):
    return Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()[19]


def live(owner):
    try:
        pid, start = owner.split('-')
        return start_time(int(pid)) == start
    except FileNotFoundError:
        return False
    except (OSError, ValueError, IndexError):
        return True  # Unknown ownership is never authority to remove a directory.


def unused(path):
    if (path / '.hirsel-keep').exists():
        return False
    try:
        owners = list((path / '.hirsel-owners').iterdir())
        return not any(live(owner.name) for owner in owners)
    except OSError:
        # Creation can be interrupted before its first marker is installed.
        for prefix in ('hirsel-native-inputs-', 'hirsel-test-'):
            if path.name.startswith(prefix):
                fields = path.name[len(prefix):].split('-')
                if len(fields) == 3 and all(field.isdecimal() for field in fields[:2]):
                    return not live('-'.join(fields[:2]))
        return False


def sweep(parent, prefix):
    try:
        for path in parent.iterdir():
            try:
                if (path.name.startswith(prefix) and not path.is_symlink()
                        and path.is_dir() and path.stat().st_uid == os.geteuid() and unused(path)):
                    shutil.rmtree(path)
            except OSError:
                pass
    except OSError:
        pass


def main():
    operation, location, prefix, pid, keep = sys.argv[1:6]
    group = sys.argv[6] if len(sys.argv) > 6 else ''
    location = Path(location).absolute()
    parent = location if operation == 'create' else location.parent
    owner = f'{pid}-{start_time(int(pid))}'
    if operation == 'create':
        parent.mkdir(parents=True, exist_ok=True)
    # This directory itself is the lock, so no persistent lock inode is needed.
    lock = os.open(parent, os.O_RDONLY | os.O_DIRECTORY)
    try:
        fcntl.flock(lock, fcntl.LOCK_EX)
        if operation == 'create':
            sweep(parent, prefix)
            shared = parent / f'{prefix}{group}' if group else None
            created = shared is None or not shared.exists()
            path = (Path(tempfile.mkdtemp(prefix=f'{prefix}{owner}-', dir=parent))
                    if created else shared)
            try:
                (path / '.hirsel-owners').mkdir(mode=0o700, exist_ok=True)
                (path / '.hirsel-owners' / owner).touch()
                if keep:
                    (path / '.hirsel-keep').touch()
                if shared is not None and created:
                    path.rename(shared)
                    path = shared
            except BaseException:
                if created:
                    shutil.rmtree(path)
                raise
            print(path)
        else:
            (location / '.hirsel-owners' / owner).unlink(missing_ok=True)
            if unused(location):
                shutil.rmtree(location)
    finally:
        os.close(lock)


if __name__ == '__main__':
    main()
