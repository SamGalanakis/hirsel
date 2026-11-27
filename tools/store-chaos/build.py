"""Build the native chaos fixtures from this checkout, including the real Store under test."""
import argparse
import json
from pathlib import Path
import subprocess
import sys
import tempfile

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
sys.path.insert(0, str(ROOT / 'core/tools'))
from store_tests import inputs

BUCKET_NET = (
    'Net=Resolve localhost;Resolve fly.storage.tigris.dev;'
    'Connect 127.0.0.1/32 1-65535;Connect ::1/128 1-65535;'
    'Connect 0.0.0.0/0 443-443;Connect ::/0 443-443;'
    'Tls localhost 1-65535 System;Tls fly.storage.tigris.dev 443-443 System'
)


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('hirsel', type=Path)
    p.add_argument('output', type=Path)
    p.add_argument('--target')
    p.add_argument('--runtime', type=Path)
    p.add_argument('--cc')
    mode = p.add_mutually_exclusive_group()
    mode.add_argument('--retention', action='store_true')
    mode.add_argument('--bucket', action='store_true')
    args = p.parse_args()
    corpus = inputs(ROOT, tests=args.retention)
    if args.retention:
        source, entry = 'retention', 'chaos_retention'
        grants = ('Clock', 'Random', 'Tasks', 'Stdout')
        dir_grant = True
    elif args.bucket:
        source, entry = 'bucket_fixture', 'chaos_bucket_main'
        grants = ('Clock', 'Random', 'Tasks', 'Stdout', 'Stdin',
                  'Secret:root=env:AWS_SECRET_ACCESS_KEY kdf', BUCKET_NET)
        dir_grant = False
    else:
        source, entry = 'fixture', 'chaos_main'
        grants = ('Clock', 'Random', 'Tasks', 'Stdout', 'Stdin')
        dir_grant = True
    corpus['packages'][-1]['sources'].append(dict(file=str(HERE / (source + '.hirsel')),
                                                  modules=['store'], signatures=False))
    with tempfile.TemporaryDirectory() as tmp:
        manifest = Path(tmp) / 'inputs.json'
        manifest.write_text(json.dumps(corpus))
        cmd = [args.hirsel, 'build', '--inputs', manifest, '--entry', 'store@1/store/' + entry,
               '--source-core', '--source-std', '--limit', '1073741824', '-o', args.output]
        if dir_grant:
            cmd.extend(['--dir', '.'])
        for grant in grants:
            cmd.extend(['--grant', grant])
        if args.runtime:
            cmd.extend(['--runtime', args.runtime])
        if args.cc:
            cmd.extend(['--cc', args.cc])
        if args.target:
            cmd.extend(['--target', args.target])
        subprocess.run(cmd, cwd=ROOT, check=True)


if __name__ == '__main__':
    main()
