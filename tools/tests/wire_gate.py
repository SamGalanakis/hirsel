#!/usr/bin/env python3
"""Build the wire fixtures (echo, finish, session, migration) and run their loopback
checks: echo, client, idle, session Close, half-close and the migration dry-run
conflict. The first failing step ends the gate."""
import argparse
import os
from pathlib import Path
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[2]
TESTS = ROOT / 'tools/tests'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--compiler', type=Path, required=True)
    parser.add_argument('--runtime', type=Path, default=os.environ.get('HIRSEL_CORE_RT_ARCHIVE'))
    parser.add_argument('--backend', choices=['cranelift', 'llvm'], default='cranelift')
    args = parser.parse_args()
    if args.runtime is None:
        parser.error('pass --runtime or set HIRSEL_CORE_RT_ARCHIVE')
    compiler, runtime = args.compiler.resolve(), args.runtime.resolve()
    with tempfile.TemporaryDirectory(prefix='wire-gate-') as tmp:
        out = Path(tmp)

        def fixture(name):
            return str(out / f'{name}-{args.backend}')

        build = [sys.executable, str(TESTS / 'wire_build.py'), '--compiler', str(compiler),
                 '--runtime', str(runtime), '--backend', args.backend, '--output', str(out)]
        steps = [
            ('build echo/finish', build),
            ('build session', build + ['--session-only']),
            ('build migration', build + ['--migration-only']),
            ('wsock echo', [sys.executable, str(TESTS / 'wire_wsock.py'), '--echo', fixture('main'),
                            '--client', fixture('client_probe'), '--idle-echo', fixture('idle_probe')]),
            ('wsock session', [sys.executable, str(TESTS / 'wire_wsock.py'), '--session', fixture('session'),
                               '--idle-session', fixture('session-idle')]),
            ('finish', [sys.executable, str(TESTS / 'wire_finish.py'), '--probe', fixture('finish')]),
            ('migration dry-run', [sys.executable, str(TESTS / 'wire_admission.py'), '--case', 'cli_dry_run_conflict',
                                   '--server', fixture('migration'), '--compiler', str(compiler),
                                   '--runtime', str(runtime)]),
        ]
        for name, command in steps:
            print(f'WIRE GATE {name}', flush=True)
            if subprocess.run(command, cwd=ROOT).returncode:
                print(f'WIRE GATE {name}: FAILED', flush=True)
                return 1
    print('WIRE GATE: PASS', flush=True)
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
