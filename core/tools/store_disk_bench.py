#!/usr/bin/env python3
"""S2 R6-R8 and S3 R4/R5 with private open, frozen sources and gated CPU16 phase counters."""
import argparse
import fcntl
import json
import os
from pathlib import Path
import re
from struct import error as StructError

from measurement_artifacts import allocation_linker, check, phase_allocations
from omap_bench import command, measure
from std_native import ROOT
from store_r6 import Manifest, check_r6
from store_tests import inputs


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


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--artifacts', type=Path, required=True)
    p.add_argument('--out', type=Path, required=True)
    p.add_argument('--prepare', action='store_true')
    p.add_argument('--measure', action='store_true')
    p.add_argument('--allocations', action='store_true',
                   help='prepare or sample the allocation-counting binary instead of the shipping binary')
    p.add_argument('--profile', action='append', choices=['dev', 'release'])
    p.add_argument('--workload', action='append', choices=['R4', 'R5', 'R6', 'R7', 'R8-load', 'R8-open', 'migrate'])
    p.add_argument('--index', action='append', type=int, choices=[0, 1])
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
        corpus = inputs(ROOT)
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
            cpu=16, protocol='READY/DONE; population, validation and final drops excluded'), indent=2) + '\n')
    if args.measure:
        rows = []
        with open('/tmp/core-llvm-905-measure.lock', 'a') as core_lock, open('/tmp/hirsel-langarena-measure.lock', 'a') as shared_lock:
            fcntl.flock(core_lock, fcntl.LOCK_EX)
            fcntl.flock(shared_lock, fcntl.LOCK_EX)
            for profile in profiles:
                for mode in args.workload or ['R4', 'R5', 'R6', 'R7', 'R8-load', 'R8-open']:
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
