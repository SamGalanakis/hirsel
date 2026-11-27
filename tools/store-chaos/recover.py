"""Verify the preserved offline image before permitting a read-write Store open."""
import base64
import gzip
from pathlib import Path
import subprocess


def verify_copy(files, target, hirsel, release=None):
    target.mkdir()
    for name, packed in files.items():
        if name in ('', '.', '..') or Path(name).name != name:
            raise ValueError('invalid Store filename')
        (target / name).write_bytes(gzip.decompress(base64.b64decode(packed, validate=True)))
    cmd = [hirsel, 'store', 'verify', target]
    if release:
        cmd.extend(['--release', release])
    result = subprocess.run(cmd, capture_output=True, text=True)
    (target.parent / (target.name + '.verify')).write_text(result.stdout + result.stderr)
    if result.returncode:
        raise ValueError(f'offline verification failed ({result.returncode}) at {target}')
