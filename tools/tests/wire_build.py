#!/usr/bin/env python3
"""Build the wire protocol fixtures from the current checkout's core and std."""
import argparse
import json
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[2]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--compiler', type=Path, required=True)
    parser.add_argument('--runtime', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--backend', choices=['cranelift', 'llvm'], required=True)
    parser.add_argument('--cc', type=Path)
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    core = {'name':'core', 'version':'1'}
    std = {'name':'std', 'version':'1'}
    packages = []
    for name, dependencies in [('core', []), ('std', [core])]:
        packages.append({
            'package': {'name':name, 'version':'1'}, 'dependencies':dependencies,
            'sources':[{'file':str(path), 'modules':[path.stem], 'signatures':name == 'core'}
                       for path in sorted((ROOT/'core'/name).glob('*.hirsel'))],
        })
    packages.append({
        'package': {'name':'wire', 'version':'1'}, 'dependencies':[core, std],
        'sources':[{'file':str(ROOT/'core/programs/wire'/f'{name}.hirsel'),
                    'modules':[name], 'signatures':False} for name in ['echo','finish']],
    })
    inputs = output/'inputs.json'
    inputs.write_text(json.dumps({'core':core, 'packages':packages})+'\n')
    for name, entry in [('main','echo/main'), ('client_probe','echo/client_probe'), ('idle_probe','echo/idle_probe'), ('finish','finish/probe')]:
        stem = f'{name}-{args.backend}'
        command = [str(args.compiler.resolve()), 'build', '--source-core',
                   '--inputs', str(inputs), '--store', str(output/'store'),
                   '--entry', 'wire@1/'+entry, '--backend', args.backend,
                   '--runtime', str(args.runtime.resolve()), '--limit', '268435456',
                   '--grant', 'Clock', '--grant', 'Tasks', '--grant', 'Random',
                   '--grant', 'Net=Listen 127.0.0.0/8 18089-18089;Connect 127.0.0.0/8 18090-18091',
                   '--json', '-o', str(output/stem)]
        if args.backend == 'llvm':
            command.append('--release')
        if args.cc:
            command += ['--cc', str(args.cc.resolve())]
        result = subprocess.run(command, cwd=ROOT, capture_output=True, text=True)
        (output/(stem+'.json')).write_text(result.stdout)
        (output/(stem+'.stderr')).write_text(result.stderr)
        try:
            linked = json.loads(result.stdout).get('native', {}).get('linked')
        except (ValueError, AttributeError):
            linked = False
        if result.returncode or not linked:
            print(result.stdout + result.stderr)
            return 1
        print(output/stem, flush=True)
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
