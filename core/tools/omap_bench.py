#!/usr/bin/env python3
"""Measure operation phases of std.omap against BTreeMap and im 15.1.0.

--prepare compiles only. --measure executes one CPU-16 sample/workload/profile
under the shared measurement locks, perf instructions:u and a 2 GiB cap.
Rust dependencies are unmodified registry sources, outside the shipped toolchain;
--registry may name a vendored copy preserving their upstream license files.
No Cargo environment or job-budget overrides are needed.
"""
import argparse
import fcntl
import json
import os
from pathlib import Path
import re
import select
import sys
import subprocess
import time

from std_native import ROOT, library_sources, wait_load

DEPENDENCIES = [
    ('typenum', '1.20.1', [], []),
    ('bitmaps', '2.1.0', ['typenum'], ['std']),
    ('sized-chunks', '0.6.5', ['typenum', 'bitmaps'], ['std']),
    ('rand_core', '0.6.4', [], []),
    ('rand_xoshiro', '0.6.0', ['rand_core'], []),
    ('im', '15.1.0', ['typenum', 'bitmaps', 'sized_chunks', 'rand_core', 'rand_xoshiro'], []),
]
EXPECTED = {'overwrite': 5000050000, 'insert': 100000, 'lookup': 5000050000, 'range': 5000000000, 'snapshots': 500500, 'bulk': 100000, 'unwrap': 100000, 'unique': 100000}


def inputs(source):
    core, std, app = [dict(name=n, version='1') for n in ('core', 'std', 'omap_bench')]
    def library(package, signatures):
        return dict(package=package, dependencies=[] if package == core else [core],
                    sources=library_sources(ROOT / 'core' / package['name'], signatures))
    return dict(core=core, packages=[library(core, True), library(std, False),
                dict(package=app, dependencies=[core, std], sources=[dict(file=str(source), modules=['main'], signatures=False)])])


def command(argv, log, env):
    wait_load()
    with log.open('w') as output:
        result = subprocess.run([str(v) for v in argv], env=env, cwd=ROOT,
                                stdout=output, stderr=subprocess.STDOUT)
    if result.returncode:
        raise RuntimeError(f'{log}: exit {result.returncode}\n{log.read_text()[-5000:]}')


def rust_build(registry, directory, profile, env, reuse=None):
    directory.mkdir(exist_ok=True)
    flags = ['-Copt-level=' + ('3' if profile == 'release' else '0'), '-Coverflow-checks=yes', '-Ccodegen-units=1']
    for name, version, deps, features in DEPENDENCIES:
        crate = name.replace('-', '_')
        target = directory / ('lib' + crate + '.rlib')
        if reuse:
            target.symlink_to(reuse / target.name)
            continue
        source = registry / (name + '-' + version) / 'src/lib.rs'
        if not source.is_file():
            raise ValueError(f'missing {source}; supply --registry with the exact pinned crates')
        argv = ['rustc', '--edition=2018', '--crate-name', crate, '--crate-type=rlib', source,
                *flags, '-Ldependency=' + str(directory), '-o', target, '--cap-lints=allow']
        for feature in features:
            argv += ['--cfg', 'feature="' + feature + '"']
        if name == 'im':
            # This is precisely im's stable build.rs result: atomic Arc owners.
            argv += ['--cfg', 'threadsafe']
        for dep in deps:
            argv += ['--extern', dep + '=' + str(directory / ('lib' + dep + '.rlib'))]
        command([ROOT / 'tools/capped', *argv], directory / (crate + '-build.log'), env)
    command([ROOT / 'tools/capped', 'rustc', '--edition=2024', ROOT / 'core/twins/omap.rs',
             '--extern', 'im=' + str(directory / 'libim.rlib'), '-Ldependency=' + str(directory),
             *flags, '-o', directory / 'omap'], directory / 'twin-build.log', env)


def refusal(compiler, directory, env):
    manifest = directory / 'refused-inputs.json'
    manifest.write_text(json.dumps(inputs(ROOT / 'core/programs/omap/resource_refused.hirsel')))
    result = subprocess.run([str(compiler), 'check', '--inputs', str(manifest), '--json',
                             '--store', str(directory / 'store')], env=env, capture_output=True, text=True)
    (directory / 'resource-refusal.json').write_text(result.stdout)
    (directory / 'resource-refusal.stderr').write_text(result.stderr)
    product = json.loads(result.stdout)
    rows = product.get('diagnostics', [])
    if result.returncode != 1 or not any(r.get('rule') == 'LANG-12' and 'shareable' in json.dumps(r) for r in rows):
        raise RuntimeError('resource bound did not refuse with LANG-12: ' + result.stdout[-4000:])
    print('omap resource bound: 1 refused as LANG-12', flush=True)


def counter_command(controls, action):
    control, ack = controls
    os.write(control, (action + '\n').encode())
    if not select.select([ack], [], [], 30)[0] or not os.read(ack, 4096).startswith(b'ack'):
        raise RuntimeError('perf control did not acknowledge ' + action)


def resident(pid):
    status = Path('/proc', str(pid), 'status').read_text()
    return {name: int(re.search('^' + name + r':\s+(\d+)', status, re.M)[1])
            for name in ['VmRSS', 'VmHWM']}


def measure(argv, directory, env, tag, expected, recording=False, host_directory=None,
            events="instructions:u", before_phase=None, unshielded=False):
    """Counters start after population; HWM resets to resident size at READY.

    The target reports its own monotonic operation duration. The post-DONE gate
    pins roots until both counters and VmHWM have been read, excluding final drops.
    With events=None, run only the gate protocol for an instrumented target.
    """
    work = directory / tag
    work.mkdir()
    host = host_directory or directory / 'host'
    host.mkdir(exist_ok=True)
    use_perf = recording or events is not None
    paths = [host / 'gate']
    if use_perf:
        paths.extend([work / 'control', work / 'ack'])
    for path in paths:
        path.unlink(missing_ok=True)
        os.mkfifo(path)
    gate = os.open(host / 'gate', os.O_RDWR)
    controls = [os.open(path, os.O_RDWR | os.O_NONBLOCK) for path in paths[1:]]
    perf = work / ('perf.data' if recording else 'perf.tsv')
    wait_load()
    with (work / 'stderr').open('w') as err:
        command_line = [ROOT / 'tools/capped']
        if not unshielded:
            command_line += ['taskset', '-c', '16']
        if use_perf:
            mode = (['record', '-e', 'instructions:u', '-c', '100000' if 'insert' in tag else '10000',
                     '--call-graph', 'fp'] if recording else ['stat', '-x', '\t', '-e', events])
            command_line += ['perf', *mode, '-D', '-1',
                             '--control=fifo:' + str(work / 'control') + ',' + str(work / 'ack'),
                             '-o', perf, '--']
        command_line += [sys.executable, Path(__file__).resolve(), '--exec-child', work / 'pid', *argv]
        process = subprocess.Popen(list(map(str, command_line)), env=env, stdout=subprocess.PIPE,
                                   stderr=err, text=True, bufsize=1)
        ready = process.stdout.readline()
        if ready != 'READY\n':
            raise RuntimeError('target did not become ready: ' + ready + (work / 'stderr').read_text())
        pid = int((work / 'pid').read_text())
        start_resident = None
        if before_phase is not None:
            before_phase()
        if use_perf:
            # Setup may finish at the end of capped's 100 ms CPU quota period.
            # Both periods pass while the target is gated and counters are off.
            time.sleep(0.2)
            start_resident = resident(pid)
            Path('/proc', str(pid), 'clear_refs').write_text('5\n')
            counter_command(controls, 'enable')
        os.write(gate, b's')
        done = process.stdout.readline()
        match = re.fullmatch(r'DONE (\d+) (\d+)\n', done)
        peak = None
        if match:
            if use_perf:
                counter_command(controls, 'disable')
                peak = resident(pid)
            os.write(gate, b'f')
        remainder = process.stdout.read()
        code = process.wait()
    os.close(gate)
    for fd in controls:
        os.close(fd)
    (work / 'stdout').write_text(ready + done + remainder)
    raw = perf.read_text() if not recording and perf.exists() else ''
    counts = {}
    active = {}
    for line in raw.splitlines():
        cols = line.split('\t')
        if len(cols) > 2 and cols[0].replace(',', '').isdigit():
            counts[cols[2]] = int(cols[0].replace(',', ''))
            if len(cols) > 4 and cols[4]:
                active[cols[2]] = float(cols[4])
    instructions = next((int(line.split('\t')[0].replace(',', '')) for line in raw.splitlines()
                         if '\tinstructions:u\t' in line and line.split('\t')[0].replace(',', '').isdigit()), None)
    ok = (code == 0 and match is not None and (expected is None or int(match[1]) == expected)
          and (not use_perf or recording or instructions is not None))
    row = dict(tag=tag, exit=code, complete=ok, instructions=instructions, counts=counts,
               instruction_active_percent=active.get('instructions:u'),
               wall_ns=int(match[2]) if match and use_perf else None,
               resident_before_kib=start_resident['VmRSS'] if start_resident else None,
               peak_rss_kib=peak['VmHWM'] if peak else None,
               censored=None if ok else (work / 'stderr').read_text()[-1000:])
    if recording and ok:
        result = subprocess.run(['perf', 'report', '--stdio', '--no-children', '-i', str(perf),
                                 '--sort', 'dso,symbol', '--call-graph', 'none', '--percent-limit', '0.5'], capture_output=True, text=True)
        (work / 'profile.txt').write_text(result.stdout + result.stderr)
    (work / 'row.json').write_text(json.dumps(row, indent=2) + '\n')
    return row


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--compiler', type=Path, required=True)
    parser.add_argument('--runtime', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--registry', type=Path)
    parser.add_argument('--reuse-rust', type=Path)
    parser.add_argument('--binary-dir', type=Path, help='measure existing frozen executables without rebuilding')
    parser.add_argument('--only-implementation', action='append', choices=['hirsel', 'btree', 'im', 'im-unique', 'arc-rewrap', 'arc-unique'])
    parser.add_argument('--samples', type=int, choices=range(1, 6), default=1)
    parser.add_argument('--prepare', action='store_true')
    parser.add_argument('--measure', action='store_true')
    parser.add_argument('--check-refusal', action='store_true')
    parser.add_argument('--profile', action='store_true')
    parser.add_argument('--events', default='instructions:u', help='perf stat event list')
    parser.add_argument('--only-profile', choices=['dev', 'release'])
    parser.add_argument('--only-workload', action='append', choices=list(EXPECTED))
    args = parser.parse_args()
    directory = args.out.resolve()
    directory.mkdir(parents=True, exist_ok=True)
    env = os.environ | dict(HIRSEL_MEMORY_LIMIT='2G', HIRSEL_JOBS='1',
                            LD_LIBRARY_PATH=str(ROOT / 'tools/buck2/toolchains/llvm-files/lib'),
                            XDG_CACHE_HOME=str(directory / 'cache'), XDG_DATA_HOME=str(directory / 'data'))
    compiler, runtime = args.compiler.resolve(), args.runtime.resolve()
    registry = args.registry or next((Path.home() / '.cargo/registry/src').glob('*/im-15.1.0')).parent
    if args.check_refusal:
        refusal(compiler, directory, env)
    if args.prepare:
        (directory / 'host').mkdir(exist_ok=True)
        manifest = directory / 'inputs.json'
        manifest.write_text(json.dumps(inputs(ROOT / 'core/programs/omap/main.hirsel')))
        for profile, backend in [('dev', 'cranelift'), ('release', 'llvm')]:
            if args.only_profile and args.only_profile != profile:
                continue
            argv = [ROOT / 'tools/capped', compiler, 'build', '--inputs', manifest,
                    '--entry', 'omap_bench@1/main/main', '--backend', backend, '--runtime', runtime,
                    '--cc', ROOT / 'tools/buck2/toolchains/llvm-files/bin/hirsel-clang',
                    '--grant', 'Stdout', '--grant', 'Clock', '--dir', directory / 'host',
                    '--debug-lines', 'on', '--store', directory / 'store', '--json', '-o', directory / ('hirsel-' + profile)]
            if profile == 'release':
                argv += ['--release']
            command(argv, directory / ('hirsel-' + profile + '-build.log'), env)
            rust_build(registry, directory / ('rust-' + profile), profile, env,
                       args.reuse_rust.resolve() / ('rust-' + profile) if args.reuse_rust else None)
    if args.measure or args.profile:
        binaries = args.binary_dir.resolve() if args.binary_dir else directory
        rows = []
        with open('/tmp/core-llvm-905-measure.lock', 'a') as core_lock, open('/tmp/hirsel-langarena-measure.lock', 'a') as shared_lock:
            fcntl.flock(core_lock, fcntl.LOCK_EX)
            fcntl.flock(shared_lock, fcntl.LOCK_EX)
            for profile in ([args.only_profile] if args.only_profile else ['dev', 'release']):
                for workload, expected in EXPECTED.items():
                    if args.only_workload and workload not in args.only_workload:
                        continue
                    programs = [('hirsel', [binaries / ('hirsel-' + profile), workload]),
                                ('btree', [binaries / ('rust-' + profile) / 'omap', 'btree', workload, binaries / 'host/gate']),
                                ('im', [binaries / ('rust-' + profile) / 'omap', 'im', workload, binaries / 'host/gate'])]
                    if workload == 'unwrap':
                        programs = [programs[0], *[(name, [binaries / ('rust-' + profile) / 'omap', name, workload, binaries / 'host/gate']) for name in ['arc-rewrap', 'arc-unique']]]
                    if workload == 'insert':
                        programs.append(('im-unique', [binaries / ('rust-' + profile) / 'omap', 'im-unique', workload, binaries / 'host/gate']))
                    for implementation, argv in programs:
                        if args.only_implementation and implementation not in args.only_implementation:
                            continue
                        for sample in range(args.samples):
                            row = measure(argv, directory, env, ('profile-' if args.profile else '') + profile + '-' + workload + '-' + implementation + ('-' + str(sample) if args.samples > 1 else ''), expected, args.profile, binaries / 'host', args.events)
                            rows.append(row)
                            (directory / ('profile-rows.json' if args.profile else 'rows.json')).write_text(json.dumps(rows, indent=2) + '\n')
                            print(json.dumps(row), flush=True)
    return 0


if __name__ == '__main__':
    if sys.argv[1:2] == ['--exec-child']:
        Path(sys.argv[2]).write_text(str(os.getpid()))
        os.execv(sys.argv[3], sys.argv[3:])
    raise SystemExit(main())
