#!/usr/bin/env python3
"""Build the safe Rust twins with pinned rustc and already-local dependency sources.

serde_core is the pinned 1.0.229 std-feature rlib produced by Buck. Other versions
come from LangArena's Cargo.lock; no network, Cargo feature-graph changes or downloads.
"""
import argparse
from pathlib import Path
import os
import subprocess
import sys
import time
import tomllib

ROOT = Path(__file__).resolve().parents[3]


def build(command):
    while os.getloadavg()[0] > 32:
        time.sleep(10)
    print(' '.join(map(str, command)), flush=True)
    subprocess.run(command, check=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--serde-core', type=Path, required=True)
    parser.add_argument('--profile', choices=['release', 'dev'], default='release')
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    rustc = ROOT / 'tools/buck2/toolchains/rust-files/bin/rustc'
    flags = ['--target=x86_64-unknown-linux-gnu', '-C', 'target-cpu=x86-64',
             '-C', 'opt-level=' + ('3' if args.profile == 'release' else '0'),
             '-C', 'overflow-checks=yes', '-C', 'codegen-units=1', '-C', 'lto=no',
             '-C', 'debuginfo=0', '-C', 'debug-assertions=' + ('no' if args.profile == 'release' else 'yes')]
    lock = tomllib.loads((ROOT / 'core/langarena/rust/Cargo.lock').read_text())
    versions = {p['name']: p['version'] for p in lock['package']}
    dependencies = {}
    registry = Path.home() / '.cargo/registry/src'
    for name in ['itoa', 'memchr', 'zmij']:
        source = next(registry.glob(f'*/{name}-{versions[name]}/src/lib.rs'))
        output = args.out / f'lib{name}.rlib'
        build([rustc, '--edition=2021', '--crate-name', name, '--crate-type=rlib', *flags,
               '--cfg', 'feature="std"', '--cfg', 'feature="alloc"', source, '-o', output])
        dependencies[name] = output.resolve()
    dependencies['serde_core'] = args.serde_core.resolve()
    source = next(registry.glob(f'*/serde_json-{versions["serde_json"]}/src/lib.rs'))
    serde = args.out / 'libserde_json.rlib'
    external = [arg for name, path in dependencies.items() for arg in ['--extern', f'{name}={path}']]
    build([rustc, '--edition=2021', '--crate-name', 'serde_json', '--crate-type=rlib', *flags,
           '--cfg', 'feature="std"', '--cfg', 'fast_arithmetic="64"', *external, source, '-o', serde])
    for name in ['rust', 'json_matched', 'json_serde']:
        extra = ['--extern', f'serde_json={serde.resolve()}', '-L', f'dependency={args.out.resolve()}',
                 '-L', f'dependency={args.serde_core.resolve().parent}'] if name == 'json_serde' else []
        build([rustc, '--edition=2024', *flags, *extra, Path(__file__).with_name(name + '.rs'), '-o', args.out / name])
    environment = subprocess.check_output([rustc, '-vV']).decode()
    (args.out / 'environment.txt').write_text(environment + repr(flags) + '\n' + repr(versions) + '\n')


if __name__ == '__main__':
    main()
