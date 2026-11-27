#!/usr/bin/env python3
"""Ownership and capability soundness fuzzer for Hirsel.

Runs two mandatory verification layers:
1. Curated negative-control corpus: asserts every known-unsound pattern across aliasing,
   move, escape, capability, and Held/unit surfaces is rejected by `hirsel check` with
   the expected soundness rule (no silent acceptance, no vacuous parse error).
2. Seeded multi-feature & unwinding campaign + 4-way oracle:
   - Frontend checker (`hirsel check --json`): rejects provably unsound mutations with
     their expected rule family; accepts ~80% complex multi-feature/unwinding programs.
   - CoreIR well-formedness & ownership verifier (`hirsel_core_ir::wf::module`, including
     `wf::own` R1 double-drop, R2 use-after-drop, R3 view-drop) via `hirsel-core-contract-eval`.
   - 3-engine differential execution: Evaluator + Cranelift + LLVM (+ optional ASan
     native runtime via `--sanitized-runtime`) asserting identical outcomes and zero crashes.

Example:
  ownership_soundness_fuzz.py controls --compiler hirsel --out /tmp/own
  ownership_soundness_fuzz.py campaign --seeds 0..240 --builds evaluator,cranelift-dev,llvm-dev \\
      --compiler hirsel --runtime-archive libhirsel_core_rt.a \\
      --evaluator hirsel_core_contract_eval --out /tmp/own
  ownership_soundness_fuzz.py nightly --count 240 --cpu-seconds 600 --base-seed 1 \\
      --alert alert.txt ... --out /tmp/own
"""
import argparse
from collections import Counter
from concurrent.futures import ThreadPoolExecutor
import json
import os
from pathlib import Path
import resource
import shutil
import subprocess
import sys
import threading
import time

sys.path.insert(0, str(Path(__file__).resolve().parent))
from contract_exec import Evaluator, library_sources, native_build  # noqa: E402
from ownership_soundness_gen import (  # noqa: E402
    FUTURE_SURFACES,
    NEGATIVE_CONTROLS,
    SURFACES,
    VERSION,
    GeneratedProgram,
    NegativeControl,
    generate_program,
    render_batch,
)

ROOT = Path(__file__).resolve().parents[2]
ROOT_LIMIT = 1 << 27
RUN_SECONDS = 120
NATIVE = {
    'cranelift-dev': ('cranelift', False),
    'llvm-dev': ('llvm', False),
    'llvm-release': ('llvm', True),
}
BUILDS = ('evaluator', *NATIVE)
VACUOUS_RULES = {'LANG-1', 'LANG-2', 'LANG-3', 'LANG-5', 'LANG-7'}
DEFAULT_BATCH_SIZE = 16
SHRINK_MAX_RUNS = 60


def rule_family(rule: str) -> str:
    return rule.split('-', 1)[0] if '-' in rule else rule


def manifest(root: Path, source: Path) -> dict:
    core = {'name': 'core', 'version': '1'}
    pkg = {'name': 'gen', 'version': '1'}
    return {
        'core': core,
        'packages': [
            {'package': core, 'sources': library_sources(root / 'core/core', True), 'dependencies': []},
            {'package': pkg, 'sources': [{'file': str(source.resolve()), 'modules': ['main'], 'signatures': False}],
             'dependencies': [core]},
        ],
    }


def check_source(compiler: Path, root: Path, work: Path, store: Path, source: str,
                 entry: str | None = None) -> dict:
    work.mkdir(parents=True, exist_ok=True)
    src_file = work / 'main.hirsel'
    src_file.write_text(source)
    inp_file = work / 'inputs.json'
    inp_file.write_text(json.dumps(manifest(root, src_file)))
    cmd = [str(compiler), 'check', '--source-core', '--inputs', str(inp_file),
           '--store', str(store), '--json']
    if entry:
        cmd.extend(['--entry', entry])
    try:
        proc = subprocess.run(cmd, capture_output=True, text=True, timeout=RUN_SECONDS)
    except subprocess.TimeoutExpired:
        return {'status': 'timeout', 'accepted': False, 'rules': [], 'families': [], 'diagnostics': []}
    if proc.returncode < 0 or proc.returncode > 128:
        return {'status': f'signal:{proc.returncode}', 'accepted': False, 'rules': [],
                'families': [], 'diagnostics': [], 'stderr': proc.stderr[-400:]}
    try:
        report = json.loads(proc.stdout)
    except ValueError:
        return {'status': f'bad-json:{proc.returncode}', 'accepted': False, 'rules': [],
                'families': [], 'diagnostics': [], 'stderr': (proc.stdout + proc.stderr)[-400:]}
    diags = report.get('diagnostics', [])
    rules = [d.get('rule', 'UNKNOWN') for d in diags]
    families = sorted({rule_family(r) for r in rules})
    return {
        'status': 'ok',
        'accepted': bool(report.get('accepted')),
        'rules': rules,
        'families': families,
        'diagnostics': diags,
        'stopped': report.get('stopped'),
        'incomplete': report.get('incomplete', []),
    }


def parse_case_records(stdout: bytes) -> dict[int, int]:
    found = {}
    for line in stdout.decode(errors='replace').splitlines():
        parts = line.split()
        if len(parts) == 3 and parts[0] == 'case':
            found[int(parts[1])] = int(parts[2])
    return found


class Quiet:
    """Suppress per-entry evaluator progress lines during batch runs."""

    def output(self, stream, chunk):
        pass

    def end_stream(self, stream):
        pass


def run_evaluator_case(resident: Evaluator, inputs: Path, entry: str) -> dict:
    """Run `hirsel-core-contract-eval`, which checks `hirsel_core_ir::wf::module` (incl. `wf::own` R1-R3)
    before executing the materialized entry on the evaluator."""
    res = resident.evaluate(inputs, entry, root_limit=ROOT_LIMIT, progress=Quiet())
    try:
        report = json.loads(res.stdout)
    except ValueError:
        return {'kind': 'harness', 'wf_checked': False, 'wf_ok': False,
                'detail': res.stderr.decode(errors='replace')[-400:]}
    if error := report.get('error'):
        err_text = str(error)
        if err_text.startswith('not well formed:'):
            return {'kind': 'ir-wf-own', 'wf_checked': True, 'wf_ok': False, 'detail': err_text[:500]}
        return {'kind': 'harness', 'wf_checked': False, 'wf_ok': False, 'detail': err_text[:400]}
    outcome = report.get('outcome', {})
    stdout = bytes(report.get('stdout', []))
    records = parse_case_records(stdout)
    if outcome.get('kind') == 'returned':
        vals = outcome.get('values') or []
        return {'kind': 'ok', 'wf_checked': True, 'wf_ok': True,
                'value': vals[0] if vals else None, 'records': records, 'cause': None}
    if outcome.get('kind') == 'failed':
        return {'kind': 'failed', 'wf_checked': True, 'wf_ok': True,
                'value': None, 'records': records, 'cause': outcome.get('cause')}
    if outcome.get('kind') == 'stopped':
        return {'kind': 'stopped', 'wf_checked': True, 'wf_ok': True,
                'name': outcome.get('name'), 'records': records}
    return {'kind': 'harness', 'wf_checked': True, 'wf_ok': True, 'detail': json.dumps(outcome)[:400]}


def run_native_case(binary: Path, argv: list[str], extra_env: dict | None = None) -> dict:
    env = dict(os.environ, **(extra_env or {}))
    try:
        proc = subprocess.run([str(binary), *argv], capture_output=True, timeout=RUN_SECONDS, env=env)
    except subprocess.TimeoutExpired:
        return {'kind': 'timeout', 'detail': f'over {RUN_SECONDS}s'}
    records = parse_case_records(proc.stdout)
    stderr_text = proc.stderr.decode(errors='replace')
    if proc.returncode == 0:
        return {'kind': 'ok', 'records': records, 'cause': None}
    if proc.returncode == 101:
        cause = stderr_text.strip().splitlines()[:1]
        return {'kind': 'failed', 'records': records, 'cause': cause[0] if cause else 'failed'}
    if proc.returncode == 102 or 'run stop' in stderr_text:
        return {'kind': 'stopped', 'name': stderr_text.strip()[-200:], 'records': records}
    if (proc.returncode < 0 or proc.returncode > 128
            or 'AddressSanitizer' in stderr_text or 'UndefinedBehaviorSanitizer' in stderr_text):
        return {'kind': 'crash', 'status': proc.returncode, 'detail': stderr_text[-500:]}
    return {'kind': 'exit', 'status': proc.returncode, 'detail': stderr_text[-400:], 'records': records}


def compare_engine_outcomes(idx: int, seed: int, surface: str, outcomes: dict[str, dict]) -> list[dict]:
    reds = []
    for build, out in outcomes.items():
        if out['kind'] in ('ir-wf-own', 'crash', 'stopped', 'timeout', 'harness', 'exit'):
            reds.append({
                'key': f'soundness:{out["kind"]}:{surface}:{build}',
                'seed': seed, 'surface': surface, 'build': build,
                'detail': str(out.get('detail') or out.get('name') or out)[:400],
            })
    if reds:
        return reds
    builds = list(outcomes.keys())
    ref_build = 'evaluator' if 'evaluator' in outcomes else builds[0]
    ref = outcomes[ref_build]
    ref_val = ref['records'].get(idx)
    if ref['kind'] == 'ok' and ref_val is None:
        reds.append({
            'key': f'soundness:missing-record:{surface}:{ref_build}',
            'seed': seed, 'surface': surface, 'build': ref_build,
            'detail': f'entry {idx} returned ok with no case output record',
        })
        return reds
    for build in builds:
        if build == ref_build:
            continue
        other = outcomes[build]
        if other['kind'] != ref['kind']:
            reds.append({
                'key': f'soundness:diverge-kind:{surface}:{build}',
                'seed': seed, 'surface': surface, 'build': build,
                'detail': f'{ref_build}={ref["kind"]} vs {build}={other["kind"]}',
            })
        elif other['records'].get(idx) != ref_val:
            reds.append({
                'key': f'soundness:diverge-value:{surface}:{build}',
                'seed': seed, 'surface': surface, 'build': build,
                'detail': f'{ref_build}={ref_val} vs {build}={other["records"].get(idx)}',
            })
    return reds


def thread_counter():
    n = 0
    lock = threading.Lock()
    while True:
        with lock:
            val = n
            n += 1
        yield val


def run_negative_controls(args, out_dir: Path) -> dict:
    ctrl_dir = out_dir / 'controls'
    shutil.rmtree(ctrl_dir, ignore_errors=True)
    ctrl_dir.mkdir(parents=True, exist_ok=True)
    local_state = threading.local()
    store_ids = thread_counter()

    def worker_store() -> Path:
        if not hasattr(local_state, 'store'):
            local_state.store = ctrl_dir / f'store-{next(store_ids)}'
        return local_state.store

    def eval_one(ctrl: NegativeControl) -> dict:
        res = check_source(args.compiler, args.root, ctrl_dir / ctrl.name, worker_store(), ctrl.source, entry=ctrl.entry)
        expected_set = set(ctrl.expected_rules)
        expected_fams = {rule_family(r) for r in ctrl.expected_rules}
        actual_rules = set(res['rules'])
        vacuous = sorted(actual_rules & VACUOUS_RULES - expected_set)
        matched = (not res['accepted']) and bool(actual_rules & expected_set) and not vacuous and res['status'] == 'ok'
        failure_reason = None
        if res['status'] != 'ok':
            failure_reason = f'checker {res["status"]}: {res.get("stderr", "")}'
        elif res['accepted']:
            failure_reason = f'UNSOUND ACCEPT: expected rejection in {sorted(expected_set)}'
        elif vacuous:
            failure_reason = f'vacuous rejection {vacuous} alongside {sorted(actual_rules)}'
        elif not (actual_rules & expected_set):
            failure_reason = f'wrong rule {sorted(actual_rules)} (expected {sorted(expected_set)} / {sorted(expected_fams)})'
        return {
            'name': ctrl.name, 'surface': ctrl.surface, 'passed': matched,
            'expected_rules': list(ctrl.expected_rules), 'actual_rules': res['rules'],
            'families': res['families'], 'reason': failure_reason,
        }

    with ThreadPoolExecutor(max_workers=args.jobs) as pool:
        items = list(pool.map(eval_one, NEGATIVE_CONTROLS))
    for store_path in ctrl_dir.glob('store-*'):
        shutil.rmtree(store_path, ignore_errors=True)
    passed = sum(1 for i in items if i['passed'])
    failed = [i for i in items if not i['passed']]
    by_family = Counter()
    for item in items:
        for fam in item['families']:
            by_family[fam] += 1
    return {'total': len(items), 'passed': passed, 'failed': failed,
            'by_family': dict(sorted(by_family.items())), 'items': items}


def execute_accepted_batch(args, batch: list[GeneratedProgram], batch_dir: Path,
                           store_root: Path) -> tuple[dict, dict, list[dict]]:
    """Run accepted programs across IR wf::own + Evaluator + Cranelift + LLVM (+ optional ASan)."""
    batch_dir.mkdir(parents=True, exist_ok=True)
    src_file = batch_dir / 'main.hirsel'
    src_file.write_text(render_batch(batch))
    inp_file = batch_dir / 'inputs.json'
    inp_file.write_text(json.dumps(manifest(args.root, src_file)))

    per_entry: list[dict[str, dict]] = [{} for _ in batch]
    reds: list[dict] = []
    executed_counts = Counter()
    wf_counts = Counter()

    if 'evaluator' in args.builds:
        with Evaluator(args.evaluator) as resident:
            for idx, prog in enumerate(batch):
                ev_out = run_evaluator_case(resident, inp_file, f'gen@1/main/case_{idx}')
                per_entry[idx]['evaluator'] = ev_out
                executed_counts['evaluator'] += 1
                if ev_out.get('wf_checked'):
                    wf_counts['checked'] += 1
                    wf_counts['passed' if ev_out.get('wf_ok') else 'failed'] += 1

    native_arms: list[tuple[str, str, bool, Path, dict | None]] = []
    for build in args.builds:
        if build in NATIVE:
            backend, release = NATIVE[build]
            native_arms.append((build, backend, release, args.runtime, None))
    if args.sanitized_runtime:
        asan_env = {'ASAN_OPTIONS': 'detect_leaks=1:abort_on_error=1:halt_on_error=1'}
        native_arms.append(('sanitized-cranelift', 'cranelift', False, args.sanitized_runtime, asan_env))

    grants = ['Stdout', 'Tasks']
    for arm_name, backend, release, runtime_path, extra_env in native_arms:
        binary = batch_dir / f'{arm_name}.bin'
        built = native_build(
            args.compiler, runtime_path, inp_file, 'gen@1/main/main', backend,
            store_root / arm_name, binary, limit=ROOT_LIMIT, release=release,
            grants=grants, cc=args.cc,
        )
        if built.returncode != 0 or not binary.exists():
            if len(batch) > 1:
                for prog in batch:
                    sc, swf, sr = execute_accepted_single(args, prog, batch_dir / f'single-{prog.seed}', store_root)
                    executed_counts.update(sc)
                    wf_counts.update(swf)
                    reds.extend(sr)
                return dict(executed_counts), dict(wf_counts), reds
            err_text = (built.stdout + built.stderr).decode(errors='replace')[-600:]
            reds.append({
                'key': f'soundness:build:{batch[0].surface}:{arm_name}',
                'seed': batch[0].seed, 'surface': batch[0].surface, 'build': arm_name,
                'detail': err_text,
            })
            continue
        for idx in range(len(batch)):
            per_entry[idx][arm_name] = run_native_case(binary, [str(idx)], extra_env=extra_env)
            executed_counts[arm_name] += 1
        binary.unlink(missing_ok=True)

    for idx, prog in enumerate(batch):
        reds.extend(compare_engine_outcomes(idx, prog.seed, prog.surface, per_entry[idx]))
    return dict(executed_counts), dict(wf_counts), reds


def execute_accepted_single(args, prog: GeneratedProgram, work: Path,
                            store_root: Path) -> tuple[dict, dict, list[dict]]:
    work.mkdir(parents=True, exist_ok=True)
    src_file = work / 'main.hirsel'
    src_file.write_text(prog.render_standalone())
    inp_file = work / 'inputs.json'
    inp_file.write_text(json.dumps(manifest(args.root, src_file)))
    outcomes: dict[str, dict] = {}
    counts = Counter()
    wf_counts = Counter()
    reds = []
    if 'evaluator' in args.builds:
        with Evaluator(args.evaluator) as resident:
            ev_out = run_evaluator_case(resident, inp_file, 'gen@1/main/case_0')
            outcomes['evaluator'] = ev_out
            counts['evaluator'] += 1
            if ev_out.get('wf_checked'):
                wf_counts['checked'] += 1
                wf_counts['passed' if ev_out.get('wf_ok') else 'failed'] += 1
    arms = [(b, NATIVE[b][0], NATIVE[b][1], args.runtime, None) for b in args.builds if b in NATIVE]
    if args.sanitized_runtime:
        arms.append(('sanitized-cranelift', 'cranelift', False, args.sanitized_runtime,
                     {'ASAN_OPTIONS': 'detect_leaks=1:abort_on_error=1:halt_on_error=1'}))
    for arm_name, backend, release, runtime_path, extra_env in arms:
        binary = work / f'{arm_name}.bin'
        built = native_build(
            args.compiler, runtime_path, inp_file, 'gen@1/main/main', backend,
            store_root / arm_name, binary, limit=ROOT_LIMIT, release=release,
            grants=['Stdout', 'Tasks'], cc=args.cc,
        )
        if built.returncode != 0 or not binary.exists():
            reds.append({
                'key': f'soundness:build:{prog.surface}:{arm_name}',
                'seed': prog.seed, 'surface': prog.surface, 'build': arm_name,
                'detail': (built.stdout + built.stderr).decode(errors='replace')[-600:],
            })
            continue
        outcomes[arm_name] = run_native_case(binary, [], extra_env=extra_env)
        counts[arm_name] += 1
        binary.unlink(missing_ok=True)
    if outcomes:
        reds.extend(compare_engine_outcomes(0, prog.seed, prog.surface, outcomes))
    return dict(counts), dict(wf_counts), reds


def shrink_failure(args, prog: GeneratedProgram, target_key: str, out_dir: Path) -> dict:
    """Two-stage reducer: choice-stream lowering, then statement deletion."""
    work = out_dir / f'seed-{prog.seed}'
    shutil.rmtree(work, ignore_errors=True)
    work.mkdir(parents=True, exist_ok=True)
    store = work / 'store'
    runs = 0

    def reproduces(candidate: GeneratedProgram) -> bool:
        nonlocal runs
        if runs >= SHRINK_MAX_RUNS:
            return False
        runs += 1
        chk = check_source(args.compiler, args.root, work / 'chk', store, candidate.render_standalone())
        if target_key.startswith('soundness:unsound-accept:'):
            return chk['status'] == 'ok' and chk['accepted']
        if target_key.startswith('soundness:frontend-'):
            return chk['status'] != 'ok'
        if not chk['accepted']:
            return False
        _, _, reds = execute_accepted_single(args, candidate, work / 'run', store)
        return any(r['key'] == target_key for r in reds)

    best = prog
    if best.draws:
        draws = list(best.draws)
        for i in range(len(draws)):
            if runs >= SHRINK_MAX_RUNS:
                break
            if draws[i] == 0:
                continue
            trial_draws = list(draws)
            trial_draws[i] = 0
            cand = generate_program(best.seed, draws=trial_draws, forced_surface=best.surface, forced_kind=best.kind)
            if reproduces(cand):
                draws = list(cand.draws)
                best = cand
    changed = True
    while changed and len(best.stmts) > 1 and runs < SHRINK_MAX_RUNS:
        changed = False
        for i in range(len(best.stmts)):
            if runs >= SHRINK_MAX_RUNS:
                break
            cand = GeneratedProgram(
                seed=best.seed, surface=best.surface, kind=best.kind,
                mutation=best.mutation, expected_rules=best.expected_rules,
                stmts=best.stmts[:i] + best.stmts[i + 1:],
                draws=best.draws, bounds=best.bounds,
            )
            if reproduces(cand):
                best = cand
                changed = True
                break
    min_path = work / 'minimized.hirsel'
    min_path.write_text(best.render_standalone())
    shutil.rmtree(store, ignore_errors=True)
    return {'path': str(min_path), 'stmts': len(best.stmts), 'runs': runs}


def parse_seeds(seeds_spec: str | None, base_seed: int, count: int) -> list[int]:
    if not seeds_spec:
        return list(range(base_seed, base_seed + count))
    if '..' in seeds_spec:
        low_s, high_s = seeds_spec.split('..', 1)
        return list(range(int(low_s), int(high_s)))
    if ',' in seeds_spec:
        return [int(x.strip()) for x in seeds_spec.split(',') if x.strip()]
    return [int(seeds_spec)]


def child_cpu() -> float:
    u = resource.getrusage(resource.RUSAGE_CHILDREN)
    return u.ru_utime + u.ru_stime


def run_campaign(args, seeds: list[int], cpu_budget: float | None = None) -> dict:
    started_wall = time.monotonic()
    started_cpu = child_cpu()
    out_dir = args.out
    out_dir.mkdir(parents=True, exist_ok=True)

    controls = run_negative_controls(args, out_dir)
    reds: list[dict] = [
        {'key': f'soundness:control:{fail["surface"]}:{fail["name"]}', 'seed': None,
         'surface': fail['surface'], 'build': 'frontend', 'detail': fail['reason']}
        for fail in controls['failed']
    ]

    work_dir = out_dir / 'campaign'
    shutil.rmtree(work_dir, ignore_errors=True)
    work_dir.mkdir(parents=True, exist_ok=True)
    local_state = threading.local()
    store_ids = thread_counter()

    def worker_store() -> Path:
        if not hasattr(local_state, 'store'):
            local_state.store = work_dir / f'check-store-{next(store_ids)}'
        return local_state.store

    def check_seed(seed: int) -> tuple[GeneratedProgram, dict]:
        prog = generate_program(seed, forced_surface=args.surface)
        res = check_source(args.compiler, args.root, work_dir / f's-{seed}', worker_store(), prog.render_standalone())
        return prog, res

    generated_programs: list[GeneratedProgram] = []
    accepted_programs: list[GeneratedProgram] = []
    rejected_count = 0
    rejected_by_family: Counter = Counter()
    rejected_by_rule: Counter = Counter()
    surface_counts: Counter = Counter()
    kind_counts: Counter = Counter()
    progs_by_seed: dict[int, GeneratedProgram] = {}

    chunk_size = max(args.jobs * 4, 16)
    for offset in range(0, len(seeds), chunk_size):
        if cpu_budget is not None and (child_cpu() - started_cpu) >= cpu_budget * 0.70:
            break
        chunk = seeds[offset:offset + chunk_size]
        with ThreadPoolExecutor(max_workers=args.jobs) as pool:
            checked = list(pool.map(check_seed, chunk))
        for prog, res in checked:
            generated_programs.append(prog)
            progs_by_seed[prog.seed] = prog
            surface_counts[prog.surface] += 1
            kind_counts[prog.kind] += 1
            if res['status'] != 'ok':
                reds.append({
                    'key': f'soundness:frontend-{res["status"]}:{prog.surface}',
                    'seed': prog.seed, 'surface': prog.surface, 'build': 'frontend',
                    'detail': res.get('stderr', res['status']),
                })
                continue
            if res['accepted']:
                if prog.kind == 'mutated_unsound':
                    reds.append({
                        'key': f'soundness:unsound-accept:{prog.surface}:{prog.mutation}',
                        'seed': prog.seed, 'surface': prog.surface, 'build': 'frontend',
                        'detail': f'mutation {prog.mutation} accepted (expected {prog.expected_rules})',
                    })
                accepted_programs.append(prog)
            else:
                rejected_count += 1
                for fam in res['families']:
                    rejected_by_family[fam] += 1
                for r in set(res['rules']):
                    rejected_by_rule[r] += 1
                if prog.kind == 'valid':
                    reds.append({
                        'key': f'soundness:generator-invalid:{prog.surface}',
                        'seed': prog.seed, 'surface': prog.surface, 'build': 'frontend',
                        'detail': f'valid program rejected: {res["rules"]}',
                    })
                elif prog.kind == 'mutated_unsound' and prog.expected_rules:
                    expected_set = set(prog.expected_rules)
                    actual_set = set(res['rules'])
                    vacuous = sorted(actual_set & VACUOUS_RULES - expected_set)
                    if vacuous or not (actual_set & expected_set):
                        reds.append({
                            'key': f'soundness:wrong-rule:{prog.surface}:{prog.mutation}',
                            'seed': prog.seed, 'surface': prog.surface, 'build': 'frontend',
                            'detail': f'expected {sorted(expected_set)}, got {sorted(actual_set)}',
                        })

    for st in work_dir.glob('check-store-*'):
        shutil.rmtree(st, ignore_errors=True)

    exec_totals: Counter = Counter()
    wf_totals: Counter = Counter()
    build_store = work_dir / 'build-store'
    batch_size = max(args.batch_size, 1)
    for b_idx in range(0, len(accepted_programs), batch_size):
        if cpu_budget is not None and (child_cpu() - started_cpu) >= cpu_budget * 0.90:
            break
        batch = accepted_programs[b_idx:b_idx + batch_size]
        counts, wf_counts, batch_reds = execute_accepted_batch(args, batch, work_dir / f'batch-{b_idx}', build_store)
        exec_totals.update(counts)
        wf_totals.update(wf_counts)
        reds.extend(batch_reds)
    shutil.rmtree(build_store, ignore_errors=True)

    minimized = {}
    if args.shrink and reds:
        seen_keys = set()
        for r in reds:
            key = r['key']
            if key in seen_keys or r['seed'] is None or r['seed'] not in progs_by_seed:
                continue
            if cpu_budget is not None and (child_cpu() - started_cpu) >= cpu_budget:
                break
            seen_keys.add(key)
            minimized[key] = shrink_failure(args, progs_by_seed[r['seed']], key, out_dir / 'minimized')

    if not reds:
        shutil.rmtree(work_dir, ignore_errors=True)

    wall_s = round(time.monotonic() - started_wall, 2)
    cpu_s = round(child_cpu() - started_cpu, 2)
    gen_n = len(generated_programs)
    acc_n = len(accepted_programs)
    accept_rate = round(acc_n / gen_n, 4) if gen_n else 0.0
    divergences = sum(1 for r in reds if 'diverge' in r['key'])
    crashes = sum(1 for r in reds if any(t in r['key'] for t in ('crash', 'signal', 'stopped', 'timeout')))
    summary = {
        'version': VERSION,
        'builds': args.builds,
        'sanitized_runtime': str(args.sanitized_runtime) if args.sanitized_runtime else None,
        'controls': {
            'total': controls['total'], 'passed': controls['passed'],
            'failed': len(controls['failed']), 'by_family': controls['by_family'],
        },
        'randomized': {
            'generated': gen_n, 'accepted': acc_n, 'rejected': rejected_count,
            'accept_rate': accept_rate,
            'by_surface': dict(sorted(surface_counts.items())),
            'by_kind': dict(sorted(kind_counts.items())),
            'rejected_by_family': dict(sorted(rejected_by_family.items())),
            'rejected_by_rule': dict(sorted(rejected_by_rule.items())),
            'ir_wf_own': {
                'checked': wf_totals.get('checked', 0),
                'passed': wf_totals.get('passed', 0),
                'failed': wf_totals.get('failed', 0),
            },
            'executed': dict(sorted(exec_totals.items())),
        },
        'divergences': divergences,
        'crashes': crashes,
        'reds': reds,
        'minimized': minimized,
        'wall_seconds': wall_s,
        'cpu_seconds': cpu_s,
    }
    (out_dir / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    return summary


def print_summary(summary: dict):
    c = summary['controls']
    r = summary['randomized']
    print(f"controls: {c['passed']}/{c['total']} passed, {c['failed']} failed; families={c['by_family']}")
    if r['generated']:
        wf = r['ir_wf_own']
        print(
            f"randomized: generated={r['generated']} accepted={r['accepted']} ({r['accept_rate']:.1%}) "
            f"rejected={r['rejected']} ir_wf_own={wf['passed']}/{wf['checked']} "
            f"executed={r['executed']} divergences={summary['divergences']} crashes={summary['crashes']}"
        )
        print(f"surfaces: {r['by_surface']} | kinds: {r['by_kind']}")
        print(f"rejected by family: {r['rejected_by_family']}")
        print(f"rejected by rule: {r['rejected_by_rule']}")
    print(f"wall {summary['wall_seconds']} s, child CPU {summary['cpu_seconds']} s")
    for red in summary['reds'][:20]:
        print(f"RED {red['key']} (seed={red['seed']}, build={red['build']}): {red['detail'][:200]}")


def write_alerts(summary: dict, alert_path: Path | None):
    if not alert_path or not summary['reds']:
        return
    by_key = {}
    for red in summary['reds']:
        by_key.setdefault(red['key'], []).append(red)
    lines = []
    for key, hits in sorted(by_key.items()):
        first = hits[0]
        mini = summary['minimized'].get(key)
        tail = f"; minimized {mini['path']} ({mini['stmts']} stmts)" if mini else ''
        lines.append(
            f"{key}: {len(hits)} hits; first seed {first['seed']} on {first['build']}: {first['detail'][:160]}{tail}"
        )
    alert_path.parent.mkdir(parents=True, exist_ok=True)
    with alert_path.open('a') as f:
        for line in lines:
            f.write(line + '\n')


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('mode', choices=['controls', 'once', 'range', 'campaign', 'nightly'])
    parser.add_argument('--seed', type=int)
    parser.add_argument('--seeds', help='A..B (end exclusive), comma list, or single integer')
    parser.add_argument('--count', type=int, default=240, help='randomized program count (campaign/nightly)')
    parser.add_argument('--base-seed', type=int, default=1, help='starting seed when --seeds is omitted')
    parser.add_argument('--builds', default='evaluator,cranelift-dev,llvm-dev')
    parser.add_argument('--compiler', type=Path, required=True)
    parser.add_argument('--runtime', '--runtime-archive', dest='runtime', type=Path,
                        help='PIC native runtime archive (libhirsel_core_rt.a)')
    parser.add_argument('--sanitized-runtime', type=Path,
                        help='optional ASan/UBSan native runtime archive (lane 3371 hook)')
    parser.add_argument('--evaluator', type=Path)
    parser.add_argument('--cc', type=Path, default=(
        Path(os.environ['LLVM_SYS_231_PREFIX']) / 'bin/clang'
        if 'LLVM_SYS_231_PREFIX' in os.environ else None
    ))
    parser.add_argument('--root', type=Path, default=ROOT)
    parser.add_argument('--jobs', type=int, default=4)
    parser.add_argument('--batch-size', type=int, default=DEFAULT_BATCH_SIZE)
    parser.add_argument('--shrink', action=argparse.BooleanOptionalAction, default=True)
    parser.add_argument('--cpu-seconds', type=float, default=600.0, help='nightly child CPU budget')
    parser.add_argument('--alert', type=Path, help='append one alert line per red key')
    parser.add_argument('--surface', choices=[*SURFACES, *FUTURE_SURFACES],
                        help='restrict generation to one surface (future hooks refuse until landed)')
    parser.add_argument('--out', type=Path, required=True)
    return parser


def main(argv=None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    if args.surface in FUTURE_SURFACES:
        parser.error(f'surface {args.surface!r} is a reserved hook for an unlanded feature and is not enabled yet')
    args.builds = [b.strip() for b in args.builds.split(',') if b.strip()]
    unknown = [b for b in args.builds if b not in BUILDS]
    if unknown:
        parser.error(f'unknown builds {unknown}; choose from {", ".join(BUILDS)}')
    args.root = args.root.resolve()
    args.compiler = args.compiler.resolve()
    if args.sanitized_runtime is not None:
        args.sanitized_runtime = args.sanitized_runtime.resolve()
        if not args.sanitized_runtime.is_file():
            parser.error(f'--sanitized-runtime does not exist: {args.sanitized_runtime}')
    if args.mode == 'controls':
        args.out.mkdir(parents=True, exist_ok=True)
        controls = run_negative_controls(args, args.out)
        summary = {
            'version': VERSION, 'builds': [], 'sanitized_runtime': None,
            'controls': {'total': controls['total'], 'passed': controls['passed'],
                         'failed': len(controls['failed']), 'by_family': controls['by_family']},
            'randomized': {'generated': 0, 'accepted': 0, 'rejected': 0, 'accept_rate': 0.0,
                           'by_surface': {}, 'by_kind': {}, 'rejected_by_family': {},
                           'rejected_by_rule': {}, 'ir_wf_own': {'checked': 0, 'passed': 0, 'failed': 0},
                           'executed': {}},
            'divergences': 0, 'crashes': 0,
            'reds': [{'key': f'soundness:control:{f["surface"]}:{f["name"]}', 'seed': None,
                      'surface': f['surface'], 'build': 'frontend', 'detail': f['reason']}
                     for f in controls['failed']],
            'minimized': {}, 'wall_seconds': 0.0, 'cpu_seconds': 0.0,
        }
        (args.out / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
        print_summary(summary)
        write_alerts(summary, args.alert)
        return int(bool(summary['reds']))

    if 'evaluator' in args.builds and args.evaluator is None:
        parser.error('evaluator build requires --evaluator')
    if any(b in NATIVE for b in args.builds) and args.runtime is None:
        parser.error('native builds require --runtime / --runtime-archive')
    if args.evaluator:
        args.evaluator = args.evaluator.resolve()
    if args.runtime:
        args.runtime = args.runtime.resolve()

    if args.mode == 'once':
        if args.seed is None:
            parser.error('once mode requires --seed')
        seeds = [args.seed]
        budget = None
    elif args.mode == 'nightly':
        seeds = parse_seeds(args.seeds, args.base_seed, args.count)
        budget = args.cpu_seconds
    else:
        seeds = parse_seeds(args.seeds, args.base_seed, args.count)
        budget = None

    summary = run_campaign(args, seeds, cpu_budget=budget)
    print_summary(summary)
    write_alerts(summary, args.alert)
    return int(bool(summary['reds']))


if __name__ == '__main__':
    raise SystemExit(main())
