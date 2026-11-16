#!/usr/bin/env python3
"""Prepare and sample R9 once per engine/profile using pinned shipping artifacts."""
import argparse
import fcntl
import json
import os
from pathlib import Path
import subprocess

from measure import sample
from measurement_artifacts import check
from omap_bench import inputs
from std_native import ROOT, wait_load


def build(command, log, env):
    wait_load()
    with log.open('w') as output:
        subprocess.run([str(ROOT / 'tools/capped'), *map(str, command)], env=env,
                       stdout=output, stderr=subprocess.STDOUT, check=True)


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--artifacts', type=Path, required=True)
    p.add_argument('--out', type=Path, required=True)
    p.add_argument('--registry', type=Path,
                   default=Path.home() / '.cargo/registry/src/index.crates.io-1949cf8c6b5b557f')
    p.add_argument('--prepare', action='store_true')
    p.add_argument('--measure', action='store_true')
    args = p.parse_args()
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=True)
    identity = json.loads(args.artifacts.read_text())
    check(identity['compiler'], identity)
    check(identity['runtime'], identity)
    env = os.environ | {'HIRSEL_MEMORY_LIMIT': '2G',
                        'LD_LIBRARY_PATH': str(ROOT / 'tools/buck2/toolchains/llvm-files/lib')}
    rustc = Path(subprocess.check_output(['rustup', 'which', '--toolchain', '1.99.0', 'rustc'], text=True).strip())
    if args.prepare:
        metadata = dict(artifacts=identity, source_base=subprocess.check_output(
            ['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip(),
            rustc=subprocess.check_output([rustc, '-vV'], text=True), cpu=16,
            cpu_model=subprocess.check_output(['lscpu'], text=True),
            host=list(os.uname()), workload='10000 checks of bytes (17*i+3) mod 256 for i=0..4095',
            cache='fresh processes; OS page cache neither flushed nor claimed cold')
        (out / 'environment.json').write_text(json.dumps(metadata, indent=2) + '\n')
        for profile, backend, opt in [('dev', 'cranelift', '0'), ('release', 'llvm', '3')]:
            flags = ['--edition=2024', '--target=x86_64-unknown-linux-gnu', '-Ctarget-cpu=x86-64',
                     '-Copt-level=' + opt, '-Coverflow-checks=yes', '-Ccodegen-units=1',
                     '-Cdebug-assertions=' + ('yes' if profile == 'dev' else 'no'), '-Cdebuginfo=0']
            upstream = args.registry / 'xxhash-rust-0.8.15/src/lib.rs'
            library = out / ('libxxhash_rust-' + profile + '.rlib')
            build([rustc, '--crate-name', 'xxhash_rust', '--crate-type=rlib',
                   upstream, '--cfg', 'feature="xxh3"', *flags, '-o', library],
                  out / ('xxh3-upstream-' + profile + '.log'), env)
            for algorithm in ['crc', 'xxh3']:
                manifest = out / (algorithm + '-inputs.json')
                manifest.write_text(json.dumps(inputs(ROOT / 'core/programs/store' / (algorithm + '_bench.hirsel'))))
                command = [identity['compiler'], 'build', '--inputs', manifest,
                           '--source-core', '--source-std', '--entry', 'omap_bench@1/main/main',
                           '--backend', backend, '--runtime', identity['runtime'], '--grant', 'Stdout',
                           '--limit', '67108864', '--store', out / 'store',
                           '--cc', ROOT / 'tools/buck2/toolchains/llvm-files/bin/hirsel-clang',
                           '-o', out / ('hirsel-' + algorithm + '-' + profile), '--json']
                if profile == 'release':
                    command.append('--release')
                build(command, out / ('hirsel-' + algorithm + '-' + profile + '-build.json'), env)
                command = [rustc, ROOT / 'core/twins/m4' / (algorithm + '.rs'), *flags,
                           '-o', out / ('rust-' + algorithm + '-' + profile)]
                if algorithm == 'xxh3':
                    command += ['--extern', 'xxhash_rust=' + str(library)]
                build(command, out / ('rust-' + algorithm + '-' + profile + '-build.log'), env)
                print(f'prepared {algorithm} {profile}', flush=True)
    if args.measure:
        locks = [open('/tmp/' + name, 'a') for name in
                 ['core-llvm-905-measure.lock', 'hirsel-langarena-measure.lock']]
        for lock in locks:
            fcntl.flock(lock, fcntl.LOCK_EX)
        os.sched_setaffinity(0, {16})
        os.environ.update(env)
        rows = []
        for profile in ['dev', 'release']:
            for algorithm in ['crc', 'xxh3']:
                for language in ['hirsel', 'rust']:
                    name = '-'.join([language, algorithm, profile])
                    wait_load()
                    row = sample([out / name, '10000'], out / (name + '-sample'))
                    row.update(language=language, algorithm=algorithm, profile=profile,
                               artifacts=identity)
                    row['directory'] = str(row['directory'])
                    rows.append(row)
                    (out / 'samples.json').write_text(json.dumps(rows, indent=2) + '\n')
                    print(name, row['status'], row['instructions'], row['seconds'], flush=True)
        for lock in locks:
            lock.close()
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
