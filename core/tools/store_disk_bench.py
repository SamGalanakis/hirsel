#!/usr/bin/env python3
"""S2 R6-R8 with private open, frozen sources and gated CPU16 phase counters."""
import argparse
import fcntl
import json
import os
from pathlib import Path
import re

from measurement_artifacts import check
from omap_bench import command, measure
from std_native import ROOT
from store_tests import inputs


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--artifacts', type=Path, required=True)
    p.add_argument('--out', type=Path, required=True)
    p.add_argument('--prepare', action='store_true')
    p.add_argument('--measure', action='store_true')
    p.add_argument('--profile', action='append', choices=['dev', 'release'])
    p.add_argument('--workload', action='append', choices=['R6', 'R7', 'R8-load', 'R8-open'])
    p.add_argument('--index', action='append', type=int, choices=[0, 1])
    args = p.parse_args()
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=True)
    host = out / 'host'
    host.mkdir(exist_ok=True)
    identity = json.loads(args.artifacts.read_text())
    check(identity['compiler'], identity)
    check(identity['runtime'], identity)
    env = os.environ | {'HIRSEL_MEMORY_LIMIT': '2G',
                        'LD_LIBRARY_PATH': str(ROOT / 'tools/buck2/toolchains/llvm-files/lib')}
    profiles = args.profile or ['dev', 'release']
    if args.prepare:
        source = ((ROOT / 'core/programs/store/store.hirsel').read_text() + '\n'
                  + (ROOT / 'core/programs/store/disk_bench.hirsel').read_text())
        frozen = out / 'store.hirsel'
        frozen.write_text(source)
        corpus = inputs(ROOT)
        corpus['packages'][-1]['sources'][0]['file'] = str(frozen)
        manifest = out / 'inputs.json'
        manifest.write_text(json.dumps(corpus))
        for profile in profiles:
            command_line = [ROOT / 'tools/capped', identity['compiler'], 'build', '--inputs', manifest,
                            '--source-core', '--source-std', '--entry', 'store@1/store/disk_bench_main',
                            '--backend', 'llvm' if profile == 'release' else 'cranelift',
                            '--runtime', identity['runtime'], '--dir', host,
                            '--limit', '1073741824', '--store', out / 'objects',
                            '--cc', ROOT / 'tools/buck2/toolchains/llvm-files/bin/hirsel-clang',
                            '-o', out / profile, '--json']
            for grant in ['Clock', 'Random', 'Tasks', 'Stdout']:
                command_line += ['--grant', grant]
            if profile == 'release':
                command_line.append('--release')
            command(command_line, out / (profile + '-build.json'), env)
            print('prepared', profile, flush=True)
        (out / 'environment.json').write_text(json.dumps(dict(
            artifacts=identity, root_limit=1073741824, task_limit=262400276,
            population='100k R6/R7, 1M R8; Account codec32 plus schema2, tree key12',
            indexes='none or one J=id%4', sample='one per configuration',
            cpu=16, protocol='READY/DONE; population, validation and final drops excluded'), indent=2) + '\n')
    if args.measure:
        rows = []
        with open('/tmp/core-llvm-905-measure.lock', 'a') as core_lock, open('/tmp/hirsel-langarena-measure.lock', 'a') as shared_lock:
            fcntl.flock(core_lock, fcntl.LOCK_EX)
            fcntl.flock(shared_lock, fcntl.LOCK_EX)
            for profile in profiles:
                for mode in args.workload or ['R6', 'R7', 'R8-load', 'R8-open']:
                    indexes = args.index or ([0, 1] if mode.startswith('R8') else [0])
                    for index in indexes:
                        n = 1000000 if mode.startswith('R8') else 100000
                        tag = f'{profile}-{mode}-i{index}'
                        row = measure([out / profile, mode, str(index)], out, env, tag, n, host_directory=host)
                        text = (out / tag / 'stdout').read_text()
                        extra = re.search(r'PACK_BYTES (\d+) NEW_NODES (\d+) ACCOUNT (\d+)\n', text)
                        row.update(profile=profile, workload=mode, indexes=index, artifacts=identity)
                        if extra:
                            row.update(pack_bytes=int(extra[1]), new_nodes=int(extra[2]), account=int(extra[3]))
                        else:
                            row['complete'] = False
                            row['censored'] = row['censored'] or 'post-phase store validation did not finish'
                        rows.append(row)
                        (out / (tag + '-sample.json')).write_text(json.dumps(row, indent=2) + '\n')
                        print(tag, json.dumps(row), flush=True)
        (out / 'samples.json').write_text(json.dumps(rows, indent=2) + '\n')
        return int(not all(row['complete'] for row in rows))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
