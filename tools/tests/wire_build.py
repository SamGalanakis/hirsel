#!/usr/bin/env python3
"""Build the wire protocol fixtures from the current checkout's core and std."""
import argparse
import json
from pathlib import Path
import subprocess
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'core/tools'))
from std_native import library_sources


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
    parser.add_argument('--runner', type=Path)
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    core = {'name':'core', 'version':'1'}
    std = {'name':'std', 'version':'1'}
    packages = []
    for name, dependencies in [('core', []), ('std', [core])]:
        packages.append({
            'package': {'name':name, 'version':'1'}, 'dependencies':dependencies,
            'sources': library_sources(ROOT/'core'/name, name == 'core'),
        })
    uses_runner = args.session_only or args.admission_only or args.migration_only
    if uses_runner:
        for directory in (ROOT / 'core/programs/store', ROOT / 'core/programs/runner'):
            with (directory / 'hirsel.toml').open('rb') as file:
                manifest = tomllib.load(file)
            package = manifest['package']
            sources = [{'file': str(directory / file), 'modules': module.split('.'), 'signatures': False}
                       for file, module in manifest['sources'].items()]
            if args.runner and package['name'] == 'runner':
                next(source for source in sources if source['modules'] == ['runner'])['file'] = str(args.runner.resolve())
            packages.append({
                'package': {'name': package['name'], 'version': str(package['major'])},
                'dependencies': [core] + [{'name': name, 'version': str(dep['major'])}
                                          for name, dep in manifest['dependencies'].items()],
                'sources': sources,
            })
    modules = ['migration'] if args.migration_only else ['admission'] if args.admission_only else ['session'] if args.session_only else ['echo', 'finish']
    packages.append({
        'package': {'name':'wire', 'version':'1'}, 'dependencies':[core, std] + ([{'name':'store', 'version':'1'}, {'name':'runner', 'version':'1'}] if uses_runner else []),
        'sources':[{'file':str(ROOT/'core/programs/wire'/f'{name}.hirsel'),
                    'modules':[name], 'signatures':False} for name in modules],
    })
    inputs = output/'inputs.json'
    inputs.write_text(json.dumps({'core':core, 'packages':packages})+'\n')
    entries = [('session','session/main'), ('session-idle','session/idle')] if args.session_only else [('main','echo/main'), ('client_probe','echo/client_probe'), ('idle_probe','echo/idle_probe'), ('finish','finish/probe')]
    if args.admission_only:
        entries = [('admission', 'admission/main')]
    if args.migration_only:
        entries = [('migration', 'migration/main')]
        (output / 'migration-store').mkdir(exist_ok=True)
    for name, entry in entries:
        stem = f'{name}-{args.backend}'
        command = [str(args.compiler.resolve()), 'build', '--source-core',
                   '--inputs', str(inputs), '--store', str(output/'store'),
                   '--entry', 'wire@1/'+entry, '--backend', args.backend,
                   '--runtime', str(args.runtime.resolve()), '--limit', '1610612736' if args.admission_only or args.migration_only else '67108864' if args.session_only else '268435456',
                   '--grant', 'Clock',
                   '--grant', 'Net=Listen 127.0.0.0/8 0-65535' if args.migration_only else 'Net=Listen 127.0.0.0/8 18094-18094' if uses_runner else 'Net=Listen 127.0.0.0/8 18089-18089;Connect 127.0.0.0/8 18090-18091',
                   '--json', '-o', str(output/stem)]
        if name not in ('client_probe', 'finish'):
            command.extend(['--grant', 'Tasks'])
        if name in ('client_probe', 'admission'):
            command.extend(['--grant', 'Random'])
        if args.migration_only:
            command.extend(['--grant', 'Random', '--grant', 'Process', '--grant', 'Stderr',
                            '--dir', str(output / 'migration-store')])
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
