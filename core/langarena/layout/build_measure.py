#!/usr/bin/env python3
"""Fresh/reused/one-body-edit build work for the complete graph lifetime.

Rust compiles the reviewed graph functions only, with prebuilt std; Hirsel reads
canonical core/std source. A check refusal is reported without a check ratio.
A retained Hirsel record hit is an absolute work count, not incremental rustc.
"""
import argparse
import importlib.util
import json
import os
import re
from pathlib import Path
import subprocess
import sys
from run import manifest

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / 'core/tools'))
from std_native import wait_load


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--compiler', type=Path, required=True)
    parser.add_argument('--runtime', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=False)
    os.environ['HIRSEL_MEMORY_LIMIT'] = '2G'
    spec = importlib.util.spec_from_file_location('measurement', ROOT / 'core/tools/measure.py')
    measurement = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(measurement)
    graph = Path(__file__).with_name('graph.hirsel').read_text()
    hirsel = args.out / 'graph.hirsel'
    hirsel.write_text(graph)
    sources = manifest('graph')
    sources['packages'][2]['sources'][0]['file'] = str(hirsel.resolve())
    inputs = args.out / 'inputs.json'
    inputs.write_text(json.dumps(sources))
    reviewed = Path(__file__).with_name('rust.rs').read_text()
    functions = reviewed[reviewed.index('#[derive(Clone)]'):reviewed.index('fn main()')]
    rust_source = 'use std::{rc::Rc,sync::{Arc,atomic::{AtomicU64,Ordering}}};\n' + functions + '\nfn main(){println!("{}",exclusive(8192,32)+churn(4096,16)+snapshots_rc(64,256)+snapshots_arc(64,256));}\n'
    rust = args.out / 'graph.rs'
    rust.write_text(rust_source)
    rustc = ROOT / 'tools/buck2/toolchains/rust-files/bin/rustc'
    flags = ['--edition=2024', '--target=x86_64-unknown-linux-gnu', '-C', 'target-cpu=x86-64',
             '-C', 'opt-level=3', '-C', 'overflow-checks=yes', '-C', 'codegen-units=1',
             '-C', 'lto=no', '-C', 'debuginfo=0', '-C', 'debug-assertions=no']
    rows = []
    for stage in ['check', 'fresh', 'reuse', 'edit']:
        if stage == 'edit':
            hirsel.write_text(graph.replace('let mut sum: u64 = 0', 'let mut sum: u64 = 1', 1))
            rust.write_text(re.sub(r'let mut sum\s*=\s*0\s*;', 'let mut sum = 1;', rust_source, count=1))
        for side in ['hirsel', 'rust']:
            binary = args.out / f'{stage}-{side}-program'
            if side == 'hirsel':
                command = [str(args.compiler.resolve()), 'check' if stage == 'check' else 'build',
                           '--inputs', str(inputs.resolve()), '--entry', 'layout@1/graph/lifetime_main',
                           '--source-core', '--store', str((args.out / 'store').resolve()), '--json',
                           '--metrics', str((args.out / f'{stage}.metrics.json').resolve())]
                if stage != 'check':
                    command += ['--backend', 'llvm', '--release', '--runtime', str(args.runtime.resolve()), '--grant', 'Stdout', '-o', str(binary.resolve())]
            else:
                command = [str(rustc), *flags, str(rust.resolve()), '-o', str(binary.resolve())]
                if stage == 'check': command += ['--emit=metadata']
            wait_load()
            sample = measurement.sample(['taskset', '-c', '16', *command], args.out / f'{stage}-{side}')
            if side == 'hirsel':
                try:
                    product = json.loads((args.out / f'{stage}-{side}/stdout').read_text())
                    if not product.get('accepted'): sample['status'] = 'refused-or-incomplete'
                    if stage != 'check' and not product.get('native'): sample['status'] = 'no-native-product'
                except ValueError: sample['status'] = 'invalid-product-output'
            if stage != 'check' and sample['status'] == 'ok':
                observed = subprocess.run([str(binary.resolve())], capture_output=True)
                expected = b'45533441\n' if stage == 'edit' else b'45533440\n'
                if observed.returncode or observed.stdout != expected or observed.stderr:
                    sample['status'] = 'wrong-output'
            row = dict(stage=stage, side=side, **sample)
            rows.append(row)
            (args.out / 'results.json').write_text(json.dumps(rows, indent=2, default=str) + '\n')
            print(json.dumps(row, default=str), flush=True)
    return int(any(r['status'] != 'ok' for r in rows))


if __name__ == '__main__':
    raise SystemExit(main())
