#!/usr/bin/env python3
"""S2 R6-R8 and S3 R4/R5 with private open, frozen sources and gated CPU16 phase counters."""
import argparse
from contextlib import ExitStack, nullcontext
import fcntl
import json
from math import ceil
import os
from pathlib import Path
import re
import shutil
from struct import error as StructError
import subprocess
import time

from measurement_artifacts import allocation_linker, check, phase_allocations
from omap_bench import command, measure
from std_native import ROOT
from store_r6 import Manifest, check_r6
from store_tests import inputs
from timing_shield import BOARD, REGIMES, hold


def r6_result(row, output, before):
    timing = re.search(r'CUT (\d+) REMOVAL (\d+) REMOVE_ATTEMPTS (\d+) DRAIN_GROUP_SYNCS (\d+)\n', output)
    if timing is None:
        raise ValueError('R6 cut timing missing')
    if not isinstance(before, Manifest):
        raise ValueError(f'R6 failed to read the pre-cut store: {before}')
    row.update(pause_ns=int(timing[1]), removal_ns=int(timing[2]),
               removal_attempts=int(timing[3]), drain_group_syncs=int(timing[4]))
    classified = check_r6(before, Manifest(before.directory))
    if any(row.get(key) != classified[key] for key in ('pack_bytes', 'new_pages')):
        raise ValueError('R6 pack totals differ from the independent parse')
    row.update(classified)
    if row['r6_violations']:
        raise ValueError(', '.join(row['r6_violations']))


def burst_sample(binary, host, out, env, width, burst, stores):
    before = set(host.glob('store-law-*'))
    tag = f'{binary.name}-RB-{width}-b{burst}-s{stores}'
    with ExitStack() as stack:
        jobs = []
        gates = []
        for slot in range(stores):
            gate_path = host / f'gate-RB-{slot}'
            gate_path.unlink(missing_ok=True)
            os.mkfifo(gate_path)
            stack.callback(gate_path.unlink)
            gate = os.open(gate_path, os.O_RDWR)
            stack.callback(os.close, gate)
            gates.append(gate)
            stderr = stack.enter_context((out / f'{tag}-{slot}.stderr').open('w'))
            argv = [ROOT / 'tools/capped', 'taskset', '-c', '16', binary, 'RB',
                    str(width), str(burst), gate_path.name]
            jobs.append(subprocess.Popen(list(map(str, argv)), env=env | {'HIRSEL_JOBS': '1'},
                                         stdout=subprocess.PIPE, stderr=stderr, text=True))
        ready = [job.stdout.readline() == 'READY\n' for job in jobs]
        for gate, waiting in zip(gates, ready):
            if waiting:
                os.write(gate, b's')
        output = [job.communicate()[0].split() for job in jobs]
    if not all(ready) or any(job.returncode for job in jobs):
        raise ValueError(f'{tag} did not finish; see stderr logs')
    if any(len(tokens) != 3 + 2000 * burst or tokens[0] != 'BURST' for tokens in output):
        raise ValueError(f'{tag} sample is incomplete')
    latencies = sorted(int(value) for tokens in output for value in tokens[3:])
    engagement = [int(tokens[1]) / 2000 for tokens in output]
    terminal_ends = {int(tokens[2]) for tokens in output}
    directories = set(host.glob('store-law-*')) - before
    if len(directories) != stores or len(terminal_ends) != 1:
        raise ValueError(f'{tag} stores did not finish at the same frame end')
    terminal_end, = terminal_ends
    for directory in directories:
        segments = sorted((p for p in directory.glob('j-*') if p.name[2:].isdecimal()),
                          key=lambda p: int(p.name[2:]))
        ends = []
        for successor in segments[1:]:
            with successor.open('rb') as file:
                file.seek(64)
                ends.append(int.from_bytes(file.read(8), 'little'))
        ends.append(terminal_end)
        if not segments or any(not 0 <= segment.stat().st_size - end <= min(4194304, (end - 72) // 4096 * 4096)
                               for segment, end in zip(segments, ends)):
            raise ValueError(f'{tag} segment exceeds the window bound: {directory}')
        shutil.rmtree(directory)
    return dict(tag=tag, complete=True, workload='RB', width=width, burst=burst, stores=stores,
                samples=len(latencies), engagement=engagement,
                p50_ns=latencies[ceil(0.50 * len(latencies)) - 1],
                p99_ns=latencies[ceil(0.99 * len(latencies)) - 1],
                p999_ns=latencies[ceil(0.999 * len(latencies)) - 1], max_ns=latencies[-1])


BL15_COMMITS = 20000
BL15_CPUS = ('16', '17')  # writer, follower
# io_uring_enter is recorded beside the handoffs so a row shows whether it ran on a ring host: a tied
# process (one whose binary spawns no unit) runs every file op inline and records neither event.
HANDOFF = ['-e', 'sched:sched_switch', '--filter', 'prev_comm == "hirsel-host"',
           '-e', 'syscalls:sys_enter_io_uring_enter', '-k', 'CLOCK_MONOTONIC']


def handoffs(perf_data, windows):
    """Per window: helper-thread switch-outs (one per off_slot call that ends with the helper idle), and ring enters."""
    script = subprocess.run(['perf', 'script', '-i', str(perf_data), '-F', 'time,event', '--ns'],
                            check=True, capture_output=True, text=True).stdout
    events = [line.split() for line in script.splitlines() if line.strip()]
    events = [(round(float(e[0].rstrip(':')) * 1e9), e[1].rstrip(':')) for e in events]
    count = lambda kind, lo, hi: sum(lo <= t < hi and name == kind for t, name in events)
    return ({name: count('sched:sched_switch', lo, hi) for name, (lo, hi) in windows.items()},
            {name: count('syscalls:sys_enter_io_uring_enter', lo, hi) for name, (lo, hi) in windows.items()})


def bl15_sample(binary, host, out, env, follower):
    """BL15: Store sync p50/p99 over R8-shaped durable commits, and off_slot handoffs per phase."""
    tag = f'{binary.name}-BL15-f{int(follower)}'
    work = out / tag
    work.mkdir()
    gate_path = host / 'gate'
    gate_path.unlink(missing_ok=True)
    os.mkfifo(gate_path)
    gate = os.open(gate_path, os.O_RDWR)
    marks = {}

    def launch(cpu, name, *argv):
        command_line = [ROOT / 'tools/capped', 'taskset', '-c', cpu, 'perf', 'record', '-q', '-N', *HANDOFF,
                        '-o', work / f'{name}.perf', '--', binary, *argv]
        return subprocess.Popen(list(map(str, command_line)), env=env | {'HIRSEL_JOBS': '1'}, stdout=subprocess.PIPE,
                                stderr=(work / f'{name}.stderr').open('w'), text=True, bufsize=1)

    def step(process, word, mark=None):
        line = process.stdout.readline().split()
        if not line or line[0] != word:
            raise ValueError(f'{tag}: expected {word}, got {line}; see {work}')
        time.sleep(0.05)  # a stdout write's helper finishes before the phase mark
        if mark:
            marks[mark] = time.monotonic_ns()
        return line[1]

    writer = launch(BL15_CPUS[0], 'writer', 'BL15', BL15_COMMITS)
    reader = None
    try:
        name = step(writer, 'DIR', 'open')
        os.write(gate, b's')
        target = step(writer, 'OPENED', 'opened')
        if follower:
            reader = launch(BL15_CPUS[1], 'follower', 'BL15F', name, target)
            step(reader, 'FOLLOWING')
        marks['commit'] = time.monotonic_ns()
        os.write(gate, b's')
        step(writer, 'COMMITTED', 'committed')
        os.write(gate, b's')
        step(writer, 'READ', 'read')
        os.write(gate, b's')
        lat = writer.stdout.readline().split()
        polls = reader.stdout.readline().split() if reader else None
        writer.communicate()
        if reader:
            reader.communicate()
    finally:
        os.close(gate)
        gate_path.unlink()
    if writer.returncode or (reader and reader.returncode) or lat[:1] != ['LAT'] or len(lat) != BL15_COMMITS + 1:
        raise ValueError(f'{tag} did not finish; see {work}')
    latencies = sorted(int(v) for v in lat[1:])
    windows = {'open': (marks['open'], marks['opened']), 'commit': (marks['commit'], marks['committed']),
               'read': (marks['committed'], marks['read'])}
    row = dict(tag=tag, complete=True, workload='BL15', follower=follower, commits=BL15_COMMITS,
               sync_p50_ns=latencies[len(latencies) // 2], sync_p99_ns=latencies[ceil(len(latencies) * 0.99) - 1],
               **dict(zip(('handoffs', 'ring_enters'), handoffs(work / 'writer.perf', windows))))
    if reader:
        row.update(follower_polls=int(polls[1]),
                   **dict(zip(('follower_handoffs', 'follower_ring_enters'), handoffs(work / 'follower.perf', windows))))
    return row


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--artifacts', type=Path, required=True)
    p.add_argument('--out', type=Path, required=True)
    p.add_argument('--prepare', action='store_true')
    p.add_argument('--measure', action='store_true')
    p.add_argument('--allocations', action='store_true',
                   help='prepare or sample the allocation-counting binary instead of the shipping binary')
    p.add_argument('--profile', action='append', choices=['dev', 'release'])
    p.add_argument('--workload', action='append', choices=['R4', 'R5', 'R6', 'R7', 'R8-load', 'R8-open', 'migrate', 'RB', 'BL15'])
    p.add_argument('--index', action='append', type=int, choices=[0, 1])
    p.add_argument('--timing-hold', action='store_true', help='shield CPU 16 for each measured sample')
    args = p.parse_args()
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=True)
    host = out / 'host'
    host.mkdir(exist_ok=True)
    identity = json.loads(args.artifacts.read_text())
    check(identity['compiler'], identity)
    check(identity['runtime'], identity)
    cc = ROOT / 'tools/buck2/toolchains/llvm-files/bin/hirsel-clang'
    runtime = identity['runtime']
    suffix = ''
    if args.allocations:
        if 'allocation_runtime' not in identity:
            p.error('--allocations needs artifacts built with --allocation-counter')
        runtime = identity['allocation_runtime']
        check(runtime, identity)
        suffix = '-allocations'
    env = os.environ | {'HIRSEL_MEMORY_LIMIT': '2G',
                        'LD_LIBRARY_PATH': str(ROOT / 'tools/buck2/toolchains/llvm-files/lib')}
    profiles = args.profile or ['dev', 'release']
    if args.prepare:
        build_env = env | {'HIRSEL_MEMORY_LIMIT': os.environ.get('HIRSEL_MEMORY_LIMIT', '2G')}
        if args.allocations:
            cc = allocation_linker(cc, out)
        source = ((ROOT / 'core/programs/store/store.hirsel').read_text() + '\n'
                  + (ROOT / 'core/programs/store/disk_bench.hirsel').read_text())
        frozen = out / 'store.hirsel'
        frozen.write_text(source)
        corpus = inputs(ROOT, tests=True)
        corpus['packages'][-1]['sources'][0]['file'] = str(frozen)
        manifest = out / 'inputs.json'
        manifest.write_text(json.dumps(corpus))
        for profile in profiles:
            binary = profile + suffix
            command_line = [ROOT / 'tools/capped', identity['compiler'], 'build', '--inputs', manifest,
                            '--source-core', '--source-std', '--entry', 'store@1/store/disk_bench_main',
                            '--backend', 'llvm' if profile == 'release' else 'cranelift',
                            '--runtime', runtime, '--dir', host,
                            '--limit', '1073741824', '--store', out / 'objects',
                            '--cc', cc, '-o', out / binary, '--json']
            for grant in ['Clock', 'Random', 'Tasks', 'Stdout']:
                command_line += ['--grant', grant]
            if profile == 'release':
                command_line.append('--release')
            command(command_line, out / (binary + '-build.json'), build_env)
            print('prepared', binary, flush=True)
        (out / f'environment{suffix}.json').write_text(json.dumps(dict(
            artifacts=identity, root_limit=1073741824,
            population='R4/R5 16 keys after 64 durable commits, 100k R6/R7/migrate, 1M R8; Account codec30 with schema2, tree key12',
            indexes='none or one J=id%4', sample='one per configuration',
            cpu=16, protocol='READY/DONE; R8-load counts creation, population and the first checkpoint; other modes exclude population; validation and final drops excluded'), indent=2) + '\n')
    if args.measure:
        # One pool worker: the bench body's spawned unit runs on the pool, so its file ops take the
        # slot ring (STORE-SPEC §3.3b) instead of running inline on a tied thread.
        env |= {'HIRSEL_UNTIED': '1'}
        rows = []
        with open('/tmp/core-llvm-905-measure.lock', 'a') as core_lock, open('/tmp/hirsel-langarena-measure.lock', 'a') as shared_lock:
            fcntl.flock(core_lock, fcntl.LOCK_EX)
            fcntl.flock(shared_lock, fcntl.LOCK_EX)
            for profile in profiles:
                for mode in args.workload or ['R4', 'R5', 'R6', 'R7', 'R8-load', 'R8-open']:
                    if mode == 'BL15':
                        for follower in (False, True):
                            with hold(REGIMES, BOARD) if args.timing_hold else nullcontext():
                                row = bl15_sample(out / (profile + suffix), host, out, env, follower)
                            row.update(profile=profile, artifacts=identity)
                            rows.append(row)
                            (out / (row['tag'] + '-sample.json')).write_text(json.dumps(row, indent=2) + '\n')
                            print(row['tag'], json.dumps(row), flush=True)
                        continue
                    if mode == 'RB':
                        for width in (97, 4096):
                            for burst in (1, 8, 64):
                                for stores in (1, 4):
                                    with hold(REGIMES, BOARD):
                                        row = burst_sample(out / (profile + suffix), host, out, env, width, burst, stores)
                                    rows.append(row)
                                    (out / (row['tag'] + '-sample.json')).write_text(json.dumps(row, indent=2) + '\n')
                                    print(row['tag'], json.dumps(row), flush=True)
                        continue
                    indexes = args.index or ([0, 1] if mode.startswith('R8') else [0])
                    for index in indexes:
                        # R4's DONE is the sum of c over 64..10064; R5's is its commit count.
                        n = {'R4': 50635000, 'R5': None}.get(mode, 1000000 if mode.startswith('R8') else 100000)
                        tag = f'{profile}-{mode}-i{index}{suffix}'
                        before = None
                        existing = set(host.glob('store-law-*'))

                        def before_phase():
                            nonlocal before
                            try:
                                directory, = set(host.glob('store-law-*')) - existing
                                before = Manifest(directory)
                            except (ValueError, OSError, KeyError, StructError) as error:
                                before = error

                        with hold(REGIMES, BOARD) if args.timing_hold else nullcontext():
                            row = measure([out / (profile + suffix), mode, str(index)], out, env, tag, n,
                                          host_directory=host, events=None if args.allocations else 'instructions:u',
                                          before_phase=before_phase if mode == 'R6' else None)
                        text = (out / tag / 'stdout').read_text()
                        extra = re.search(r'PACK_BYTES (\d+) NEW_PAGES (\d+) ACCOUNT (\d+)\n', text)
                        journal = re.search(r'COMMITS (\d+) VERSION (\d+)\n', text)
                        row.update(profile=profile, workload=mode, indexes=index, artifacts=identity)
                        if mode in ('R4', 'R5') and journal:
                            row.update(commits=int(journal[1]), version=int(journal[2]))
                        elif extra:
                            row.update(pack_bytes=int(extra[1]), new_pages=int(extra[2]), account=int(extra[3]))
                        else:
                            row['complete'] = False
                            row['censored'] = row['censored'] or 'post-phase store validation did not finish'
                        if mode == 'R6':
                            try:
                                r6_result(row, text, before)
                            except (ValueError, OSError, KeyError, StructError) as error:
                                row['complete'] = False
                                row['censored'] = row['censored'] or str(error)
                        if args.allocations:
                            try:
                                row['allocations'] = phase_allocations((out / tag / 'stderr').read_text())
                            except ValueError as error:
                                row['allocations'] = None
                                row['complete'] = False
                                row['censored'] = row['censored'] or str(error)
                        rows.append(row)
                        (out / (tag + '-sample.json')).write_text(json.dumps(row, indent=2) + '\n')
                        print(tag, json.dumps(row), flush=True)
        (out / f'samples{suffix}.json').write_text(json.dumps(rows, indent=2) + '\n')
        return int(not all(row['complete'] for row in rows))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
