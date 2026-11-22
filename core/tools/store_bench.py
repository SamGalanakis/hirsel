#!/usr/bin/env python3
"""R1–R3, using the ordered-map bench's gated, CPU-16 phase protocol."""
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

EXPECTED = {'R1': 5000750000, 'R2': 5000750000, 'R3': 4994343687}


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--artifacts', type=Path, required=True)
    p.add_argument('--out', type=Path, required=True)
    p.add_argument('--store-source', type=Path,
                   help='store source to freeze before preparing either native configuration')
    p.add_argument('--samples', type=int, choices=range(1, 6), default=1)
    p.add_argument('--prepare', action='store_true')
    p.add_argument('--measure', action='store_true')
    p.add_argument('--profile', action='store_true')
    p.add_argument('--only-language', action='append', choices=['hirsel', 'rust'])
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
    if args.prepare:
        manifest = inputs(ROOT)
        source = args.store_source or ROOT / 'core/programs/store/store.hirsel'
        frozen = out / 'store.hirsel'
        frozen.write_text(source.read_text())
        for item in manifest['packages'][-1]['sources']:
            if Path(item['file']).name == 'store.hirsel':
                item['file'] = str(frozen)
        manifest['packages'][-1]['sources'].append(dict(file=str(ROOT / 'core/programs/store/bench.hirsel'),
                                                       modules=['bench'], signatures=False))
        (out / 'inputs.json').write_text(json.dumps(manifest))
        rustc = subprocess.check_output(['rustup', 'which', '--toolchain', '1.99.0', 'rustc'], text=True).strip()
        metadata = dict(artifacts=identity, source_base=subprocess.check_output(
            ['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip(),
            rustc=subprocess.check_output([rustc, '-vV'], text=True), cpu=16,
            host=list(os.uname()), cpu_model=subprocess.check_output(['lscpu'], text=True),
            workloads=EXPECTED, sample='one per configuration; population and final drops excluded; OS cache not flushed')
        (out / 'environment.json').write_text(json.dumps(metadata, indent=2) + '\n')
        for profile, backend, opt in [('dev', 'cranelift', '0'), ('release', 'llvm', '3')]:
            native = [ROOT / 'tools/capped', identity['compiler'], 'build', '--inputs', out / 'inputs.json',
                      '--source-core', '--source-std', '--entry', 'store@1/bench/main',
                      '--backend', backend, '--runtime', identity['runtime'], '--dir', host,
                      '--grant', 'Stdout', '--grant', 'Clock', '--grant', 'Random', '--grant', 'Tasks',
                      '--limit', '134217728', '--store', out / 'store',
                      '--cc', ROOT / 'tools/buck2/toolchains/llvm-files/bin/hirsel-clang',
                      '-o', out / ('hirsel-' + profile), '--json']
            if profile == 'release':
                native.append('--release')
            command(native, out / ('hirsel-' + profile + '-build.json'), env)
            command([ROOT / 'tools/capped', rustc, '--edition=2024', '--target=x86_64-unknown-linux-gnu',
                     '-Ctarget-cpu=x86-64', '-Copt-level=' + opt, '-Coverflow-checks=yes', '-Ccodegen-units=1',
                     '-Cdebug-assertions=' + ('yes' if profile == 'dev' else 'no'), '-Cdebuginfo=0',
                     ROOT / 'core/twins/m4/store_mem.rs', '-o', out / ('rust-' + profile)],
                    out / ('rust-' + profile + '-build.log'), env)
            print('prepared', profile, flush=True)
    if args.measure or args.profile:
        locks = [open('/tmp/' + name, 'a') for name in
                 ['core-llvm-905-measure.lock', 'hirsel-langarena-measure.lock']]
        for lock in locks:
            fcntl.flock(lock, fcntl.LOCK_EX)
        rows = []
        profiles = ['release'] if args.profile else ['dev', 'release']
        for profile in profiles:
            for mode in EXPECTED:
                languages = ['hirsel'] if args.profile else ['hirsel', 'rust']
                for language in languages:
                    if args.only_language and language not in args.only_language:
                        continue
                    argv = [out / (language + '-' + profile), mode]
                    if language == 'rust':
                        argv.append(host / 'gate')
                    tag = '-'.join([language, profile, mode]) + ('-insert-profile' if args.profile and mode == 'R1' else '-profile' if args.profile else '')
                    for sample in range(args.samples):
                        row = measure(argv, out, env, tag + ('-' + str(sample) if args.samples > 1 else ''), EXPECTED[mode],
                                      recording=args.profile, host_directory=host)
                        row.update(language=language, profile=profile, workload=mode, artifacts=identity)
                        rows.append(row)
                        print(tag, row, flush=True)
                        (out / ('profiles.json' if args.profile else 'samples.json')).write_text(json.dumps(rows, indent=2) + '\n')
        for lock in locks:
            lock.close()
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
