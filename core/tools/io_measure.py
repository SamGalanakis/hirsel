#!/usr/bin/env python3
"""One serial sample per I/O side/profile with exact parity and mandatory M2 flags."""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys
from langarena import inputs
from std_native import wait_load

ROOT = Path(__file__).resolve().parents[2]
LINE = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789.\n"
SIZE = len(LINE) * 65536


def checked(command, **kwargs):
    wait_load()
    result = subprocess.run(command, capture_output=True, **kwargs)
    if result.returncode:
        raise ValueError(f'{command}: {result.returncode}\n{result.stdout!r}\n{result.stderr!r}')
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--compiler', type=Path, required=True)
    parser.add_argument('--runtime', type=Path, required=True)
    parser.add_argument('--rustc', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--profile', choices=['m2'], default='m2',
                        help='only M2 profiles are admitted; flags cannot be overridden')
    args = parser.parse_args()
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    rustc = args.rustc.resolve()
    version = checked([str(rustc), '-vV']).stdout.decode()
    if not version.startswith('rustc 1.99.0 '):
        raise ValueError(f'M2 requires rustc 1.99.0: {version}')
    (out / 'rustc.txt').write_text(version)
    (out / 'host.txt').write_text(subprocess.check_output(['uname', '-a'], text=True))
    host = out / 'host'
    host.mkdir()
    (host / 'input').write_bytes(LINE * 65536)
    (host / 'parity').write_bytes((ROOT / 'core/langarena/tests/io/parity.txt').read_bytes())
    outputs = {}
    for engine, opt in [('cranelift', '0'), ('llvm', '3')]:
        rust = out / f'rust-{engine}'
        checked([str(rustc), '--edition=2024', '--target=x86_64-unknown-linux-gnu',
                 '-C', 'opt-level=' + opt, '-C', 'overflow-checks=yes',
                 '-C', 'codegen-units=1', '-C', 'lto=off', '-C', 'debuginfo=0',
                 '-C', 'target-cpu=x86-64', str(ROOT / 'core/langarena/tests/io/rust.rs'),
                 '-o', str(rust)])
        for operation in ['read', 'write']:
            module = 'tests/io/' + operation
            manifest = out / f'{operation}.inputs.json'
            manifest.write_text(json.dumps(inputs(ROOT, module)))
            binary = out / f'hirsel-{engine}-{operation}'
            command = [str(args.compiler.resolve()), 'build', '--inputs', str(manifest),
                       '--entry', f'langarena@1/{module}/main', '--backend', engine,
                       '--runtime', str(args.runtime.resolve()), '--store', str(out / 'store'),
                       '--dir', str(host), '--grant', 'Stdout', '--json', '-o', str(binary)]
            if engine == 'llvm':
                command.append('--release')
            build = checked(command)
            (out / f'{engine}-{operation}.build.json').write_bytes(build.stdout)
            if operation == 'read':
                expected = (ROOT / 'core/langarena/tests/io/parity.expected').read_bytes()
                for side, executable in [('rust', rust), ('hirsel', binary)]:
                    run = checked([str(executable), str(host / 'parity') if side == 'rust' else 'parity'])
                    if run.stdout != expected or run.stderr:
                        raise ValueError(f'{side}/{engine} parity: {run.stdout!r}/{run.stderr!r}, expected {expected!r}')
                outputs[engine + '/parity'] = '5 lines / 12 UTF8 bytes; both sides pass'
            for side, executable in [('rust', rust), ('hirsel', binary)]:
                stem = out / f'{engine}-{operation}-{side}'
                arguments = [str(host / 'input') if side == 'rust' else 'input'] if operation == 'read' else []
                command = ['taskset', '-c', '16', str(ROOT / 'tools/capped'),
                           'perf', 'stat', '--no-big-num', '-x', ';', '-e', 'instructions:u',
                           '-o', str(stem) + '.perf', '--', '/usr/bin/time',
                           '-f', '%M', '-o', str(stem) + '.rss', str(executable), *arguments]
                wait_load()
                with Path(str(stem) + '.stdout').open('wb') as stdout, Path(str(stem) + '.stderr').open('wb') as stderr:
                    run = subprocess.run(command, stdout=stdout, stderr=stderr,
                                         env=os.environ | {'HIRSEL_MEMORY_LIMIT': '2G', 'HIRSEL_JOBS': '1'})
                if run.returncode:
                    raise ValueError(f'{side}/{engine}/{operation}: exit {run.returncode}; no ratio')
                expected = b'65536 4128768\n' if operation == 'read' else LINE * 65536
                if Path(str(stem) + '.stdout').read_bytes() != expected:
                    raise ValueError(f'{side}/{engine}/{operation}: wrong output; no ratio')
                perf = Path(str(stem) + '.perf').read_text()
                counts = [line.split(';')[0].strip() for line in perf.splitlines()
                          if ';instructions:u;' in line]
                if len(counts) != 1 or not counts[0].isdigit():
                    raise ValueError(f'incomplete perf sample: {perf}')
                instructions = int(counts[0])
                rss = int(Path(str(stem) + '.rss').read_text())
                outputs[f'{engine}/{operation}/{side}'] = dict(
                    instructions=instructions, input_bytes=SIZE, instructions_per_byte=instructions / SIZE,
                    peak_rss_kib=rss)
                print(engine, operation, side, outputs[f'{engine}/{operation}/{side}'], flush=True)
                (out / 'results.json').write_text(json.dumps(outputs, indent=2) + '\n')
            lhs = outputs[f'{engine}/{operation}/hirsel']['instructions']
            rhs = outputs[f'{engine}/{operation}/rust']['instructions']
            outputs[f'{engine}/{operation}/ratio'] = lhs / rhs
    (out / 'results.json').write_text(json.dumps(outputs, indent=2) + '\n')


if __name__ == '__main__':
    try:
        main()
    except (OSError, ValueError) as error:
        print(error, file=sys.stderr)
        raise SystemExit(1)
