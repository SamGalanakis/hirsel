#!/usr/bin/env python3
"""Complete selected LAYOUT cells once, with shipping and observed runs kept separate."""
import argparse
import fcntl
import json
import os
from pathlib import Path
import re
import shutil
import shlex
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / 'core/tools'))
import measure
import measurement_artifacts
import std_native
from run import manifest, PHASES

# The two own families, with each refusal/malformed phase kept separate.
MATRIX = ['ordinary', 'sized', 'streaming', 'json_parse', 'json_print',
          'exclusive', 'churn', 'rc', 'arc', 'lifetime',
          'ordinary_capacity', 'sized_capacity', 'streaming_capacity',
          'exclusive_capacity', 'churn_capacity', 'rc_capacity', 'arc_capacity',
          'graph_malformed', 'document_malformed', 'json_reject', 'json_capacity', 'json_utf8']


def save(args, row):
    with (args.out / 'results.jsonl').open('a') as out:
        out.write(json.dumps(row, default=str) + '\n')
    print(json.dumps(row, default=str), flush=True)


def execute(command, **kwargs):
    std_native.wait_load()
    return subprocess.run(list(map(str, command)), capture_output=True, **kwargs)


def sample(command, directory):
    with open('/tmp/hirsel-langarena-measure.lock', 'a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        std_native.wait_load()
        return measure.sample(['taskset', '-c', '16', *map(str, command)], directory)


def build(args, phase, backend, observed=False):
    module = PHASES[phase]
    stem = args.out / f'{phase}-{backend}'
    inputs = stem.with_suffix('.inputs.json')
    inputs.write_text(json.dumps(manifest(module)))
    command = [args.artifacts['compiler'], 'build', '--inputs', inputs, '--entry',
               f'layout@1/{module}/{phase}_main', '--backend', backend,
               '--runtime', args.observed_runtime if observed else args.artifacts['runtime'],
               '--store', args.out / 'store', '--grant', 'Stdout', '--json', '-o', stem]
    if module == 'json':
        command += ['--dir', Path(__file__).with_name('inputs').resolve()]
    if args.events:
        linker = args.out / 'recording-cc'
        linker.write_text('#!/bin/sh\nexec cc \"$@\" ' + shlex.quote(str(Path(__file__).with_name('arc_events.c'))) + ' -I' + shlex.quote(str(ROOT / 'crates/hirsel-core-rt/include')) + ' -Wl,--wrap=hirsel_core_os_run\n')
        linker.chmod(0o755)
        command += ['--cc', linker]
    if backend == 'llvm':
        command += ['--release']
    if observed:
        command += ['--logical-charge-report', stem.with_suffix('.memory.json')]
    result = execute(command)
    stem.with_suffix('.build.stdout').write_bytes(result.stdout)
    stem.with_suffix('.build.stderr').write_bytes(result.stderr)
    try:
        product = json.loads(result.stdout)
    except ValueError:
        product = {'stopped': result.stderr.decode(errors='replace')}
    if result.returncode or not product.get('native'):
        return stem, std_native.first_stop(product)
    return stem, None


def runtime(args):
    expected = json.loads(Path(__file__).with_name('inputs').joinpath('expected.json').read_text())['phases']
    for phase in args.phase:
        for backend in args.backend:
            row = dict(stage=args.stage, phase=phase, backend=backend, event_mode='recording-sink' if args.events else 'default-no-sink', **args.identity)
            binary, cause = build(args, phase, backend, args.stage == 'observe')
            if cause:
                save(args, dict(row, status='not-run', cause=cause))
                continue
            if args.stage == 'observe':
                with open('/tmp/hirsel-langarena-measure.lock', 'a') as lock:
                    fcntl.flock(lock, fcntl.LOCK_EX)
                    ran = execute([ROOT / 'tools/capped', 'taskset', '-c', '16', binary])
                binary.with_suffix('.stdout').write_bytes(ran.stdout)
                binary.with_suffix('.stderr').write_bytes(ran.stderr)
                row['status'] = 'pass' if ran.returncode == 0 and ran.stdout == f'{expected[phase]}\n'.encode() else 'fail'
                report = binary.with_suffix('.memory.json')
                if report.exists():
                    row['memory'] = json.loads(report.read_text())
                else:
                    row.update(status='no-report', cause='observer report missing')
            else:
                row.update(sample([binary], args.out / f'{phase}-{backend}-sample'))
                if (args.out / f'{phase}-{backend}-sample/stdout').read_bytes() != f'{expected[phase]}\n'.encode():
                    row['status'] = 'wrong-output'
            if args.events:
                stderr = ran.stderr.decode(errors='replace') if args.stage == 'observe' else (args.out / f'{phase}-{backend}-sample/stderr').read_text()
                counts = re.search(r'Arc events new=(\d+) share=(\d+) release=(\d+) last=(\d+)', stderr)
                row['arc_events'] = dict(zip(['new', 'share', 'release', 'last'], map(int, counts.groups()))) if counts else None
                if row['arc_events'] != dict(new=8512, share=512, release=9024, last=8512):
                    row['status'] = 'wrong-event-schedule'
            save(args, row)
            if args.stage == 'observe':
                binary.unlink()


def laws(args):
    for module in args.module:
        source = Path(__file__).with_name(module + '.hirsel').read_text()
        cases = re.findall(r'^// expect: run (\w+\(\)) = (.+)$', source, re.M)
        if args.case:
            cases = [(c, e) for c, e in cases if c[:-2] in args.case]
        if not cases:
            raise ValueError('zero selected law bodies')
        for call, expected in cases:
            for backend in args.backend:
                name = call[:-2]
                stem = args.out / f'{module}-{name}-{backend}'
                adapted = stem.with_suffix('.hirsel')
                evaluating = backend == 'evaluator'
                ret, yes, no = ('u64', '1u64', '0u64') if evaluating else ('Result[(),()]', 'Ok(())', 'Error(())')
                adapter = f'\npub fn native_contract()->{ret} {{ if {call} == {expected} {{{yes}}} else {{{no}}} }}\n'
                if not evaluating:
                    adapter = '\n@test' + adapter
                adapted.write_text(source + adapter)
                inputs = manifest(module)
                inputs['packages'][-1]['sources'][0]['file'] = str(adapted)
                request = stem.with_suffix('.inputs.json')
                request.write_text(json.dumps(inputs))
                entry = f'layout@1/{module}/native_contract'
                command = ([args.evaluator, request, entry] if evaluating else
                           [args.artifacts['compiler'], 'test', '--inputs', request, '--backend', backend,
                            '--runtime', args.artifacts['runtime'], '--store', args.out / 'store',
                            '--limit', '67108864', '--test-timeout', '30', '--json', '-o', stem.with_suffix('.programs')])
                if backend == 'llvm':
                    command += ['--release']
                result = execute(command)
                stem.with_suffix('.stdout').write_bytes(result.stdout)
                stem.with_suffix('.stderr').write_bytes(result.stderr)
                case = std_native.Contract(adapted, 1, call, expected, False)
                ran, cause = (std_native.evaluator_observation(case, result) if evaluating else
                              std_native.test_observation(case, result, entry))
                save(args, dict(stage='law', module=module, case=name, backend=backend, ran=ran,
                                status='pass' if ran and cause is None else 'fail' if ran else 'not-run', cause=cause))
                programs = stem.with_suffix('.programs')
                if programs.is_dir():
                    shutil.rmtree(programs)


def twins(args):
    rustc = ROOT / 'tools/buck2/toolchains/rust-files/bin/rustc'
    environment = execute([rustc, '-vV'])
    (args.out / 'rustc.txt').write_bytes(environment.stdout)
    expected = json.loads(Path(__file__).with_name('inputs').joinpath('expected.json').read_text())['phases']
    for backend in args.backend:
        flags = ['--edition=2024', '--target=x86_64-unknown-linux-gnu', '-C', 'target-cpu=x86-64',
                 '-C', 'opt-level=' + ('3' if backend == 'llvm' else '0'), '-C', 'overflow-checks=yes',
                 '-C', 'codegen-units=1', '-C', 'lto=no', '-C', 'debuginfo=0',
                 '-C', 'debug-assertions=' + ('no' if backend == 'llvm' else 'yes')]
        for side in args.side or ['graph_matched', 'graph_idiomatic']:
            binary = args.out / f'{side}-{backend}'
            built = execute([rustc, *flags, Path(__file__).with_name(side + '.rs'), '-o', binary])
            if built.returncode:
                raise ValueError(built.stderr.decode())
            for phase in args.phase:
                row = dict(stage='twin', phase=phase, backend=backend, side=side, rustc_flags=flags)
                row.update(sample([binary, phase], args.out / f'{phase}-{side}-{backend}-sample'))
                if (args.out / f'{phase}-{side}-{backend}-sample/stdout').read_bytes() != f'{expected[phase]}\n'.encode():
                    row['status'] = 'wrong-output'
                save(args, row)
            if not args.skip_twin_laws:
                tested = execute([binary, 'laws'])
                save(args, dict(stage='twin-laws', backend=backend, side=side,
                                status='pass' if tested.returncode == 0 and tested.stdout == b'0\n' else 'fail'))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--artifacts', type=Path, required=True)
    parser.add_argument('--observed-runtime', type=Path)
    parser.add_argument('--evaluator', type=Path)
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--stage', choices=['runtime', 'observe', 'laws', 'twins'], required=True)
    parser.add_argument('--phase', action='append', choices=PHASES, default=[])
    parser.add_argument('--matrix', action='store_true', help='all22 own-family runtime/observe phases; never Rust retakes')
    parser.add_argument('--backend', action='append', choices=['llvm', 'cranelift', 'evaluator'])
    parser.add_argument('--module', action='append', choices=['graph', 'documents', 'json'], default=[])
    parser.add_argument('--case', action='append')
    parser.add_argument('--side', action='append', choices=['graph_matched', 'graph_idiomatic'])
    parser.add_argument('--skip-twin-laws', action='store_true', help='reuse completed laws when resuming missing twin cells')
    parser.add_argument('--events', action='store_true', help='separate Arc recording-sink configuration')
    args = parser.parse_args()
    if args.matrix:
        if args.phase or args.stage not in ['runtime', 'observe']:
            parser.error('--matrix selects runtime/observe phases without --phase')
        args.phase = MATRIX
    if args.events and (args.phase != ['arc'] or args.stage not in ['runtime', 'observe']):
        parser.error('--events requires the single arc runtime/observe phase')
    args.out = args.out.resolve()
    args.out.mkdir(parents=True, exist_ok=False)
    args.artifacts = json.loads(args.artifacts.read_text())
    args.identity = measurement_artifacts.identity(Path(args.artifacts['compiler']), Path(args.artifacts['runtime']), args.artifacts['compiler_build_profile'])
    if args.stage == 'observe':
        if not args.observed_runtime:
            parser.error('observe requires --observed-runtime')
        args.identity = measurement_artifacts.identity(Path(args.artifacts['compiler']), args.observed_runtime,
                                                       args.artifacts['compiler_build_profile'], 'charge-observation', diagnostic=True)
    args.backend = args.backend or (['evaluator', 'cranelift', 'llvm'] if args.stage == 'laws' else ['llvm', 'cranelift'])
    os.environ['HIRSEL_MEMORY_LIMIT'] = '2G'
    {'laws': laws, 'runtime': runtime, 'observe': runtime, 'twins': twins}[args.stage](args)
    store = args.out / 'store'
    if store.is_dir():
        shutil.rmtree(store)
    rows = [json.loads(line) for line in (args.out / 'results.jsonl').read_text().splitlines()]
    return int(any(r['status'] not in ['pass', 'ok'] for r in rows))


if __name__ == '__main__':
    raise SystemExit(main())
