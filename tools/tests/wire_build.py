#!/usr/bin/env python3
"""Build the wire protocol fixtures from the current checkout's core and std.

The fixtures are the workspace member core/programs/wire; `hirsel build` resolves
its manifest, so this script names only entries, limits and grants.
"""
import argparse
import json
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[2]

# Each mode: its executables (output stem, entry), logical root limit and Net grant.
MODES = {
    'default': ([('main', 'echo/main'), ('client_probe', 'echo/client_probe'),
                 ('idle_probe', 'echo/idle_probe'), ('finish', 'finish/probe')],
                268435456, 'Net=Listen 127.0.0.0/8 18089-18089;Connect 127.0.0.0/8 18090-18091'),
    'session': ([('session', 'session/main'), ('session-idle', 'session/idle')],
                67108864, 'Net=Listen 127.0.0.0/8 18094-18094'),
    'admission': ([('admission', 'admission/main')], 1610612736, 'Net=Listen 127.0.0.0/8 18094-18094'),
    'migration': ([('migration', 'migration/main')], 1610612736, 'Net=Listen 127.0.0.0/8 0-65535'),
}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--compiler', type=Path, required=True)
    parser.add_argument('--runtime', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--backend', choices=['cranelift', 'llvm'], required=True)
    parser.add_argument('--cc', type=Path)
    parser.add_argument('--session-only', action='store_true')
    parser.add_argument('--admission-only', action='store_true')
    parser.add_argument('--migration-only', action='store_true')
    parser.add_argument('--libraries', type=Path, default=ROOT / 'core',
                        help='library root whose programs/wire is built (default: the checkout)')
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    mode = ('migration' if args.migration_only else 'admission' if args.admission_only
            else 'session' if args.session_only else 'default')
    entries, limit, net = MODES[mode]
    if mode == 'migration':
        (output / 'migration-store').mkdir(exist_ok=True)
    libraries = args.libraries.absolute()
    for name, entry in entries:
        stem = f'{name}-{args.backend}'
        command = [str(args.compiler.resolve()), 'build', str(libraries / 'programs/wire'),
                   '--libraries', str(libraries), '--source-core', '--store', str(output / 'store'),
                   '--entry', 'wire@1/' + entry, '--backend', args.backend,
                   '--runtime', str(args.runtime.resolve()), '--limit', str(limit),
                   '--grant', 'Clock', '--grant', net, '--json', '-o', str(output / stem)]
        if name not in ('client_probe', 'finish'):
            command.extend(['--grant', 'Tasks'])
        if name in ('client_probe', 'admission'):
            command.extend(['--grant', 'Random'])
        if mode == 'migration':
            command.extend(['--grant', 'Random', '--grant', 'Process', '--grant', 'Stderr',
                            '--dir', str(output / 'migration-store')])
        if args.backend == 'llvm':
            command.append('--release')
        if args.cc:
            command += ['--cc', str(args.cc.resolve())]
        result = subprocess.run(command, cwd=ROOT, capture_output=True, text=True)
        (output / (stem + '.json')).write_text(result.stdout)
        (output / (stem + '.stderr')).write_text(result.stderr)
        # A request-record hit reports linked: false; its executable is still at -o.
        try:
            report = json.loads(result.stdout)
            built = (report.get('accepted') is True and report.get('stopped') is None
                     and report['native']['executable'] == str(output / stem))
        except (ValueError, AttributeError, KeyError, TypeError):
            built = False
        if result.returncode or not built or not (output / stem).is_file():
            print(result.stdout + result.stderr)
            return 1
        print(output / stem, flush=True)
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
