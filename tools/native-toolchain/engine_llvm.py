#!/usr/bin/env python3
"""Optimised builds through a given libLLVM.so.23.1: the engine library's training run and its guard.

    engine_llvm.py train LIBDIR OUT --artifacts JSON
        one optimised build of each training program through LIBDIR's library, so an
        IR-instrumented library writes its profile (build-libllvm's train stage)
    engine_llvm.py guard STOCK CANDIDATE OUT --artifacts JSON [--previous-warm RATIO]
        every training and held-out program through both libraries; passes only if each
        program's .text is byte-identical across them and the candidate's warm build cost,
        as a geomean over programs of its ratio to the stock library's, is below 1 and no
        worse than RATIO, the previous pin's (llvm-23.lock's engine_llvm_guard record)

JSON is a core/tools/measurement_artifacts.py identity: the compiler and runtime archive
every build uses. Training programs: the LangArena suite, every M1 program and the
documents layout family. Held-out programs, which the profile never sees: the graph and
json layout families and the M2b table and dispatch_cross_package workloads.

Each guard program has its own XDG cache and three builds 3 s apart, each with a fresh
store. Cold is build 1, which also compiles core and std into the cache. Warm is build 3:
core and std cached, which is an installed compiler's ordinary build. One sample each:
process-tree instructions:u on one CPU (HIRSEL_MEASURE_CPU, default 16).

Each library is loaded through a shadow build tree. The compiler is six directories
deep, beside an llvm-files view whose engine/ holds the given library and whose lib/
is the pinned tree's. This diagnostic run supplies the engine's expected build ID.
"""
import argparse
import hashlib
import importlib.util
import json
import math
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[2]
sys.path[:0] = [str(ROOT / 'core/tools'), str(ROOT / 'core/langarena/layout')]
import langarena  # noqa: E402
import m1  # noqa: E402
import m2b  # noqa: E402
import service_build  # noqa: E402

# core/langarena/run.py by path: service_build imports the layout directory's own run module.
_spec = importlib.util.spec_from_file_location('arena_run', ROOT / 'core/langarena/run.py')
arena_run = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(arena_run)

PIN = ROOT / 'tools/buck2/toolchains/llvm-files/lib'
CC = ROOT / 'tools/buck2/toolchains/llvm-files/bin/hirsel-clang'
CPU = os.environ.get('HIRSEL_MEASURE_CPU', '16')


def training(out):
    """(tag, build arguments) for each training program; inputs are written under OUT."""
    jobs = []
    host = out / 'arena-host'
    host.mkdir()
    shutil.copyfile(ROOT / 'core/langarena/rust/run.js', host / 'config.json')
    spec = langarena.inputs(ROOT, 'suite')
    suite = out / 'suite.hirsel'
    suite.write_text(arena_run.suite_source())
    spec['packages'][-1]['sources'] = [s for s in spec['packages'][-1]['sources'] if len(s['modules']) == 1]
    spec['packages'][-1]['sources'].append({'file': str(suite), 'modules': ['suite'], 'signatures': False})
    (out / 'arena.json').write_text(json.dumps(spec))
    jobs.append(('arena', ['--inputs', str(out / 'arena.json'), '--entry', 'langarena@1/suite/main', '--dir', str(host),
                           '--grant', 'Stdout', '--grant', 'Tasks', '--limit', '1073741824']))
    manifest = m1.load_manifest(ROOT / 'core/programs/m1/manifest.json')
    for e in manifest['entries']:
        tag = f"{e['workload']}-{e['variant']}"
        file = out / f'{tag}.json'
        file.write_text(json.dumps(m1.inputs_for(manifest, e, ROOT)))
        jobs.append((tag, ['--inputs', str(file), '--entry', f"{e['package']['name']}@1/{e['module']}/{e['function']}",
                           *m1.grant_args(e)]))
    jobs.append(layout(out, 'documents'))
    return jobs


def layout(out, group):
    """A layout family as service_build.py builds it, against the core image."""
    folder = out / f'layout-{group}'
    folder.mkdir()
    _, _, file, _, _, _ = service_build.source_sets(argparse.Namespace(out=folder), group)
    extra = ['--dir', str(ROOT / 'core/langarena/layout/inputs')] if group == 'json' else []
    return (f'layout-{group}', ['--inputs', str(file), '--entry', f'layout@1/{group}/build_family', '--grant', 'Stdout', *extra])


def held_out(out):
    jobs = [layout(out, 'graph'), layout(out, 'json')]
    manifest = json.loads((ROOT / 'core/programs/m2b/manifest.json').read_text())
    for workload in ['table', 'dispatch_cross_package']:
        folder = out / f'm2b-{workload}'
        folder.mkdir()
        entry, selected = m2b.inputs(ROOT, manifest, workload)
        main = next(Path(s['file']) for p in selected['packages'] if p['package'] == entry['package']
                    for s in p['sources'] if entry['module'] in s['modules'])
        cases = ROOT / 'core/corpus/m2b' / workload / 'cases.json'
        names = [c['name'] for c in json.loads(cases.read_text())] if cases.exists() else []
        probe = folder / 'probe.hirsel'
        probe.write_text(m2b.probe_source(main.read_text(), entry['package']['name'], names))
        selected['packages'].append({'package': {'name': m2b.PROBE, 'version': '1'},
                                     'dependencies': [{'name': 'core', 'version': '1'}, {'name': 'std', 'version': '1'}, entry['package']],
                                     'sources': [{'file': str(probe), 'modules': ['main'], 'signatures': False}]})
        file = folder / 'inputs.json'
        file.write_text(json.dumps(selected))
        grants = [x for g in entry.get('grants', []) for x in ('--grant', g)]
        jobs.append((f'm2b-{workload}', ['--inputs', str(file), '--entry', f'{m2b.PROBE}@1/main/main', '--grant', 'Stdout', *grants]))
    return jobs


def shadow(library, compiler, out):
    """A compiler copy whose build-tree engine is LIBRARY's libLLVM.so.23.1."""
    tree = out / 'tools/buck2/toolchains/llvm-files'
    (tree / 'engine').mkdir(parents=True)
    shutil.copyfile(library / 'libLLVM.so.23.1', tree / 'engine/libLLVM.so.23.1')
    (tree / 'lib').symlink_to(PIN.resolve())
    copy = out / 'a/b/c/d/e/f/hirsel'
    copy.parent.mkdir(parents=True)
    shutil.copy2(compiler, copy)
    return copy


def build(compiler, runtime, args, out, xdg, sample):
    out.mkdir(parents=True)
    engine = compiler.parents[6] / 'tools/buck2/toolchains/llvm-files/engine/libLLVM.so.23.1'
    notes = subprocess.check_output(['readelf', '-n', engine], text=True)
    [build_id] = [line.split()[-1] for line in notes.splitlines() if 'Build ID:' in line]
    env = os.environ | dict(HIRSEL_MEMORY_LIMIT='2G', HIRSEL_JOBS='1', LD_LIBRARY_PATH=str(PIN),
                            HIRSEL_LLVM_EXPERIMENT_BUILD_ID=build_id,
                            XDG_CACHE_HOME=str(xdg / 'cache'), XDG_DATA_HOME=str(xdg / 'data'))
    pre = ['taskset', '-c', CPU, 'perf', 'stat', '-x', ';', '-e', 'instructions:u', '-o', str(out / 'perf'), '--'] if sample else []
    command = pre + [str(compiler), 'build', *args, '--backend', 'llvm', '--release', '--runtime', str(runtime),
                     '--cc', str(CC), '--store', str(out / 'store'), '--json', '-o', str(out / 'exe')]
    result = subprocess.run(command, capture_output=True, env=env, cwd=ROOT)
    if result.returncode:
        raise SystemExit(f'engine_llvm: {out.name} failed to build:\n{result.stdout[-1500:]!r}\n{result.stderr[-1500:]!r}')
    if not sample:
        return None
    subprocess.run(['objcopy', '--dump-section', f".text={out / 'text'}", str(out / 'exe')], check=True)
    instructions = next(int(line.split(';')[0]) for line in (out / 'perf').read_text().splitlines() if 'instructions:u' in line)
    return dict(build=instructions, text=hashlib.sha256((out / 'text').read_bytes()).hexdigest())


def measure(compiler, runtime, jobs, out):
    results = {}
    for tag, args in jobs:
        xdg = out / 'xdg' / tag
        cold = build(compiler, runtime, args, out / f'{tag}.1', xdg, True)
        time.sleep(3)
        build(compiler, runtime, args, out / f'{tag}.2', xdg, False)
        time.sleep(3)
        warm = build(compiler, runtime, args, out / f'{tag}.3', xdg, True)
        results[tag] = dict(warm=warm['build'], cold=cold['build'], text=warm['text'])
        print(tag, results[tag], flush=True)
    return results


def geomean(xs):
    return math.exp(sum(map(math.log, xs)) / len(xs))


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest='command', required=True)
    t = sub.add_parser('train')
    t.add_argument('library', type=Path)
    t.add_argument('out', type=Path)
    g = sub.add_parser('guard')
    g.add_argument('stock', type=Path)
    g.add_argument('candidate', type=Path)
    g.add_argument('out', type=Path)
    g.add_argument('--previous-warm', type=float)
    for p in (t, g):
        p.add_argument('--artifacts', type=Path, required=True)
    args = parser.parse_args()
    identity = json.loads(args.artifacts.read_text())
    compiler, runtime = Path(identity['compiler']), Path(identity['runtime'])
    shutil.rmtree(args.out, ignore_errors=True)
    args.out.mkdir(parents=True)
    inputs = args.out / 'inputs'
    inputs.mkdir()
    if args.command == 'train':
        exe = shadow(args.library, compiler, args.out / 'shadow')
        for tag, build_args in training(inputs):
            build(exe, runtime, build_args, args.out / tag, args.out / 'xdg', False)
        return 0
    jobs = training(inputs) + held_out(inputs)
    sides = {}
    for side, library in (('stock', args.stock), ('candidate', args.candidate)):
        exe = shadow(library, compiler, args.out / side / 'shadow')
        sides[side] = measure(exe, runtime, jobs, args.out / side)
    stock, candidate = sides['stock'], sides['candidate']
    report = dict(programs=sides, differing=[t for t in stock if stock[t]['text'] != candidate[t]['text']],
                  warm=geomean([candidate[t]['warm'] / stock[t]['warm'] for t in stock]),
                  cold=geomean([candidate[t]['cold'] / stock[t]['cold'] for t in stock]))
    failures = [f'.text differs on {t}' for t in report['differing']]
    if report['warm'] >= 1:
        failures.append(f"warm build cost geomean {report['warm']:.3f} is not below the stock library's")
    if args.previous_warm is not None and report['warm'] > args.previous_warm:
        failures.append(f"warm build cost geomean {report['warm']:.4f} is worse than the previous pin's {args.previous_warm:.4f}")
    (args.out / 'guard.json').write_text(json.dumps(report, indent=1))
    print(f"engine_llvm guard: warm {report['warm']:.3f}, cold {report['cold']:.3f}, "
          f"{len(stock) - len(report['differing'])}/{len(stock)} .text identical")
    for failure in failures:
        print('engine_llvm guard: FAIL', failure, file=sys.stderr)
    return 1 if failures else 0


if __name__ == '__main__':
    sys.exit(main())
