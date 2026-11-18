#!/usr/bin/env python3
"""CONC-10 event order: the evaluator against native, through the poll recorder.

Each fixture in fixtures/order runs natively (cranelift) under the recorder with
the default FIFO scheduler. The same program runs on the evaluator
(hirsel-core-contract-eval). Their
stdout and their whole CONC-10 event lists must be equal: every kind, none dropped.
Kind numbers come from the published header.

Normalisation, and why each part is engine-local rather than semantic:
- CANCELLED and CHECK_CANCEL carry a source site, numbered per engine.
- SPAWN carries the child's frame size, which is a layout fact.
- CAPACITY's object is the requested charge, a value; it is kept as is.
- Task and object ids are renamed by first appearance.

FAIL-15 fixtures state their hand-derived outcome in an `expect:` comment. These
also run on LLVM, including the named Deadlock stop and its recorded replay.
"""
import argparse
import json
import os
from pathlib import Path
import re
import shlex
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'core/tools'))
from std_native import Evaluator
FIXTURES = Path(__file__).resolve().parent / 'fixtures/order'
RECORDER = Path(__file__).resolve().parent / 'fixtures/polls/recorder.c'
KIND = {name: int(n) for name, n in re.findall(
    r'HIRSEL_CORE_EVENT_(\w+)=(\d+)', (ROOT / 'crates/hirsel-core-rt/include/hirsel_core_rt_host.h').read_text())}
SITED = {KIND['CANCELLED'], KIND['CHECK_CANCEL']}
SPAWN = KIND['SPAWN']


def normalized(events):
    names, out = {}, []
    for kind, task, obj, value in events:
        if kind in SITED:
            value = 'site'
        elif kind == SPAWN:
            value = 'frame'
        task = names.setdefault(('id', task), len(names))
        if kind not in SITED and kind != KIND['CAPACITY']:
            obj = names.setdefault(('id', obj), len(names))
        out.append((kind, task, obj, value))
    return out


def manifest(source):
    core = dict(name='core', version='1')
    return dict(core=core, packages=[
        dict(package=core, dependencies=[], sources=[dict(file=str(p), modules=[p.stem], signatures=True)
                                                      for p in sorted((ROOT / 'core/core').glob('*.hirsel'))]),
        dict(package=dict(name='app', version='1'), dependencies=[core],
             sources=[dict(file=str(source), modules=['main'], signatures=False)])])


def step_image_laws(args, evaluator, inputs, folder, source, recorder_linker):
    """Launch the named roots without the recorder's foreign link adapter."""
    roots = {'unbounded': True, 'bounded': False, 'flow_step_owner': False}
    failures = ('failure_before_slice', 'failure_at_slice', 'failure_after_slice')

    def position(offset):
        prefix = source[:offset]
        return prefix.count('\n') + 1, len(prefix.rsplit('\n', 1)[-1]) + 1

    divide = position(source.index('1u64 / d', source.index('fn exhausted_finalizer(')))
    finalizer = position(source.index('{', source.index('drop(f)')))
    causes = [('Division', *divide), ('OutOfTime', *finalizer)]
    # The evaluator delegates flows to the native reference.
    for name in ('unbounded', 'bounded'):
        report = evaluator.evaluate(inputs, f'app@1/main/{name}', program_entry=True)
        outcome = json.loads(report.stdout).get('outcome')
        if outcome != {'kind': 'returned', 'values': [1]}:
            raise ValueError(f'{name}: evaluator {outcome}')
    for name in failures:
        report = json.loads(evaluator.evaluate(inputs, f'app@1/main/{name}', program_entry=True).stdout)
        outcomes = report.get('outcome')
        sites = report.get('cause_sites', [])
        if outcomes != {'kind': 'failed', 'causes': [cause for cause, *_ in causes]} or len(sites) != 2 or any(
                site is None or not site[0].endswith('step-bounds.hirsel') or tuple(site[1:]) != expected
                for site, expected in zip(sites, (divide, finalizer), strict=True)):
            raise ValueError(f'{name}: evaluator causes {outcomes} at {sites}')

    def build(name, backend, store, certified, cc=args.cc):
        store.mkdir(parents=True, exist_ok=True)
        output = store / name
        command = [str(args.compiler), 'build', '--source-core', '--inputs', str(inputs),
                   '--entry', f'app@1/main/{name}', '--backend', backend,
                   '--runtime', str(args.runtime), '--store', str(store / 'store'),
                   '--cc', str(cc), '--json', '-o', str(output)]
        if backend == 'llvm':
            command.append('--release')
        env = dict(os.environ, XDG_CACHE_HOME=str(store / 'cache'), XDG_DATA_HOME=str(store / 'data'))
        result = subprocess.run(command, cwd=ROOT, env=env, capture_output=True)
        if result.returncode:
            raise ValueError(f'{name}/{backend}: build {result.stdout!r} {result.stderr!r}')
        actual = json.loads(result.stdout)['native']['step_charging_elided']
        if actual != certified:
            raise ValueError(f'{name}/{backend}: expected Step elision {certified}, got {actual}')
        ran = subprocess.run([output], capture_output=True)
        if name in failures:
            sites = [(kind, int(line), int(column)) for kind, line, column in re.findall(
                r'^(\w+) at .*step-bounds\.hirsel:(\d+):(\d+)$', ran.stderr.decode(), re.M)]
            if ran.returncode != 101 or ran.stdout or len(ran.stderr.splitlines()) != 2 or sites != causes:
                raise ValueError(f'{name}/{backend}: causes {ran}')
        elif (ran.returncode, ran.stdout, ran.stderr) != (0, b'', b''):
            raise ValueError(f'{name}/{backend}: run {ran}')
        return output.read_bytes()

    if 'cranelift' in (args.backend or ('cranelift', 'llvm')):
        for name in (*roots, *failures):
            build(name, 'cranelift', folder / 'cranelift-images', False)
    if 'llvm' not in (args.backend or ('cranelift', 'llvm')):
        return
    build('flow_step_owner', 'llvm', folder / 'flow-image', False)
    build('unbounded', 'llvm', folder / 'foreign-image', False, recorder_linker)
    for name in failures:
        build(name, 'llvm', folder / 'failure-images', False)
    pair = ('unbounded', 'bounded')
    cold = {name: build(name, 'llvm', folder / f'cold-{name}', roots[name]) for name in pair}
    for order in (pair, pair[::-1]):
        store = folder / f'warm-{order[0]}'
        for name in order:
            if build(name, 'llvm', store, roots[name]) != cold[name]:
                raise ValueError(f'{name}: release bytes differ after priming with {order[0]}')
    print('step-bounds: exact cause sites, metered flow stage and both cache orders pass', flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--compiler', type=Path, required=True)
    parser.add_argument('--evaluator', type=Path, required=True)
    parser.add_argument('--runtime', type=Path, default=os.environ.get('HIRSEL_CORE_RT_ARCHIVE'))
    parser.add_argument('--cc', type=Path, default=(Path(os.environ['LLVM_SYS_231_PREFIX']) / 'bin/clang')
                        if 'LLVM_SYS_231_PREFIX' in os.environ else Path('clang'))
    parser.add_argument('--only', nargs='+')
    parser.add_argument('--backend', nargs='+', choices=['cranelift', 'llvm'])
    parser.add_argument('--release', action='store_true')
    parser.add_argument('--test-threads', type=int, choices=[1], default=1)
    args = parser.parse_args()
    work = Path(tempfile.mkdtemp(prefix='event-order-'))
    recorder = work / 'recorder.o'
    subprocess.run([str(args.cc), '-c', '-fPIC', str(RECORDER), '-I', str(ROOT / 'crates/hirsel-core-rt/include'),
                    '-o', str(recorder)], check=True)
    linker = work / 'cc'
    linker.write_text(f'#!/bin/sh\nexec {shlex.quote(str(args.cc))} "$@" {shlex.quote(str(recorder))} '
                      '-Wl,--wrap=hirsel_core_run\n')
    linker.chmod(0o755)
    fixtures = sorted(p for p in FIXTURES.glob('*.hirsel') if not args.only or p.stem in args.only)
    assert fixtures, 'no fixture selected'
    failures = []
    unbuilt = 0
    with Evaluator(args.evaluator.resolve()) as evaluator:
        for fixture in fixtures:
            text = fixture.read_text()
            found = re.search(r'pub fn main\(([^)]*)\)[^{]*?(waits)?\s*\{', text)
            header = found.group(1)
            folder = work / fixture.stem
            folder.mkdir()
            inputs = folder / 'inputs.json'
            inputs.write_text(json.dumps(manifest(fixture)))
            ran = evaluator.evaluate(inputs, 'app@1/main/main', program_entry=True)
            report = json.loads(ran.stdout)
            oracle = re.search(r'// expect: (Deadlock|returned)(?: stdout=(\S*))?', text)
            stopped = oracle is not None and oracle.group(1) == 'Deadlock'
            outcome = {'kind': 'stopped', 'name': 'Deadlock'} if stopped else {'kind': 'returned', 'values': [1]}
            if report.get('outcome') != outcome:
                failures.append(f'{fixture.stem}: evaluator {report.get("outcome")}')
                continue
            printed = re.search(r'// hand-derived stdout: (.*)', text)
            if printed and bytes(report['stdout']) != printed.group(1).encode():
                failures.append(f'{fixture.stem}: evaluator stdout {bytes(report["stdout"])!r}, expected {printed.group(1)!r}')
                continue
            sends = re.search(r'// hand-derived try-send: (\d+)', text)
            if sends:
                count = sum(e[0] == KIND['TRY_SEND'] for e in report['events'])
                if count != int(sends.group(1)):
                    failures.append(f'{fixture.stem}: evaluator try-send count {count}, expected {sends.group(1)}')
                    continue
            by_task = re.search(r'// hand-derived try-send by task: ([\d,]+)', text)
            if by_task:
                want = [int(n) for n in by_task.group(1).split(',')]
                tasks = [0] + [e[2] for e in report['events'] if e[0] == SPAWN]
                got = [sum(e[0] == KIND['TRY_SEND'] and e[1] == task for e in report['events'])
                       for task in tasks]
                if got != want:
                    failures.append(f'{fixture.stem}: sends by task {got}, expected {want}')
                    continue
            capacity = re.search(r'// hand-derived capacity: (\d+),(\d+)', text)
            if capacity:
                want = [tuple(map(int, capacity.groups()))]
                got = [(e[2], e[3]) for e in report['events'] if e[0] == KIND['CAPACITY']]
                if got != want:
                    failures.append(f'{fixture.stem}: capacity events {got}, expected {want}')
                    continue
            refund = re.search(r'// hand-derived refund: (\d+),(\d+),(\d+)', text)
            if refund:
                want = tuple(map(int, refund.groups()))
                got = sum(tuple(e[1:]) == want for e in report['events'] if e[0] == KIND['REFUND'])
                if got != 1:
                    failures.append(f'{fixture.stem}: observed {got} copies of refund {want}, expected one')
                    continue
            slices = re.search(r'// hand-derived step slices: (\d+)', text)
            if slices:
                count = sum(e[0] == KIND['STEP_SLICE'] for e in report['events'])
                if count != int(slices.group(1)):
                    failures.append(f'{fixture.stem}: evaluator step-slice count {count}, expected {slices.group(1)}')
                    continue
            fixture_linker = linker
            if '// provider: pending stdout' in text:
                provider_recorder = folder / 'recorder.o'
                provider = folder / 'provider.o'
                for src, obj, flags in [(RECORDER, provider_recorder, ['-DPENDING_STDOUT']),
                                        (fixture.with_suffix('.c'), provider, [])]:
                    subprocess.run([str(args.cc), '-c', '-fPIC', str(src), '-I', str(ROOT / 'crates/hirsel-core-rt/include'),
                                    '-o', str(obj), *flags], check=True)
                fixture_linker = folder / 'cc'
                fixture_linker.write_text(f'#!/bin/sh\nexec {shlex.quote(str(args.cc))} "$@" '
                                          f'{shlex.quote(str(provider_recorder))} {shlex.quote(str(provider))} '
                                          '-lpthread -Wl,--wrap=hirsel_core_run\n')
                fixture_linker.chmod(0o755)
            for backend in args.backend or (('cranelift', 'llvm') if oracle else ('cranelift',)):
                command = [str(args.compiler), 'build', '--source-core', '--inputs', str(inputs), '--entry', 'app@1/main/main',
                           '--backend', backend, '--runtime', str(args.runtime), '--store', str(work / 'store'),
                           '--cc', str(fixture_linker), '--json', '-o', str(folder / backend)]
                if args.release:
                    command += ['--release']
                for grant in ('Tasks', 'Clock', 'Stdout'):
                    if re.search(rf':\s*{grant}\b', header):
                        command += ['--grant', grant]
                while os.getloadavg()[0] > 32:
                    time.sleep(10)
                built = subprocess.run(command, capture_output=True, cwd=ROOT)
                if '// native: unbuilt' in text:
                    if built.returncode and b'native step scopes are not built' in built.stdout + built.stderr:
                        unbuilt += 1
                        print(f'{fixture.stem}: evaluator hand count passed; native scope lowering is unbuilt', flush=True)
                    else:
                        failures.append(f'{fixture.stem}: native expected the explicit step-scope feature gap')
                    continue
                if built.returncode:
                    failures.append(f'{fixture.stem}/{backend}: build {built.returncode}: {built.stdout[-2000:]!r} {built.stderr[-2000:]!r}')
                    continue
                native_revision = (json.loads(built.stdout).get('native') or {}).get('revision')
                if report.get('revision') is None or report['revision'] != native_revision:
                    failures.append(f'{fixture.stem}/{backend}: evaluator/build entry revisions differ')
                    continue
                trace = folder / f'{backend}.trace'
                env = dict(os.environ, HIRSEL_POLL_TRACE=str(trace))
                try:
                    native = subprocess.run([str(folder / backend)], capture_output=True, timeout=30, env=env)
                except subprocess.TimeoutExpired:
                    failures.append(f'{fixture.stem}/{backend}: run did not end')
                    continue
                code = 102 if stopped else 0
                stderr = b'run stop: Deadlock\n' if stopped else b''
                if native.returncode != code or native.stderr != stderr:
                    failures.append(f'{fixture.stem}/{backend}: native exit {native.returncode} {native.stderr!r}')
                    continue
                expected = normalized([tuple(int(w) for w in line.split()[1:]) for line in trace.read_text().splitlines()
                                       if line.startswith('E ')])
                got = normalized([tuple(e) for e in report['events']])
                stdout = bytes(report['stdout'])
                if oracle and stdout != (oracle.group(2) or '').encode():
                    failures.append(f'{fixture.stem}: stdout {stdout!r}, expected {oracle.group(2)!r}')
                elif stdout != native.stdout or bytes(report['stderr']) != native.stderr:
                    failures.append(f'{fixture.stem}/{backend}: output evaluator {stdout!r}, native {native.stdout!r}')
                elif got != expected:
                    at = next((i for i, (a, b) in enumerate(zip(got, expected)) if a != b), min(len(got), len(expected)))
                    failures.append(f'{fixture.stem}/{backend}: events differ at {at}: evaluator {got[at:at + 4]}, native {expected[at:at + 4]}')
                else:
                    print(f'{fixture.stem}/{backend}: {len(got)} events equal, {outcome}', flush=True)
                if stopped or slices:
                    replay = subprocess.run([str(folder / backend)], capture_output=True, timeout=30,
                                            env=dict(env, HIRSEL_POLL_REPLAY='1'))
                    if (replay.returncode, replay.stdout, replay.stderr) != (native.returncode, native.stdout, native.stderr):
                        failures.append(f'{fixture.stem}/{backend}: replay differs: {replay}')
                    else:
                        print(f'{fixture.stem}/{backend}: replay agrees', flush=True)
            if fixture.stem == 'step-bounds' and args.release:
                try:
                    step_image_laws(args, evaluator, inputs, folder, text, fixture_linker)
                except ValueError as error:
                    failures.append(str(error))
    for failure in failures:
        print('FAIL', failure, flush=True)
    print(f'event order: {len(fixtures) - len(failures) - unbuilt} equal, {unbuilt} evaluator-only, {len(failures)} differ', flush=True)
    return int(bool(failures))


if __name__ == '__main__':
    sys.exit(main())
