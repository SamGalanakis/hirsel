#!/usr/bin/env python3
"""Native source polls: both engines, exact record/replay, test-side mutant caps."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import time

ROOT = Path(__file__).resolve().parents[2]
FIXTURES = Path(__file__).resolve().parent / 'fixtures/polls'


def low_load():
    while os.getloadavg()[0] > 32:
        time.sleep(10)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--compiler', required=True, type=Path)
    parser.add_argument('--runtime', required=True, type=Path)
    parser.add_argument('--before', type=Path)
    parser.add_argument('--out', required=True, type=Path)
    parser.add_argument('--only', nargs='+', default=['full', 'check', 'arc'])
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    wrapper = args.out / 'cc'
    # This is a test-only link observer; neither emitted code nor runtime has a
    # timeout, mutation flag, or file recorder. Compiler objects remain unchanged.
    wrapper.write_text('#!/usr/bin/env python3\nimport os,sys\na=sys.argv[1:]\n'
                       'if "--version" not in a:\n a += ' + repr([
                           str(FIXTURES / 'recorder.c'), '-I', str(ROOT / 'crates/hirsel-core-rt/include'),
                           '-Wl,--wrap=hirsel_core_run']) + '\nos.execvp("cc",["cc",*a])\n')
    wrapper.chmod(0o755)
    core = dict(name='core', version='1')
    template = dict(core=core, packages=[
        dict(package=core, sources=[dict(file=str(p), modules=[p.stem], signatures=True)
                                   for p in sorted((ROOT / 'core/core').glob('*.hirsel'))], dependencies=[]),
        dict(package=dict(name='app', version='1'), sources=[dict(file='', modules=['main'], signatures=False)], dependencies=[core])])
    rows = []
    for kind in args.only:
        for engine in ['cranelift', 'llvm']:
            folder = args.out / f'{kind}-{engine}'
            folder.mkdir(exist_ok=True)
            for package in template['packages']:
                if package['package']['name'] == 'app':
                    package['sources'][0]['file'] = str(FIXTURES / f'{kind}.hirsel')
            inputs = folder / 'inputs.json'
            inputs.write_text(json.dumps(template))
            binary = folder / 'program'
            command = [str(args.compiler.resolve()), 'build', '--inputs', str(inputs), '--entry', 'app@1/main/main',
                       '--backend', engine, '--runtime', str(args.runtime.resolve()), '--grant', 'Tasks', '--store',
                       str(folder / 'store'), '--cc', str(wrapper), '--json', '-o', str(binary)]
            if engine == 'llvm':
                command.append('--release')
            low_load()
            build = subprocess.run(command, capture_output=True, cwd=ROOT)
            (folder / 'build.json').write_bytes(build.stdout)
            (folder / 'build.stderr').write_bytes(build.stderr)
            if build.returncode:
                raise RuntimeError(f'{kind}/{engine}: build {build.returncode}: {build.stdout[-3000:]!r} {build.stderr!r}')
            trace = folder / 'trace'
            env = dict(os.environ, HIRSEL_POLL_TRACE=str(trace))
            record = subprocess.run([str(binary)], capture_output=True, env=env, timeout=5)
            replay = subprocess.run([str(binary)], capture_output=True, env=dict(env, HIRSEL_POLL_REPLAY='1'), timeout=5)
            plain = subprocess.run([str(binary)], capture_output=True, timeout=5)
            assert (record.returncode, record.stdout, record.stderr) == (0, b'', b''), (kind, engine, record)
            assert (replay.returncode, replay.stdout, replay.stderr) == (0, b'', b'')
            assert (plain.returncode, plain.stdout, plain.stderr) == (0, b'', b'')
            events = [line.split() for line in trace.read_text().splitlines() if line.startswith('E ')]
            cancels = [e for e in events if e[1] == '3']
            assert bool(cancels) == (kind != 'arc'), (kind, cancels)
            row = dict(kind=kind, engine=engine, recorded=True, replayed=True, unrecorded=True,
                       events=len(events), cancelled=cancels)
            # The original archive is the mutant that lacks Full/Arc scheduling.
            # Its check_cancel already yields; remove only the new switch via the
            # separate test-source mutant for that row, reported alongside this run.
            if args.before and kind != 'check':
                mutant = folder / 'mutant'
                command[command.index(str(args.runtime.resolve()))] = str(args.before.resolve())
                command[command.index(str(binary))] = str(mutant)
                low_load()
                built = subprocess.run(command, capture_output=True, cwd=ROOT)
                assert built.returncode == 0, built.stdout
                try:
                    subprocess.run([str(mutant)], capture_output=True, timeout=2)
                except subprocess.TimeoutExpired:
                    row['no_switch_mutant'] = 'timed out after 2s (test-side)'
                else:
                    raise AssertionError(f'{kind}/{engine} no-switch mutant returned')
            rows.append(row)
            (args.out / 'results.json').write_text(json.dumps(rows, indent=2) + '\n')
            print(kind, engine, 'record/replay/plain pass', row.get('no_switch_mutant', ''), flush=True)
    # Source sites differ between engines' local numbering; the decision/event
    # order and all semantic values besides the engine-local site are identical.
    for kind in args.only:
        def normalized(engine):
            lines = []
            for line in (args.out / f'{kind}-{engine}/trace').read_text().splitlines():
                parts = line.split()
                if parts[:2] == ['E', '3'] or parts[:2] == ['E', '15']:
                    parts[-1] = 'site'
                lines.append(parts)
            return lines
        assert normalized('cranelift') == normalized('llvm'), kind
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
