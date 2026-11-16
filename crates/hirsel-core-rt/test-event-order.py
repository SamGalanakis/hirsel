#!/usr/bin/env python3
"""CONC-10 event order: the evaluator against native, through the poll recorder.

Each fixture in fixtures/order runs natively (cranelift) under the recorder with
HIRSEL_POLL_FIRST, which always runs the first runnable candidate: native's default
FIFO rule. The same program runs on the evaluator (hirsel-core-contract-eval). Their
stdout and their whole CONC-10 event lists must be equal: every kind, none dropped.
Kind numbers come from the published header.

Normalisation, and why each part is engine-local rather than semantic:
- CANCELLED and CHECK_CANCEL carry a source site, numbered per engine.
- SPAWN carries the child's frame size, which is a layout fact.
- CAPACITY's object is the requested charge, a value; it is kept as is.
- Task and object ids are renamed by first appearance.

A run where every task waits is not compared: the evaluator reports it as Stuck,
a diagnostic with no specified result. None of these fixtures is such a run.
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


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--compiler', type=Path, required=True)
    parser.add_argument('--evaluator', type=Path, required=True)
    parser.add_argument('--runtime', type=Path, default=os.environ.get('HIRSEL_CORE_RT_ARCHIVE'))
    parser.add_argument('--cc', type=Path, default=(Path(os.environ['LLVM_SYS_231_PREFIX']) / 'bin/clang')
                        if 'LLVM_SYS_231_PREFIX' in os.environ else Path('clang'))
    parser.add_argument('--only', nargs='+')
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
    for fixture in fixtures:
        text = fixture.read_text()
        found = re.search(r'pub fn main\(([^)]*)\)[^{]*?(waits)?\s*\{', text)
        header = found.group(1)
        names = ', '.join(a.split(':')[0].strip() for a in header.split(',') if a.strip())
        folder = work / fixture.stem
        folder.mkdir()
        # The evaluator's entry returns a word; `main` returns its Result through a place.
        def wrapped(source, waits):
            return source + (f'\npub fn evaluator_main({header}) -> u64{" waits" if waits else ""} '
                             f'{{ match main({names}) {{ Ok(()) => 1u64, Error(_) => 0u64 }} }}\n')
        adapted = folder / 'main.hirsel'
        adapted.write_text(wrapped(text, found.group(2)))
        inputs = folder / 'inputs.json'
        inputs.write_text(json.dumps(manifest(adapted)))
        ran = subprocess.run([str(args.evaluator), str(inputs), 'app@1/main/evaluator_main'], capture_output=True, timeout=120)
        # The evaluator checks `main` as an ordinary function, not as the entry LANG-8
        # exempts: one declared `waits` that waits on nothing runs there without it.
        if found.group(2) and b'declared `waits` but calls nothing that waits' in ran.stdout:
            ordinary = folder / 'evaluator' / 'main.hirsel'
            ordinary.parent.mkdir()
            ordinary.write_text(wrapped(text[:found.start(2)] + text[found.end(2):], None))
            (ordinary.parent / 'inputs.json').write_text(json.dumps(manifest(ordinary)))
            ran = subprocess.run([str(args.evaluator), str(ordinary.parent / 'inputs.json'),
                                  'app@1/main/evaluator_main'], capture_output=True, timeout=120)
        report = json.loads(ran.stdout)
        if report.get('outcome', {}).get('kind') != 'returned' or report['outcome'].get('values') != [1]:
            failures.append(f'{fixture.stem}: evaluator {report.get("outcome")}')
            continue
        command = [str(args.compiler), 'build', '--source-core', '--inputs', str(inputs), '--entry', 'app@1/main/main',
                   '--backend', 'cranelift', '--runtime', str(args.runtime), '--store', str(work / 'store'),
                   '--cc', str(linker), '--json', '-o', str(folder / 'program')]
        for grant in ('Tasks', 'Clock', 'Stdout'):
            if re.search(rf':\s*{grant}\b', header):
                command += ['--grant', grant]
        while os.getloadavg()[0] > 32:
            time.sleep(10)
        built = subprocess.run(command, capture_output=True, cwd=ROOT)
        if built.returncode:
            failures.append(f'{fixture.stem}: build {built.returncode}: {built.stdout[-2000:]!r} {built.stderr[-2000:]!r}')
            continue
        trace = folder / 'trace'
        native = subprocess.run([str(folder / 'program')], capture_output=True, timeout=30,
                                env=dict(os.environ, HIRSEL_POLL_TRACE=str(trace), HIRSEL_POLL_FIRST='1'))
        if native.returncode:
            failures.append(f'{fixture.stem}: native exit {native.returncode} {native.stderr!r}')
            continue
        expected = normalized([tuple(int(w) for w in line.split()[1:]) for line in trace.read_text().splitlines()
                               if line.startswith('E ')])
        got = normalized([tuple(e) for e in report['events']])
        stdout = bytes(report['stdout'])
        if stdout != native.stdout:
            failures.append(f'{fixture.stem}: stdout evaluator {stdout!r}, native {native.stdout!r}')
        elif got != expected:
            at = next((i for i, (a, b) in enumerate(zip(got, expected)) if a != b), min(len(got), len(expected)))
            failures.append(f'{fixture.stem}: events differ at {at}: evaluator {got[at:at + 4]}, native {expected[at:at + 4]}')
        else:
            print(f'{fixture.stem}: {len(got)} events equal', flush=True)
    for failure in failures:
        print('FAIL', failure, flush=True)
    print(f'event order: {len(fixtures) - len(failures)} equal, {len(failures)} differ', flush=True)
    return int(bool(failures))


if __name__ == '__main__':
    sys.exit(main())
