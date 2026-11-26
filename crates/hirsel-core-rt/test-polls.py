#!/usr/bin/env python3
"""Native source polls: both engines, exact record/replay, test-side mutant caps."""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import shutil
import time

ROOT = Path(__file__).resolve().parents[2]
FIXTURES = Path(__file__).resolve().parent / 'fixtures/polls'
# CONC-10 event kinds as the published header numbers them.
KIND = {name: int(n) for name, n in re.findall(
    r'HIRSEL_CORE_EVENT_(\w+)=(\d+)', (ROOT / 'crates/hirsel-core-rt/include/hirsel_core_rt_host.h').read_text())}


def low_load():
    while os.getloadavg()[0] > 32:
        time.sleep(10)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--compiler', required=True, type=Path)
    parser.add_argument('--runtime', required=True, type=Path,
                        help='the //:core-native-runtime-fixture-pic archive')
    parser.add_argument('--before', type=Path)
    parser.add_argument('--out', required=True, type=Path)
    parser.add_argument('--only', nargs='+', default=['full', 'check', 'arc', 'capacity', 'latest', 'latest-order', 'steps'])
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    wrapper = args.out / 'cc'
    # This is a test-only link observer; neither emitted code nor runtime has a
    # timeout, mutation flag, or file recorder. Compiler objects remain unchanged.
    # It runs clang (the pinned one first on PATH): the driver replays only a
    # clang `-cc1` compile of the recorder it adds to the link.
    wrapper.write_text('#!/usr/bin/env python3\nimport os,sys\na=sys.argv[1:]\n'
                       'if "--version" not in a:\n a += ' + repr([
                           str(FIXTURES / 'recorder.c'), '-I', str(ROOT / 'crates/hirsel-core-rt/include')]) + '\nos.execvp("clang",["clang",*a])\n')
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
            command = [str(args.compiler.resolve()), 'build', '--source-core', '--inputs', str(inputs), '--entry', 'app@1/main/main',
                       '--backend', engine, '--runtime', str(args.runtime.resolve()), '--grant', 'Tasks', '--store',
                       str(folder / 'store'), '--cc', str(wrapper), '--json', '-o', str(binary)]
            if engine == 'llvm':
                # Keep the former 64 MiB default across the poll fixtures.
                command.extend(['--release', '--limit', str(64 << 20)])
            low_load()
            build = subprocess.run(command, capture_output=True, cwd=ROOT)
            (folder / 'build.json').write_bytes(build.stdout)
            (folder / 'build.stderr').write_bytes(build.stderr)
            if build.returncode:
                raise RuntimeError(f'{kind}/{engine}: build {build.returncode}: {build.stdout[-3000:]!r} {build.stderr!r}')
            trace = folder / 'trace'
            env = dict(os.environ, HIRSEL_POLL_TRACE=str(trace))
            if kind != 'steps':
                env['HIRSEL_POLL_LAST'] = '1'
            record = subprocess.run([str(binary)], capture_output=True, env=env, timeout=5)
            replay = subprocess.run([str(binary)], capture_output=True, env=dict(env, HIRSEL_POLL_REPLAY='1'), timeout=5)
            plain = subprocess.run([str(binary)], capture_output=True, timeout=5)
            assert (record.returncode, record.stdout, record.stderr) == (0, b'', b''), (kind, engine, record)
            assert (replay.returncode, replay.stdout, replay.stderr) == (0, b'', b'')
            assert (plain.returncode, plain.stdout, plain.stderr) == (0, b'', b'')
            events = [line.split() for line in trace.read_text().splitlines() if line.startswith('E ')]
            cancels = [e for e in events if e[1] == '3']
            assert bool(cancels) == (kind in ('full', 'check')), (kind, cancels)
            row = dict(kind=kind, engine=engine, recorded=True, replayed=True, unrecorded=True,
                       events=len(events), cancelled=cancels)
            if kind == 'steps':
                def step_counts(trace_events):
                    children = [e[3] for e in trace_events if int(e[1]) == KIND['SPAWN']]
                    assert len(children) == 1, (engine, children)
                    tasks = ['0', children[0]]
                    slices = [e for e in trace_events if int(e[1]) == KIND['STEP_SLICE']]
                    assert all(e[3:] == ['0', '65536'] for e in slices), slices
                    assert [sum(e[2] == t for e in slices) for t in tasks] == [2, 1], (engine, slices)
                    sends = [sum(int(e[1]) == KIND['TRY_SEND'] and e[2] == t
                                 for e in trace_events) for t in tasks]
                    assert sends == [43690, 43690], (engine, sends)

                step_counts(events)
                last = folder / 'trace-last'
                last_env = dict(os.environ, HIRSEL_POLL_TRACE=str(last), HIRSEL_POLL_LAST='1')
                record = subprocess.run([str(binary)], capture_output=True, env=last_env, timeout=5)
                replay = subprocess.run([str(binary)], capture_output=True,
                                        env=dict(last_env, HIRSEL_POLL_REPLAY='1'), timeout=5)
                assert (record.returncode, record.stdout, record.stderr) == (0, b'', b'')
                assert (replay.returncode, replay.stdout, replay.stderr) == (0, b'', b'')
                assert last.read_text() != trace.read_text(), 'the two schedules coincide'
                step_counts([line.split() for line in last.read_text().splitlines() if line.startswith('E ')])
                row['schedules'] = 2
                row['step_slices'] = 3
                row['step_sends'] = 87380
            if kind == 'latest':
                # CONC-13: publish and read are ordered events that record no
                # value, and replay holds under a second schedule.
                latest = [e for e in events if int(e[1]) in (KIND['LATEST_PUBLISH'], KIND['LATEST_READ'])]
                assert latest and all(e[-1] == '0' for e in latest), latest
                first = folder / 'trace-first'
                env = dict(os.environ, HIRSEL_POLL_TRACE=str(first))
                record = subprocess.run([str(binary)], capture_output=True, env=env, timeout=5)
                replay = subprocess.run([str(binary)], capture_output=True, env=dict(env, HIRSEL_POLL_REPLAY='1'), timeout=5)
                assert (record.returncode, replay.returncode) == (0, 0), (kind, engine, record, replay)
                assert first.read_text() != trace.read_text(), 'the two schedules coincide'
                row['schedules'] = 2
            if kind == 'latest-order':
                # CONC-13 by hand, kinds as the published ABI names them
                # (hirsel_core_rt_host.h, hirsel_core_rt_tasks.h): every read and
                # publish is its own kind; the read's share and the publish's
                # release of the replaced Arc come right after it; new, handle
                # and each handle's cleanup move the handle count; the last
                # handle's cleanup releases the root's last share. Values: Arc
                # share = count after, release = 1 if last, handle count after.
                SENDERS, ARC_NEW, ARC_SHARE, ARC_RELEASE, PUBLISH, READ, END = (KIND[k] for k in (
                    'SENDERS', 'ARC_NEW', 'ARC_SHARE', 'ARC_RELEASE', 'LATEST_PUBLISH', 'LATEST_READ', 'END'))
                arc, root = '0', events[1][3]
                expected = [(ARC_NEW, arc, 1), (SENDERS, root, 1),                   # arc.new(1), latest.new
                            (READ, root, 0), (ARC_SHARE, arc, 2),                    # r1 = read
                            (ARC_NEW, arc, 1), (PUBLISH, root, 0), (ARC_RELEASE, arc, 0),  # publish(arc.new(2)); r1 keeps 1
                            (ARC_RELEASE, arc, 1),                                   # r1 dropped: last share of 1
                            (READ, root, 0), (ARC_SHARE, arc, 2), (ARC_RELEASE, arc, 0),  # r2 = read, dropped
                            (SENDERS, root, 2),                                      # k = handle
                            (READ, root, 0), (ARC_SHARE, arc, 2), (ARC_RELEASE, arc, 0),  # read(k), dropped
                            (SENDERS, root, 1), (SENDERS, root, 0), (ARC_RELEASE, arc, 1),  # k, h cleaned: root's share
                            (END, '0', 0)]
                got = [(int(e[1]), e[3], int(e[4])) for e in events]
                assert root != arc and all(e[2] == '0' for e in events), events
                assert got == expected, (engine, got)
            # A separately built runtime mutant omits the central poll switch.
            # Its source is never shipped; only the native test applies a timeout.
            if args.before:
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
            shutil.rmtree(folder / 'store')
            binary.unlink()
            (folder / 'mutant').unlink(missing_ok=True)
    # Source sites differ between engines' local numbering; the decision/event
    # order and all semantic values besides the engine-local site are identical.
    for kind in args.only:
        def normalized(engine):
            lines = []
            for line in (args.out / f'{kind}-{engine}/trace').read_text().splitlines():
                parts = line.split()
                if parts[0] == 'E' and int(parts[1]) in (KIND['CANCELLED'], KIND['CHECK_CANCEL']):
                    parts[-1] = 'site'
                lines.append(parts)
            return lines
        assert normalized('cranelift') == normalized('llvm'), kind
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
