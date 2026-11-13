#!/usr/bin/env python3
"""Build/run a LangArena port through harness.main, using the native product driver.

The benchmark runner belongs to slice F (core/langarena/run.py). This is the shared
correctness entry: unchanged upstream config, exact task name and checksum output.
"""
import argparse
import json
from pathlib import Path
import subprocess
import tempfile
from std_native import wait_load

ROOT = Path(__file__).resolve().parents[2]


def inputs(root, module, source=None):
    core = {'name': 'core', 'version': '1'}
    std = {'name': 'std', 'version': '1'}
    app = {'name': 'langarena', 'version': '1'}
    def files(directory):
        return [{'file': str(p.resolve()), 'modules': list(p.relative_to(directory).with_suffix('').parts),
                 'signatures': directory.name == 'core'}
                for p in sorted(directory.rglob('*.hirsel')) if 'tests' not in p.relative_to(directory).parts or directory.name == 'langarena']
    libraries = files(root / 'core/std')
    ports = files(root / 'core/langarena')
    if source:
        next(p for p in ports if p['modules'] == module.split('/'))['file'] = str(source)
    return {'core': core, 'packages': [
        {'package': core, 'sources': files(root / 'core/core'), 'dependencies': []},
        {'package': std, 'sources': libraries, 'dependencies': [core]},
        {'package': app, 'sources': ports, 'dependencies': [core, std]}]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--compiler', type=Path, required=True)
    parser.add_argument('--runtime', type=Path, required=True)
    parser.add_argument('--module', default='tests/bootstrap')
    parser.add_argument('--task', default='Base64::Encode')
    parser.add_argument('--config', type=Path, required=True)
    parser.add_argument('--backend', choices=['both', 'cranelift', 'llvm'], default='both')
    parser.add_argument('--report', type=Path)
    parser.add_argument('--helper-laws', action='store_true')
    parser.add_argument('--expect-mismatch', type=int, metavar='ACTUAL',
                        help='negative control: require checksum ACTUAL, status ERR and exit 1')
    args = parser.parse_args()
    if args.expect_mismatch is not None and (args.helper_laws or not 0 <= args.expect_mismatch <= 4294967295):
        parser.error('--expect-mismatch requires a u32 checksum and a task run')
    rows = []
    with tempfile.TemporaryDirectory(prefix='hirsel-langarena-') as temporary:
        scratch = Path(temporary)
        manifest = scratch / 'inputs.json'
        adapted = None
        function = 'main'
        if args.helper_laws:
            adapted = scratch / 'helper-laws.hirsel'
            source = ROOT / 'core/langarena' / (args.module + '.hirsel')
            adapted.write_text(source.read_text() + '\npub fn helper_main()->Result[(),Exit] {if helper_laws() {Ok(())} else {Error(Exit(status:1))}}\n')
            function = 'helper_main'
        manifest.write_text(json.dumps(inputs(ROOT, args.module, adapted)))
        host = scratch / 'host'
        host.mkdir()
        (host / 'config.json').write_bytes(args.config.read_bytes())
        for engine in ['cranelift', 'llvm'] if args.backend == 'both' else [args.backend]:
            binary = scratch / engine
            command = [str(args.compiler.resolve()), 'build', '--inputs', str(manifest), '--entry',
                       f'langarena@1/{args.module}/{function}', '--backend', engine,
                       '--runtime', str(args.runtime.resolve()), '--store', str(scratch / 'store'),
                       '--dir', str(host), '--grant', 'Stdout', '--grant', 'Tasks', '--json', '-o', str(binary)]
            if engine == 'llvm':
                command.append('--release')
            wait_load()
            build = subprocess.run(command, capture_output=True)
            row = dict(engine=engine, built=False, passed=False)
            if build.returncode:
                row['stop'] = build.stdout.decode(errors='replace') + build.stderr.decode(errors='replace')
            else:
                row['built'] = True
                wait_load()
                run = subprocess.run([str(binary), 'config.json', args.task], cwd=host, capture_output=True, timeout=120)
                row.update(exit=run.returncode, stdout=run.stdout.decode(errors='replace'), stderr=run.stderr.decode(errors='replace'))
                expected = ''
                if not args.helper_laws:
                    config = json.loads(args.config.read_text())
                    item = next(c for c in config if c['name'] == args.task) if isinstance(config, list) else config[args.task]
                    checksum = item['checksum']
                    expected = f'{args.task} checksum={checksum} expected={checksum} status=OK\n'
                status = 0
                if args.expect_mismatch is not None:
                    if args.expect_mismatch == checksum:
                        parser.error('--expect-mismatch checksum equals the configured checksum')
                    expected = f'{args.task} checksum={args.expect_mismatch} expected={checksum} status=ERR\n'
                    status = 1
                row['passed'] = run.returncode == status and row['stdout'] == expected and not row['stderr']
            rows.append(row)
            print(json.dumps(row), flush=True)
    if args.report:
        args.report.write_text(json.dumps(rows, indent=2) + '\n')
    return int(any(not row['passed'] for row in rows))


if __name__ == '__main__':
    raise SystemExit(main())
