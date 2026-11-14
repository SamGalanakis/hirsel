#!/usr/bin/env python3
"""Execute the existing core/front/runtime C/CLI oracles on a declared target.

The cases, grants, limits, closed streams and private string fixture match
hirsel-core-driver/src/core_ops_native.rs. Every selected source actually runs.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]
NAMES = ['args-empty', 'args-len', 'args-print', 'args-returned-views', 'args-values',
         'clocks', 'host-close-cancel', 'host-read', 'host-wait-cancel', 'host',
         'mkdtemp-bounds', 'mkdtemp', 'stderr-cancel', 'stderr-closed', 'stdout-cancel',
         'stdout-closed', 'streams', 'string-growth-refusal', 'string-growth', 'string-shorten']


def run(compiler, runtime, root, cc=None, target=None, emulator=None, sysroot=None, only=None, report=None, backend="llvm", optimize=True):
    paths = {p.stem: p for p in (root / 'core/front/runtime').glob('*.hirsel')}
    if set(paths) != set(NAMES):
        raise ValueError(f'runtime inventory changed: missing {set(NAMES)-set(paths)}, new {set(paths)-set(NAMES)}')
    names = [name for name in NAMES if only is None or name in only]
    if not names or only and set(only) - set(names):
        raise ValueError('unknown or empty runtime selection')
    rows = []
    with tempfile.TemporaryDirectory(prefix='hirsel-front-native-') as temporary:
        work = Path(temporary)
        for name in names:
            scratch = work / name
            scratch.mkdir()
            grant = scratch / 'grant'
            grant.mkdir()
            if name in ('host-read', 'host-close-cancel', 'host-wait-cancel'):
                (grant / 'input').write_bytes(b'hello')
            core = {'name': 'core', 'version': '1'}
            sources = [{'file': str(p.resolve()), 'modules': [p.stem], 'signatures': True}
                       for p in sorted((root / 'core/core').glob('*.hirsel'))]
            if name in ('mkdtemp', 'host-wait-cancel'):
                string = next(s for s in sources if s['modules'] == ['string'])
                adapted = scratch / 'string.hirsel'
                adapted.write_text(Path(string['file']).read_text() + '\npub fn proof_temp_name() -> String { let b:Bytes = result.unwrap(list.with_capacity(32)); of_bytes(take b) }\n')
                string['file'] = str(adapted)
            inputs = scratch / 'inputs.json'
            inputs.write_text(json.dumps({'core': core, 'packages': [
                {'package': core, 'dependencies': [], 'sources': sources},
                {'package': {'name': 'app', 'version': '1'}, 'dependencies': [core],
                 'sources': [{'file': str(paths[name]), 'modules': ['main'], 'signatures': False}]}]}))
            limit = {'args-returned-views': 288, 'string-growth': 304,
                     'string-growth-refusal': 288, 'string-shorten': 296}.get(name, 1_048_576)
            grants = []
            if name in ('streams', 'args-print', 'stdout-closed', 'stderr-closed', 'stdout-cancel', 'stderr-cancel'):
                grants = ['Stdout', 'Stderr', 'Tasks']
            if name == 'args-returned-views':
                grants = ['Stdout']
            host = name.startswith('host') or name in ('clocks', 'mkdtemp', 'mkdtemp-bounds')
            if host:
                grants = ['Clock', 'Tasks']
            binary = scratch / 'program'
            command = [str(compiler), 'build', '--source-core', '--inputs', str(inputs),
                       '--entry', 'app@1/main/main', '--backend', backend,
                       '--runtime', str(runtime), '--store', str(work / 'store'),
                       '--limit', str(limit), '--json', '-o', str(binary)]
            if optimize:
                command.append('--release')
            if host:
                command.extend(['--dir', str(grant)])
            for capability in grants:
                command.extend(['--grant', capability])
            if target:
                command.extend(['--target', target])
            if cc:
                command.extend(['--cc', str(cc)])
            row = dict(case=name, target=target, built=False, ran=False, passed=False, failure=None)
            try:
                built = subprocess.run(command, cwd=root, capture_output=True)
                product = json.loads(built.stdout)
                if built.returncode or not product.get('accepted') or not product.get('native'):
                    raise ValueError(f'build exit {built.returncode}: {product}; {built.stderr.decode(errors="replace")}')
                row['built'] = True
                arguments = ['alpha', 'beta', ''] if name in ('args-values', 'args-print', 'args-returned-views') else []
                run_command = ([str(emulator), '-L', str(sysroot)] if emulator else []) + [str(binary), *arguments]
                writer = None
                stdout, stderr = subprocess.PIPE, subprocess.PIPE
                if name in ('stdout-closed', 'stderr-closed'):
                    reader, writer = os.pipe()
                    os.close(reader)
                    if name == 'stdout-closed':
                        stdout = writer
                    else:
                        stderr = writer
                try:
                    result = subprocess.run(run_command, cwd=scratch, stdout=stdout, stderr=stderr)
                finally:
                    if writer is not None:
                        os.close(writer)
                row['ran'] = True
                want_stdout = b'alphabeta' if name in ('args-print', 'args-returned-views') else b'out' if name == 'streams' else b''
                want_stderr = b'err' if name == 'streams' else b''
                want_exit = 101 if name == 'mkdtemp-bounds' else 0
                assert result.returncode == want_exit, f'exit {result.returncode}, stderr={result.stderr!r}'
                assert (result.stdout or b'') == want_stdout, f'stdout {result.stdout!r} != {want_stdout!r}'
                if name == 'mkdtemp-bounds':
                    assert result.stderr.startswith(b'Bounds'), repr(result.stderr)
                else:
                    assert (result.stderr or b'') == want_stderr, f'stderr {result.stderr!r} != {want_stderr!r}'
                if name in ('host-read', 'host-close-cancel', 'host-wait-cancel'):
                    assert (grant / 'input').read_bytes() == b'hello'
                elif host:
                    assert not list(grant.iterdir()), 'granted directory has unexpected contents'
                row['passed'] = True
            except (OSError, ValueError, AssertionError) as error:
                row['failure'] = str(error)
            rows.append(row)
            print(f'{name}: ' + ('PASS' if row['passed'] else 'FAIL ' + row['failure']), flush=True)
            if report:
                report.write_text(json.dumps(rows, indent=2) + '\n')
    return rows


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, default=ROOT)
    parser.add_argument('--compiler', type=Path, required=True)
    parser.add_argument('--runtime', type=Path, required=True)
    parser.add_argument('--cc', type=Path)
    parser.add_argument('--backend', choices=['llvm', 'cranelift'], default='llvm')
    parser.add_argument('--dev', action='store_true')
    parser.add_argument('--target')
    parser.add_argument('--emulator', type=Path)
    parser.add_argument('--sysroot', type=Path)
    parser.add_argument('--only', nargs='+')
    parser.add_argument('--report', type=Path)
    args = parser.parse_args()
    if bool(args.emulator) != bool(args.sysroot):
        parser.error('--emulator and --sysroot must be supplied together')
    rows = run(args.compiler.resolve(), args.runtime.resolve(), args.root.resolve(),
               args.cc.resolve() if args.cc else None, args.target,
               args.emulator.resolve() if args.emulator else None,
               args.sysroot.resolve() if args.sysroot else None, args.only, args.report, args.backend, not args.dev)
    print(f'{sum(r["ran"] for r in rows)} ran; {sum(r["passed"] for r in rows)}/{len(rows)} passed')
    return int(not all(r['passed'] for r in rows))


if __name__ == '__main__':
    raise SystemExit(main())
