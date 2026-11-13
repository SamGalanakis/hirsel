#!/usr/bin/env python3
"""Memory cost samples under core/MEASURE.md: CPU 16, one job, one sample.

Run from the repository: python3 crates/hirsel-core-rt/measure.py --out <new-dir>
Builds first-party code and upstream Rust allocator wrappers with the pinned
rustc, without Cargo flags or downloads. Mimalloc uses the installed C library;
its exact Debian version and the upstream wrapper versions are recorded.
"""
import argparse
import csv
import json
import os
from pathlib import Path
import platform
import re
import subprocess
import time

ROOT = Path(__file__).resolve().parents[2]
CRATE = ROOT / 'crates/hirsel-core-rt'
FLAGS = ['--target', 'x86_64-unknown-linux-gnu', '-C', 'target-cpu=x86-64',
         '-C', 'opt-level=3', '-C', 'overflow-checks=on', '-C', 'debug-assertions=no',
         '-C', 'codegen-units=1', '-C', 'debuginfo=0', '-C', 'lto=off']
WORKLOADS = ['alloc16', 'alloc64', 'alloc256', 'alloc4096', 'growth', 'region', 'rc', 'limits']


def wait_for_load():
    while os.getloadavg()[0] > 32:
        print(f'build deferred: one-minute load {os.getloadavg()[0]:.2f} > 32', flush=True)
        time.sleep(10)


def source(name):
    matches = list((Path.home() / '.cargo/registry/src').glob(f'*/{name}/src/lib.rs'))
    if len(matches) != 1:
        raise RuntimeError(f'expected one installed source for {name}: {matches}')
    return matches[0]


def build(out, env):
    rustc = subprocess.check_output(['rustup', 'which', '--toolchain', '1.99.0', 'rustc'], text=True).strip()
    information = {
        'rustc': subprocess.check_output([rustc, '-vV'], text=True),
        'platform': platform.platform(),
        'cpu': next(line.split(':', 1)[1].strip() for line in Path('/proc/cpuinfo').read_text().splitlines() if line.startswith('model name')),
        'source_base': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip(),
        'flags': FLAGS, 'cpu_affinity': [16], 'jobs': 1, 'memory_limit': '2G',
        'mimalloc': subprocess.check_output(['dpkg-query', '-W', 'libmimalloc2.0', 'libmimalloc-dev'], text=True),
        'wrappers': ['mimalloc-0.1.52', 'libmimalloc-sys-0.1.49', 'bumpalo-3.20.3'],
        'sampling': 'one first-run sample per configuration; OS page cache is shared and not flushed',
    }
    (out / 'environment.json').write_text(json.dumps(information, indent=2) + '\n')
    libs = out / 'lib'
    libs.mkdir(exist_ok=True)
    # Memory uses only charge-v1. Include that exact module rather than the
    # unrelated shape serializer and its proc-macro build graph.
    charge_root = out / 'layout_charge.rs'
    charge_root.write_text('#[path = ' + json.dumps(str(ROOT/'crates/hirsel-core-layout/src/charge.rs')) + ']\npub mod charge;\n')
    targets = [
        ('hirsel_core_layout', charge_root, '2024', []),
        ('libc', source('libc-0.2.190'), '2021', []),
        ('hirsel_stack', ROOT/'crates/hirsel-stack/src/lib.rs', '2024', []),
        ('bumpalo', source('bumpalo-3.20.3'), '2021', []),
        ('libmimalloc_sys', source('libmimalloc-sys-0.1.49'), '2018', ['-l', 'dylib=mimalloc']),
        ('mimalloc', source('mimalloc-0.1.52'), '2018', ['--extern', f'libmimalloc_sys={libs}/liblibmimalloc_sys.rlib']),
        ('hirsel_core_rt', CRATE/'src/lib.rs', '2024', ['--extern', f'hirsel_stack={libs}/libhirsel_stack.rlib', '--extern', f'libc={libs}/liblibc.rlib', '--extern', f'hirsel_core_layout={libs}/libhirsel_core_layout.rlib']),
        ('hirsel_core_rt_mimalloc', CRATE/'src/lib.rs', '2024', ['--cfg', 'feature="mimalloc"', '--extern', f'hirsel_stack={libs}/libhirsel_stack.rlib', '--extern', f'libc={libs}/liblibc.rlib', '--extern', f'libmimalloc_sys={libs}/liblibmimalloc_sys.rlib', '--extern', f'hirsel_core_layout={libs}/libhirsel_core_layout.rlib']),
    ]
    for name, src, edition, extra in targets:
        artifact = libs / f'lib{name}.rlib'
        if artifact.exists():
            continue
        wait_for_load()
        with (out / f'build-{name}.log').open('w') as log:
            command = [str(ROOT/'tools/capped'), 'taskset', '-c', '16', rustc,
                       str(src), '--edition', edition, '--crate-name', name,
                       '--crate-type', 'rlib', *FLAGS, '-L', f'dependency={libs}',
                       *extra, '-o', str(artifact)]
            subprocess.run(command, cwd=ROOT, env=env, stdout=log, stderr=subprocess.STDOUT, check=True)
    for allocator in ['system', 'mimalloc']:
        artifact = out / f'memory-{allocator}'
        if artifact.exists():
            continue
        wait_for_load()
        with (out / f'build-memory-{allocator}.log').open('w') as log:
            command = [str(ROOT/'tools/capped'), 'taskset', '-c', '16', rustc,
                       str(CRATE/f'examples/memory_{allocator}.rs'), '--edition', '2024',
                       *FLAGS, '-L', f'dependency={libs}']
            for name in ['hirsel_core_rt', 'hirsel_core_layout', 'bumpalo', 'mimalloc']:
                suffix = '_mimalloc' if name == 'hirsel_core_rt' and allocator == 'mimalloc' else ''
                command += ['--extern', f'{name}={libs}/lib{name}{suffix}.rlib']
            command += ['-o', str(artifact)]
            subprocess.run(command, cwd=ROOT, env=env, stdout=log, stderr=subprocess.STDOUT, check=True)


def expected(workload):
    if workload.startswith('alloc'):
        return 127493856
    if workload == 'rc':
        return 7000000
    if workload == 'limits':
        return 64000000
    return 499999500000


def abi_smoke(out, env):
    rustc = subprocess.check_output(['rustup', 'which', '--toolchain', '1.99.0', 'rustc'], text=True).strip()
    for allocator in ['system', 'mimalloc']:
        suffix = '' if allocator == 'system' else '-mimalloc'
        archive = out / f'libhirsel_core_rt{suffix}.a'
        extra = [] if allocator == 'system' else ['--cfg', 'feature="mimalloc"', '--extern', f'libmimalloc_sys={out}/lib/liblibmimalloc_sys.rlib']
        commands = [
            ['taskset','-c','16',rustc,str(CRATE/'src/lib.rs'),'--crate-name','hirsel_core_rt',
             '--crate-type','staticlib','--edition=2024',*FLAGS,*extra,'--extern',f'hirsel_stack={out}/lib/libhirsel_stack.rlib','--extern',f'libc={out}/lib/liblibc.rlib','--extern',
             f'hirsel_core_layout={out}/lib/libhirsel_core_layout.rlib','-L',f'dependency={out}/lib','-o',str(archive)],
            ['taskset','-c','16','cc','-std=c11','-O3','-Wall','-Wextra','-Werror','-pedantic',
             '-I'+str(CRATE/'include'),str(CRATE/'tests/abi.c'),str(archive),
             *([] if allocator == 'system' else ['-lmimalloc']),'-lpthread','-ldl','-lm','-o',str(out/f'c-abi{suffix}')],
            ['taskset','-c','16',str(out/f'c-abi{suffix}')],
        ]
        with (out/f'c-abi{suffix}.log').open('w') as log:
            for command in commands:
                wait_for_load()
                subprocess.run([str(ROOT/'tools/capped'),*command],cwd=ROOT,env=env,stdout=log,stderr=subprocess.STDOUT,check=True)
            log.write(f'C ABI {allocator} smoke: 1 passed\n')


def sample(out, allocator, workload, mode, env):
    directory = out / f'{allocator}-{workload}-{mode}'
    directory.mkdir()
    perf, stats = directory/'perf.csv', directory/'time.tsv'
    command = [str(ROOT/'tools/capped'), 'taskset', '-c', '16', 'perf', 'stat',
               '-x', ';', '-e', 'instructions:u,duration_time', '-o', str(perf),
               '--', '/usr/bin/time', '-f', '%e\t%M', '-o', str(stats),
               str(out/f'memory-{allocator}'), workload, mode]
    started = time.monotonic()
    result = subprocess.run(command, cwd=ROOT, env=env, capture_output=True, text=True)
    (directory/'stdout').write_text(result.stdout)
    (directory/'stderr').write_text(result.stderr)
    row = dict(allocator=allocator, workload=workload, mode=mode, status='ok',
               instructions='', seconds='', max_rss_kib='', tree_peak_mib='', wrapper_seconds=f'{time.monotonic()-started:.6f}')
    if result.returncode or result.stdout.strip() != str(expected(workload)):
        row['status'] = f'exit {result.returncode}' if result.returncode else 'wrong output'
    if perf.exists():
        for line in perf.read_text().splitlines():
            fields = line.split(';')
            if len(fields) > 2 and fields[0].strip().isdigit():
                if fields[2] == 'instructions:u':
                    row['instructions'] = fields[0].strip()
                elif fields[2] == 'duration_time':
                    row['seconds'] = f'{int(fields[0])/1e9:.6f}'
    if stats.exists():
        fields = stats.read_text().strip().split('\t')
        if len(fields) == 2:
            row['max_rss_kib'] = fields[1]
    if row['status'] == 'ok' and not row['instructions']:
        row['status'] = 'counter unavailable'
    peak = re.search(r'capped: peak (\d+) MiB of', result.stderr)
    row['tree_peak_mib'] = peak[1] if peak else ''
    return row


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--abi-only', action='store_true', help='compile and run the public C header fixture instead of cost samples')
    parser.add_argument('--workloads', nargs='+', choices=WORKLOADS, default=WORKLOADS)
    parser.add_argument('--modes', nargs='+', help='sample only these modes')
    args = parser.parse_args()
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=True)
    env = os.environ | dict(HIRSEL_MEMORY_LIMIT='2G', HIRSEL_JOBS='1',
                            XDG_RUNTIME_DIR=f'/run/user/{os.getuid()}',
                            DBUS_SESSION_BUS_ADDRESS=f'unix:path=/run/user/{os.getuid()}/bus')
    build(out, env)
    if args.abi_only:
        abi_smoke(out, env)
        return
    table = out/'samples.tsv'
    rows = list(csv.DictReader(table.open(), delimiter='\t')) if table.exists() else []
    done = {(r['allocator'], r['workload'], r['mode']) for r in rows}
    fields = ['allocator', 'workload', 'mode', 'status', 'instructions', 'seconds', 'max_rss_kib', 'tree_peak_mib', 'wrapper_seconds']
    with table.open('a') as stream:
        writer = csv.DictWriter(stream, fieldnames=fields, delimiter='\t')
        if not rows:
            writer.writeheader()
        for allocator in ['system', 'mimalloc']:
            for workload in args.workloads:
                for mode in ['rust', 'runtime'] + (['matched', 'wrapped'] if workload == 'growth' else ['matched', 'inline', 'current', 'different'] if workload.startswith('alloc') else ['inline'] if workload == 'region' else ['full', 'query', 'full_alloc', 'alloc_control', 'c_full', 'c_full_alloc'] if workload == 'limits' else []):
                    if args.modes and mode not in args.modes:
                        continue
                    if (allocator, workload, mode) in done:
                        continue
                    row = sample(out, allocator, workload, mode, env)
                    writer.writerow(row)
                    stream.flush()
                    print(row, flush=True)


if __name__ == '__main__':
    main()
