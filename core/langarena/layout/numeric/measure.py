#!/usr/bin/env python3
"""One LAYOUT3 sample per layout, engine and checked/wrapping Rust variant."""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[4]
spec = importlib.util.spec_from_file_location('langarena_runner', ROOT / 'core/langarena/run.py')
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--compiler', type=Path, required=True)
    p.add_argument('--runtime', type=Path, required=True)
    p.add_argument('--compiler-profile', choices=['release', 'checked', 'unknown'], default='unknown')
    p.add_argument('--runtime-profile', choices=['ordinary-release', 'checked', 'unknown'])
    p.add_argument('--diagnostic', action='store_true')
    p.add_argument('--resume', type=Path, help='reuse complete Rust sides; Hirsel requires identical toolchain')
    p.add_argument('--rustc', type=Path, default=ROOT / 'tools/buck2/toolchains/rust-files/bin/rustc')
    p.add_argument('--out', type=Path, required=True)
    args = p.parse_args()
    try:
        toolchain = runner.toolchain_identity(args.compiler, args.runtime, args.compiler_profile, args.runtime_profile, args.diagnostic)
    except (OSError, ValueError) as error:
        p.error(str(error))
    previous = json.loads(args.resume.read_text()) if args.resume else None
    out = args.out.resolve()
    if out.exists() or out.is_relative_to(ROOT):
        p.error('--out must be new and outside the repository')
    if 16 not in os.sched_getaffinity(0):
        p.error('CPU16 required')
    out.mkdir(parents=True)
    os.sched_setaffinity(0, {16})
    os.environ['HIRSEL_MEMORY_LIMIT'] = '2G'
    os.environ['HIRSEL_JOBS'] = '1'
    family = ROOT / 'core/langarena/layout/numeric'
    host = out / 'host'
    host.mkdir()
    (host / 'config.json').write_bytes((family / 'run.json').read_bytes())
    manifest = out / 'inputs.json'
    manifest.write_text(json.dumps(runner.inputs(ROOT, 'layout/numeric/pack')))
    report = {'source': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip(),
              'rustc': subprocess.check_output([str(args.rustc), '-vV'], text=True),
              'cpu': 16, 'rows': [], 'toolchain': toolchain,
              'result_kind': 'diagnostic' if args.diagnostic else 'result', 'sample_policy': 'one per configuration; shared serial lock; no timeout'}
    def save():
        (out / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
    configs = json.loads((family / 'run.json').read_text())
    for engine in ['llvm', 'cranelift']:
        prior_rows = [row for row in previous['rows'] if row['engine'] == engine] if previous else []
        covered = {row['layout'] for row in prior_rows} == {row['name'].rsplit('::', 1)[1] for row in configs}
        reuse_hirsel = covered and previous.get('toolchain') == toolchain and all(row['samples'].get('hirsel', {}).get('status') == 'ok' for row in prior_rows)
        exe = out / ('hirsel-' + engine)
        command = [str(args.compiler.resolve()), 'build', '--inputs', str(manifest),
                   '--entry', 'langarena@1/layout/numeric/pack/main', '--backend', engine,
                   '--runtime', str(args.runtime.resolve()), '--store', str(out / 'store'),
                   '--dir', str(host), '--grant', 'Stdout', '--grant', 'Tasks', '--json', '-o', str(exe)]
        if engine == 'llvm':
            command.append('--release')
        built = None
        if not reuse_hirsel:
            runner.wait_load()
            with (out / ('build-' + engine + '.log')).open('w') as log:
                built = subprocess.run(command, stdout=log, stderr=subprocess.STDOUT)
        binaries = {}
        for variant in ['checked', 'wrapping']:
            if covered and all(row['samples'].get(variant, {}).get('status') == 'ok' for row in prior_rows):
                binaries[variant] = None
                continue
            binary = out / ('rust-' + engine + '-' + variant)
            command = [str(args.rustc.resolve()), '--edition=2024', '--target', 'x86_64-unknown-linux-gnu',
                       '-C', 'target-cpu=x86-64', '-C', 'overflow-checks=on', '-C', 'codegen-units=1',
                       '-C', 'opt-level=' + ('3' if engine == 'llvm' else '0'),
                       str(family / 'numeric.rs'), '-o', str(binary)]
            if variant == 'wrapping':
                command += ['--cfg', 'wrapping']
            runner.wait_load()
            with (out / ('build-rust-' + engine + '-' + variant + '.log')).open('w') as log:
                rust = subprocess.run(command, stdout=log, stderr=subprocess.STDOUT)
            binaries[variant] = binary if rust.returncode == 0 else None
        for config in configs:
            task = config['name']
            layout = task.rsplit('::', 1)[1]
            row = {'engine': engine, 'layout': layout, 'samples': {}, 'ratios': {}, 'toolchain': toolchain}
            prior = next((r for r in previous['rows'] if r['engine'] == engine and r['layout'] == layout), None) if previous else None
            row['samples']['hirsel'] = prior['samples']['hirsel'] if reuse_hirsel else runner.observe([str(exe), 'config.json', task],
                out / (engine + '-' + layout + '-hirsel'), task, config['checksum']) if built.returncode == 0 else {'status': 'build-failed'}
            for variant, binary in binaries.items():
                if prior and prior['samples'].get(variant, {}).get('status') == 'ok':
                    row['samples'][variant] = prior['samples'][variant]
                    row['ratios'][variant] = runner.ratio(row['samples']['hirsel'], row['samples'][variant]) if runner.reviewed(family / 'numeric.rs', 'langs-numeric-920') else {'status': 'blocked: independent review pending'}
                    continue
                row['samples'][variant] = runner.observe(runner.matched_command(binary, task, config),
                    out / (engine + '-' + layout + '-' + variant), task, config['checksum']) if binary else {'status': 'build-failed'}
                row['ratios'][variant] = runner.ratio(row['samples']['hirsel'], row['samples'][variant]) if runner.reviewed(family / 'numeric.rs', 'langs-numeric-920') else {'status': 'blocked: independent review pending'}
            report['rows'].append(row)
            save()
            print(engine, layout, row['ratios'], flush=True)
    return int(any(s['status'] != 'ok' for row in report['rows'] for s in row['samples'].values()))


if __name__ == '__main__':
    raise SystemExit(main())
