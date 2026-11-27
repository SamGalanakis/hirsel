"""Guest operations on one disposable APFS sparse image."""
import base64
import errno
import gzip
import json
import os
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parent
VOL = ROOT / 'volume'
STORE = VOL / 's'
IMG = ROOT / 'scratch.sparseimage'


def run(*cmd):
    subprocess.run(cmd, check=True, stdout=subprocess.DEVNULL)


def fixture(mode, data=b'', when=''):
    proc = subprocess.run([ROOT / 'fixture', mode], input=data, cwd=STORE, capture_output=True,
                          env=os.environ | dict(DYLD_INSERT_LIBRARIES=str(ROOT / 'witness.dylib'),
                                                CHAOS_RENAME=when))
    err = proc.stderr.decode()
    return dict(rc=proc.returncode, output=proc.stdout.decode(),
                native=[s for s in err.splitlines() if s.startswith(('FULLFSYNC ', 'CUT '))],
                diagnostic=err[-600:] if proc.returncode not in (0, 86) else '')


def main(action, *args):
    if action == 'setup':
        VOL.mkdir(exist_ok=True)
        run('cc', '-arch', 'arm64', '-dynamiclib', ROOT / 'mac_interpose.c', '-o', ROOT / 'witness.dylib')
        run('hdiutil', 'create', '-size', '96m', '-type', 'SPARSE', '-fs', 'APFS',
            '-volname', 'HirselChaos', IMG)
        attach()
        STORE.mkdir()
        return fixture('init')
    if action == 'tx':
        return fixture('serve', bytes([int(args[0])]))
    if action == 'cut':
        return fixture('serve', bytes([int(args[1]), 0]), args[0])
    if action == 'pressure':
        size = 0
        try:
            with (VOL / 'fill').open('xb', buffering=0) as f:
                block = bytes(1024 * 1024)
                while True:
                    size += f.write(block)
        except OSError as e:
            if e.errno != errno.ENOSPC:
                raise
            return dict(errno=e.errno, bytes=size)
        finally:
            (VOL / 'fill').unlink(missing_ok=True)
    if action == 'cold':
        run('hdiutil', 'detach', VOL)
        attach()
        return dict(files={p.name: base64.b64encode(gzip.compress(p.read_bytes())).decode()
                           for p in STORE.iterdir() if p.is_file()})
    if action == 'dump':
        return fixture('dump')
    if action == 'cleanup':
        run('hdiutil', 'detach', VOL)
        IMG.unlink()
        return {}
    raise ValueError(action)


def attach():
    run('hdiutil', 'attach', '-nobrowse', '-mountpoint', VOL, IMG)


if __name__ == '__main__':
    print(json.dumps(main(*sys.argv[1:])))
