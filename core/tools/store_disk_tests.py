#!/usr/bin/env python3
"""Run the store's S2 laws from checkout sources with exact body selection."""
import argparse
import contextlib
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile

from std_native import ROOT, Evaluator
from store_tests import inputs
from store_crash_tests import run_crashes

# Evaluator instructions measured on the unchanged stress law, plus 25% rounded up.
INSTRUCTION_ALLOWANCES = {'pack_bound': 1_622_341_249}  # measured 1_297_872_999

LAWS = ('frames', 'manifest', 'nodes', 'graph', 'packs', 'load', 'compact',
        'draft_refusal', 'worker_refusal', 'opening', 'db', 'db_refusal', 'schema_refusal', 'publish_refusal', 'pack_bound', 'branches', 'migrate')
NATIVE_TESTS = {'store@1/store/disk_test_' + name for name in LAWS if name != 'pack_bound'}

# Orchestrator ruling: these evaluator assembly defects ride with owner914.
# Execute and report every law; only this exact failure family is nonblocking.
ASSEMBLY_OWNER = 'core-ops-914'
KNOWN_ASSEMBLY = frozenset({'compact', 'worker_refusal', 'opening', 'db',
                            'db_refusal', 'schema_refusal', 'pack_bound'})


def known_assembly(row):
    return (row.get('engine') == 'evaluator' and row.get('law') in KNOWN_ASSEMBLY
            and re.fullmatch(r'not well formed: f[0-9]+: a[0-9]+: base is not a pointer',
                             row.get('result', {}).get('error', '')) is not None)


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--root', type=Path, default=ROOT)
    p.add_argument('--compiler', type=Path)
    p.add_argument('--evaluator', type=Path)
    p.add_argument('--runtime', type=Path, default=os.environ.get('HIRSEL_CORE_RT_ARCHIVE'))
    p.add_argument('--cc', type=Path)
    p.add_argument('--bridge', type=Path, default=os.environ.get('HIRSEL_STORE_CRASH_BRIDGE'))
    p.add_argument('--backend', action='append', choices=['evaluator', 'cranelift', 'llvm', 'llvm-release'])
    p.add_argument('--report', type=Path)
    p.add_argument('--test-threads', type=int, choices=[1], default=1)
    args = p.parse_args()
    rows = []
    with tempfile.TemporaryDirectory(prefix='hirsel-store-disk-laws-') as tmp, contextlib.ExitStack() as resources:
        work = Path(tmp)
        manifest = work / 'inputs.json'
        manifest.write_text(json.dumps(inputs(args.root.resolve())))
        evaluator = resources.enter_context(Evaluator(args.evaluator.resolve())) if args.evaluator else None
        for engine in args.backend or ['evaluator', 'cranelift', 'llvm', 'llvm-release']:
            if engine == 'evaluator':
                if args.evaluator is None:
                    p.error('evaluator backend requires --evaluator')
                for name in LAWS:
                    result = evaluator.evaluate(manifest, 'store@1/store/disk_law_' + name,
                                                instructions=INSTRUCTION_ALLOWANCES.get(name))
                    try:
                        report = json.loads(result.stdout)
                    except ValueError:
                        report = {'stdout': result.stdout.decode(errors='replace'), 'stderr': result.stderr.decode(errors='replace')}
                    good = result.returncode == 0 and report.get('outcome') == {'kind': 'returned', 'values': [1]}
                    rows.append(dict(engine=engine, law=name, passed=good, result=report))
                    print(f'{engine} {name}: {"PASS" if good else "FAIL"}', flush=True)
            else:
                if args.compiler is None or args.runtime is None or args.bridge is None:
                    p.error('native backends require --compiler, --runtime and --bridge')
                host = work / (engine + '-host')
                host.mkdir()
                command = [str(args.compiler.resolve()), 'test', '--inputs', str(manifest),
                           '--source-core', '--source-std', '--backend', 'llvm' if engine == 'llvm-release' else engine,
                           '--runtime', str(args.runtime.resolve()), '--store', str(work / engine), '--json',
                           '--filter', 'store@1/store/disk_test_', '--dir', str(host), '--limit', str(64 * 1024 * 1024),
                           '--grant', 'Clock', '--grant', 'Random', '--grant', 'Tasks', '--grant', 'Stderr']
                if engine == 'llvm-release':
                    command.append('--release')
                if args.cc:
                    command += ['--cc', str(args.cc.resolve())]
                result = subprocess.run(command, capture_output=True, text=True)
                try:
                    report = json.loads(result.stdout)
                except ValueError:
                    report = {'stdout': result.stdout, 'stderr': result.stderr}
                tests = report.get('tests', [])
                good = (result.returncode == 0 and len(tests) == len(NATIVE_TESTS)
                        and {t['definition'] for t in tests} == NATIVE_TESTS and all(t.get('passed') for t in tests))
                rows.append(dict(engine=engine, passed=good, result=report))
                print(f'{engine}: {sum(bool(t.get("passed")) for t in tests)}/{len(NATIVE_TESTS)} passed', flush=True)
                # New L25 case keeps36MiB of rows plus a40MiB pack and the
                # default262MiB child reservation; other laws retain64MiB.
                large = list(command)
                large[large.index('--filter') + 1] = 'store@1/store/disk_pack_bound'
                large[large.index('--limit') + 1] = str(512 * 1024 * 1024)
                result = subprocess.run(large, capture_output=True, text=True)
                try:
                    report = json.loads(result.stdout)
                except ValueError:
                    report = {'stdout': result.stdout, 'stderr': result.stderr}
                tests = report.get('tests', [])
                good = (result.returncode == 0 and len(tests) == 1
                        and tests[0].get('definition') == 'store@1/store/disk_pack_bound'
                        and tests[0].get('passed'))
                rows.append(dict(engine=engine, law='pack_bound', passed=good, result=report))
                print(f'{engine} pack_bound: {"PASS" if good else "FAIL"}', flush=True)
                try:
                    cuts = run_crashes(args.root.resolve(), args.compiler, args.runtime, args.bridge,
                                       args.cc, manifest, work, engine)
                    rows.append(dict(engine=engine, passed=True, crashes=cuts))
                except RuntimeError as error:
                    rows.append(dict(engine=engine, passed=False, crash_failure=str(error)))
    failures = [row for row in rows if not row['passed']]
    for row in failures:
        if known_assembly(row):
            row['known_failure_owner'] = ASSEMBLY_OWNER
    if args.report:
        args.report.write_text(json.dumps(rows, indent=2) + '\n')
    if failures:
        print(json.dumps(failures, indent=2))
    known = sum(known_assembly(row) for row in failures)
    if known:
        print(f'{known} known evaluator assembly failures ride; owner {ASSEMBLY_OWNER}', flush=True)
    return int(any(not known_assembly(row) for row in failures))


if __name__ == '__main__':
    raise SystemExit(main())
