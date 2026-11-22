#!/usr/bin/env python3
"""Same-J puts at 0/1/3 indexes, before and after S1.2; phase counters on CPU16."""
import argparse
import fcntl
import json
import os
from pathlib import Path
import subprocess

from measurement_artifacts import check
from omap_bench import command, measure
from std_native import ROOT
from store_tests import inputs


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--artifacts', type=Path, required=True)
    p.add_argument('--out', type=Path, required=True)
    p.add_argument('--phase', choices=['before', 'after'], required=True)
    p.add_argument('--prepare', action='store_true')
    p.add_argument('--measure', action='store_true')
    args = p.parse_args()
    identity = json.loads(args.artifacts.read_text())
    check(identity['compiler'], identity)
    check(identity['runtime'], identity)
    directory = args.out.resolve() / args.phase
    directory.mkdir(parents=True, exist_ok=True)
    host = directory / 'host'
    host.mkdir(exist_ok=True)
    env = os.environ | {'HIRSEL_MEMORY_LIMIT': '2G',
                        'LD_LIBRARY_PATH': str(ROOT / 'tools/buck2/toolchains/llvm-files/lib')}
    if args.prepare:
        frozen = directory / 'store.hirsel'
        frozen.write_text((ROOT / 'core/programs/store/store.hirsel').read_text())
        app = directory / 'index_bench.hirsel'
        app.write_text((ROOT / 'core/programs/store/index_bench.hirsel').read_text())
        manifest = inputs(ROOT)
        for source in manifest['packages'][-1]['sources']:
            if Path(source['file']).name == 'store.hirsel':
                source['file'] = str(frozen)
        manifest['packages'][-1]['sources'].append(dict(file=str(app), modules=['index_bench'], signatures=False))
        path = directory / 'inputs.json'
        path.write_text(json.dumps(manifest))
        (directory / 'environment.json').write_text(json.dumps(dict(
            artifacts=identity, phase=args.phase, cpu=16, population=100000, measured_puts=10000,
            value='Account{id:u64,balance:i64,owner:owner000}; same32-byte codec as R1',
            indexes='0/1/3 named nonunique indexes, each J=id%4; every update changes balance only',
            source_base=subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip(),
            sample='one/configuration; population and final verification/drop excluded'), indent=2) + '\n')
        for profile, backend in [('dev', 'cranelift'), ('release', 'llvm')]:
            argv = [ROOT / 'tools/capped', identity['compiler'], 'build', '--inputs', path,
                    '--source-core', '--source-std', '--entry', 'store@1/index_bench/main',
                    '--backend', backend, '--runtime', identity['runtime'], '--dir', host,
                    '--grant', 'Stdout', '--grant', 'Clock', '--grant', 'Random',
                    '--limit', '1073741824', '--store', '/tmp/store-1010-index-measure-' + args.phase,
                    '--cc', ROOT / 'tools/buck2/toolchains/llvm-files/bin/hirsel-clang',
                    '-o', directory / ('hirsel-' + profile), '--json']
            if profile == 'release':
                argv.append('--release')
            command(argv, directory / (profile + '-build.json'), env)
            print('prepared', args.phase, profile, flush=True)
    if args.measure:
        rows = []
        with open('/tmp/core-llvm-905-measure.lock', 'a') as core_lock, open('/tmp/hirsel-langarena-measure.lock', 'a') as shared_lock:
            fcntl.flock(core_lock, fcntl.LOCK_EX)
            fcntl.flock(shared_lock, fcntl.LOCK_EX)
            for profile in ['dev', 'release']:
                for count in [0, 1, 3]:
                    row = measure([directory / ('hirsel-' + profile), str(count)], directory, env,
                                  profile + '-indexes-' + str(count), 1049995000, host_directory=host)
                    row.update(phase=args.phase, profile=profile, indexes=count, artifacts=identity,
                               instructions_per_put=row['instructions'] / 10000 if row['instructions'] else None)
                    rows.append(row)
                    (directory / 'samples.json').write_text(json.dumps(rows, indent=2) + '\n')
                    print(args.phase, profile, count, row, flush=True)
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
