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
from struct import Struct, error as StructError
import subprocess
import sys
import time

from measurement_artifacts import allocation_linker, check, phase_allocations
from omap_bench import command, measure
from std_native import ROOT
from store_r6 import Manifest, check_r6
from store_tests import inputs
from timing_shield import BOARD, REGIMES, hold

PROFILES = ('dev', 'release')
PHASES = ('R4', 'R5', 'R6', 'R7', 'R8-load', 'R8-open')


def phase_indexes(mode):
    return (0, 1) if mode.startswith('R8') else (0,)


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


BURST_TRIALS = 2000
BURST_KEYS = 16
BURST_WARMUP = (8 * 1024 * 1024) // 4096
JOURNAL_HEADER_BYTES = 72
FRAME = Struct('<HHI8x')
# STORE-SPEC §2.4, a single data-section put with a recorded prior value width.
PUT = Struct('<IQQBBIBI')
ACCOUNT_KEY = Struct('>IQ')
ACCOUNT_VALUE = Struct('<HQqI')


def check_burst_row(key_bytes, value, update, width, where):
    key = update % BURST_KEYS
    expected_key = ACCOUNT_KEY.pack(1, key)
    if key_bytes != expected_key:
        raise ValueError(f'{where} key {key} expected {expected_key.hex()}, actual {key_bytes.hex()}')
    owner_size = width - 89
    expected_fields = (0, key, update, owner_size)
    if len(value) < ACCOUNT_VALUE.size:
        raise ValueError(f'{where} key {key} expected {ACCOUNT_VALUE.size + owner_size} value bytes, '
                         f'actual {len(value)}')
    actual_fields = ACCOUNT_VALUE.unpack_from(value)
    if actual_fields != expected_fields:
        raise ValueError(f'{where} key {key} (schema, id, balance, owner length) '
                         f'expected {expected_fields}, actual {actual_fields}')
    expected_owner = b'x' * owner_size
    owner = value[ACCOUNT_VALUE.size:]
    if owner != expected_owner:
        offset = next((i for i, (expected, actual) in enumerate(zip(expected_owner, owner))
                       if expected != actual), min(len(expected_owner), len(owner)))
        raise ValueError(f'{where} key {key} owner byte {offset} '
                         f'expected {expected_owner[offset:offset + 1]!r} ({owner_size} bytes), '
                         f'actual {owner[offset:offset + 1]!r} ({len(owner)} bytes)')


def check_burst_journal(segment, terminal_end, width, commits):
    with segment.open('rb') as journal:
        header = journal.read(JOURNAL_HEADER_BYTES)
        if (len(header) != JOURNAL_HEADER_BYTES or FRAME.unpack_from(header) != (1, 1, 56)
                or header[16:24] != b'HIRSTORE'):
            raise ValueError(f'{segment.name} has an invalid journal header')
        for update in range(commits):
            where = f'journal update {update} at {segment.name}:{journal.tell()}'
            frame_width = 4096 if update < BURST_WARMUP else width
            frame = journal.read(frame_width)
            if len(frame) != frame_width or FRAME.unpack_from(frame) != (0x0011, 1, frame_width - FRAME.size):
                raise ValueError(f'{where} expected a {frame_width}-byte COMMIT frame')
            branch, version, _, sections, section, count, op, key_size = PUT.unpack_from(frame, FRAME.size)
            if (branch, version, sections, section, count, op, key_size) != (0, update + 2, 1, 0, 1, 1, ACCOUNT_KEY.size):
                raise ValueError(f'{where} expected one main-branch put at version {update + 2}')
            key_at = FRAME.size + PUT.size
            value_at = key_at + key_size + 8  # prior width and value length, both u32 LE
            if int.from_bytes(frame[value_at - 4:value_at], 'little') != len(frame) - value_at:
                raise ValueError(f'{where} has an invalid value length')
            check_burst_row(frame[key_at:key_at + key_size], frame[value_at:], update, frame_width, where)
        if journal.tell() != terminal_end:
            raise ValueError(f'{segment.name} expected final frame end {journal.tell()}, actual {terminal_end}')


def check_burst_reopen(directory, lines, width, commits):
    for key, line in enumerate(lines):
        label, raw_key, raw_value = line.split()
        if label != 'ROW':
            raise ValueError(f'{directory}: expected reopened key {key}, got {label}')
        if key >= BURST_KEYS:
            raise ValueError(f'{directory}: expected end of reopened table, actual extra key {raw_key}')
        update = commits - 1 - (commits - 1 - key) % BURST_KEYS
        check_burst_row(bytes.fromhex(raw_key), bytes.fromhex(raw_value), update, width, f'{directory}: reopened row')
    if len(lines) != BURST_KEYS:
        raise ValueError(f'{directory}: expected reopened key {len(lines)}, actual end of table')


def run_burst(binary, host, out, env, width, burst, stores, tag, check):
    with ExitStack() as stack:
        jobs = []
        gates = []
        with hold(REGIMES, BOARD) if check is None else nullcontext():
            for slot in range(stores):
                gate_path = host / f'gate-RB-{slot}'
                gate_path.unlink(missing_ok=True)
                os.mkfifo(gate_path)
                stack.callback(gate_path.unlink)
                gate = os.open(gate_path, os.O_RDWR)
                stack.callback(os.close, gate)
                gates.append(gate)
                stderr = stack.enter_context((out / f'{tag}-{slot}.stderr').open('w'))
                affinity = ['taskset', '-c', '16'] if check is None else []
                mode = 'RB-SUBSTITUTION' if check == 'substitution' else 'RB'
                argv = [ROOT / 'tools/capped', *affinity, binary, mode, width, burst, gate_path.name]
                jobs.append(subprocess.Popen(list(map(str, argv)), env=env | {'HIRSEL_JOBS': '1'},
                                             stdout=subprocess.PIPE, stderr=stderr, text=True))
            ready = [job.stdout.readline().split() for job in jobs]
            for gate in gates:
                os.write(gate, b's')
            output = [job.stdout.readline().split() for job in jobs]
        for gate in gates:
            os.write(gate, b'v')
        reopened = [job.communicate()[0].splitlines() for job in jobs]
    if any(job.returncode for job in jobs):
        raise ValueError(f'benchmark or reopen failed; see {out / tag}-*.stderr')
    return ready, output, reopened


def burst_sample(binary, host, out, env, width, burst, stores, *, check=None):
    before = set(host.glob('store-law-*'))
    tag = f'{binary.name}-RB-{width}-b{burst}-s{stores}' + (f'-{check}' if check else '')
    samples = BURST_TRIALS * burst
    commits = BURST_WARMUP + samples
    try:
        ready, output, reopened = run_burst(binary, host, out, env, width, burst, stores, tag, check)
        directories = set(host.glob('store-law-*')) - before
        if (len(directories) != stores or any(len(tokens) != 3 or tokens[0] != 'READY' for tokens in ready)
                or {host / tokens[1] for tokens in ready} != directories):
            raise ValueError('incomplete READY records or unexpected Store directories')
        if any(len(tokens) != 3 + samples or tokens[0] != 'BURST' for tokens in output):
            raise ValueError('incomplete BURST sample')
        verify_burst_stores(host, ready, output, reopened, width, commits)
        latencies = sorted(int(value) for tokens in output for value in tokens[3:])
        engagement = [int(tokens[1]) / BURST_TRIALS for tokens in output]
    except (OSError, ValueError, StructError) as error:
        retained = sorted(set(host.glob('store-law-*')) - before)
        paths = ', '.join(map(str, retained)) or '(none created)'
        raise ValueError(f'{tag}: {error}; Store directories retained: {paths}') from error
    for directory in directories:
        shutil.rmtree(directory)
    return dict(tag=tag, complete=True, workload='RB', width=width, burst=burst, stores=stores,
                samples=len(latencies), engagement=engagement,
                verified_updates=commits * stores, verified_reopened_rows=BURST_KEYS * stores,
                p50_ns=latencies[ceil(0.50 * len(latencies)) - 1],
                p99_ns=latencies[ceil(0.99 * len(latencies)) - 1],
                p999_ns=latencies[ceil(0.999 * len(latencies)) - 1], max_ns=latencies[-1])


def verify_burst_stores(host, ready, output, reopened, width, commits):
    terminal_ends = {int(tokens[2]) for tokens in output}
    if len(terminal_ends) != 1:
        raise ValueError('stores did not finish at the same frame end')
    terminal_end, = terminal_ends
    for (_, name, journal), lines in zip(ready, reopened):
        directory = host / name
        check_burst_reopen(directory, lines, width, commits)
        segments = sorted((p for p in directory.glob('j-*') if p.name[2:].isdecimal()),
                          key=lambda p: int(p.name[2:]))
        if not segments or segments[-1].name != journal:
            raise ValueError(f'{directory}: expected active journal {journal}')
        ends = []
        for successor in segments[1:]:
            with successor.open('rb') as file:
                file.seek(64)
                ends.append(int.from_bytes(file.read(8), 'little'))
        ends.append(terminal_end)
        if any(not 0 <= segment.stat().st_size - end <= min(4194304, (end - 72) // 4096 * 4096)
               for segment, end in zip(segments, ends)):
            raise ValueError(f'{directory}: segment exceeds the window bound')
        try:
            check_burst_journal(segments[-1], terminal_end, width, commits)
        except ValueError as error:
            raise ValueError(f'{directory}: {error} after verifying {BURST_KEYS} reopened rows') from error


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
        # HIRSEL_UDP_MEASURE_CLOCK prints the provider's mono_ns epoch, which turns the phase
        # stamps the program prints into perf's CLOCK_MONOTONIC.
        return subprocess.Popen(list(map(str, command_line)), env=env | {'HIRSEL_JOBS': '1', 'HIRSEL_UDP_MEASURE_CLOCK': '1'},
                                stdout=subprocess.PIPE,
                                stderr=(work / f'{name}.stderr').open('w'), text=True, bufsize=1)

    def step(process, word, phase=None):
        line = process.stdout.readline().split()
        if not line or line[0] != word:
            raise ValueError(f'{tag}: expected {word}, got {line}; see {work}')
        if phase:
            # The program's own mono_ns bounds: after its gate read, before this line's write.
            marks[phase] = (int(line[2]), int(line[3]))
        return line[1]

    def go():
        # The helper that wrote the last line goes idle before the next phase's first stamp.
        time.sleep(0.05)
        os.write(gate, b's')

    writer = launch(BL15_CPUS[0], 'writer', 'BL15', BL15_COMMITS)
    reader = None
    try:
        name = step(writer, 'DIR')
        go()
        target = step(writer, 'OPENED', 'open')
        if follower:
            reader = launch(BL15_CPUS[1], 'follower', 'BL15F', name, target)
            step(reader, 'FOLLOWING')
        go()
        step(writer, 'COMMITTED', 'commit')
        go()
        step(writer, 'READ', 'read')
        go()
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
    def epoch(name):
        at, = {int(m) for m in re.findall(r'^UDP_CLOCK_EPOCH (\d+)$', (work / f'{name}.stderr').read_text(), re.M)}
        return at

    windows = {phase: (epoch('writer') + lo, epoch('writer') + hi) for phase, (lo, hi) in marks.items()}
    row = dict(tag=tag, complete=True, workload='BL15', follower=follower, commits=BL15_COMMITS,
               sync_p50_ns=latencies[len(latencies) // 2], sync_p99_ns=latencies[ceil(len(latencies) * 0.99) - 1],
               **dict(zip(('handoffs', 'ring_enters'), handoffs(work / 'writer.perf', windows))))
    if reader:
        # The follower's windows end where its FOLLOWED line's write begins.
        done = epoch('follower') + int(polls[2])
        clipped = {phase: (lo, min(hi, done)) for phase, (lo, hi) in windows.items()}
        row.update(follower_polls=int(polls[1]),
                   **dict(zip(('follower_handoffs', 'follower_ring_enters'), handoffs(work / 'follower.perf', clipped))))
    return row


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--artifacts', type=Path, required=True)
    p.add_argument('--out', type=Path, required=True)
    p.add_argument('--prepare', action='store_true')
    p.add_argument('--measure', action='store_true')
    p.add_argument('--check-burst', choices=['normal', 'substitution'],
                   help='run one RB configuration without CPU shielding to check durable contents')
    p.add_argument('--allocations', action='store_true',
                   help='prepare or sample the allocation-counting binary instead of the shipping binary')
    p.add_argument('--profile', action='append', choices=['dev', 'release'])
    p.add_argument('--workload', action='append', choices=['R4', 'R5', 'R6', 'R7', 'R8-load', 'R8-open', 'migrate', 'RB', 'BL15'])
    p.add_argument('--index', action='append', type=int, choices=[0, 1])
    p.add_argument('--timing-hold', action='store_true', help='shield CPU 16 for each measured sample')
    p.add_argument('--unshielded', action='store_true', help='count phase instructions on the build pool')
    args = p.parse_args()
    if args.unshielded and (args.timing_hold or set(args.workload or ()) & {'BL15', 'RB'}):
        p.error('--unshielded is for phase counters')
    if args.check_burst and args.measure:
        p.error('--check-burst and --measure are separate runs')
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
    profiles = args.profile or PROFILES
    if args.prepare:
        build_env = env | {'HIRSEL_MEMORY_LIMIT': os.environ.get('HIRSEL_MEMORY_LIMIT', '2G')}
        if args.allocations:
            cc = allocation_linker(cc, out)
        store = ROOT / 'core/programs/store/store.hirsel'
        source = (store.read_text() + '\n'
                  + (store.parent / 'disk_bench.hirsel').read_text())
        frozen = out / 'store.hirsel'
        frozen.write_text(source)
        corpus = inputs(ROOT, tests=True)
        store_input, = (item for item in corpus['packages'][-1]['sources'] if item['file'] == str(store))
        store_input['file'] = str(frozen)
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
            cpu=None if args.unshielded else 16, protocol='READY/DONE; R8-load counts creation, population and the first checkpoint; other modes exclude population; validation and final drops excluded'), indent=2) + '\n')
    if args.check_burst:
        for profile in profiles:
            try:
                row = burst_sample(out / (profile + suffix), host, out, env | {'HIRSEL_UNTIED': '1'},
                                   97, 1, 1, check=args.check_burst)
            except ValueError as error:
                print(error, file=sys.stderr)
                return 1
            if args.check_burst == 'substitution':
                print('RB negative control unexpectedly passed', file=sys.stderr)
                return 2
            print(f'{row["tag"]}: verified {row["verified_updates"]} acknowledged updates '
                  f'and {row["verified_reopened_rows"]} reopened rows', flush=True)
    if args.measure:
        # One pool worker: the bench body's spawned unit runs on the pool, so its file ops take the
        # slot ring (STORE-SPEC §3.3b) instead of running inline on a tied thread.
        env |= {'HIRSEL_UNTIED': '1'}
        rows = []
        with ExitStack() as locks:
            if not args.unshielded:
                for name in ('core-llvm-905-measure.lock', 'hirsel-langarena-measure.lock'):
                    lock = locks.enter_context(open('/tmp/' + name, 'a'))
                    fcntl.flock(lock, fcntl.LOCK_EX)
            for profile in profiles:
                for mode in args.workload or PHASES:
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
                                    row = burst_sample(out / (profile + suffix), host, out, env, width, burst, stores)
                                    rows.append(row)
                                    (out / (row['tag'] + '-sample.json')).write_text(json.dumps(row, indent=2) + '\n')
                                    print(row['tag'], json.dumps(row), flush=True)
                        continue
                    indexes = args.index or phase_indexes(mode)
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
                                          before_phase=before_phase if mode == 'R6' else None,
                                          unshielded=args.unshielded)
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
