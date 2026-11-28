#!/usr/bin/env python3
"""Ownership and capability soundness fuzzer for Hirsel.

Runs two mandatory verification layers:
1. Curated negative-control corpus: asserts every known-unsound pattern across aliasing,
   move, escape, capability, Held/unit and the six feature families is rejected by
   `hirsel check` with the expected rule (no silent acceptance, no vacuous parse error).
2. Seeded multi-feature campaign + oracles:
   - Frontend checker (`hirsel check --json`): rejects provably unsound mutations with
     their expected rule family; accepts ~80% complex multi-feature/unwinding programs.
   - CoreIR well-formedness & ownership verifier (`hirsel_core_ir::wf::module`, including
     `wf::own` R1 double-drop, R2 use-after-drop, R3 view-drop) via `hirsel-core-contract-eval`.
   - 3-engine differential execution: Evaluator + Cranelift + LLVM asserting identical
     outcomes and zero crashes.
   - Value oracle: the generator's own model of each feature statement, reported by the
     program on an `oracle` line when an engine computes another value.
   - `--asan`: the native programs again on an AddressSanitizer-instrumented runtime
     (ownership_soundness_asan.py); a missing nightly component is reported as skipped
     and the run exits 75, never 0.

Example:
  ownership_soundness_fuzz.py controls --compiler hirsel --out /tmp/own
  ownership_soundness_fuzz.py campaign --seeds 0..240 --builds evaluator,cranelift-dev,llvm-dev \\
      --compiler hirsel --runtime-archive libhirsel_core_rt.a \\
      --evaluator hirsel_core_contract_eval --out /tmp/own
  ownership_soundness_fuzz.py campaign --seeds 0..240 --asan --asan-sample 64 ... --out /tmp/own
  ownership_soundness_fuzz.py once --seed 17 ... --out /tmp/own        # replay one seed
  ownership_soundness_fuzz.py positive-control --seed 5 ... --out /tmp/own
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
import ownership_soundness_asan as asan  # noqa: E402
from contract_exec import Evaluator, library_sources, native_build  # noqa: E402
from ownership_soundness_features import FEATURE_SURFACES  # noqa: E402
from ownership_soundness_gen import (  # noqa: E402
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
ASAN_ARMS = {'cranelift-dev': ('asan-cranelift', 'cranelift'), 'llvm-dev': ('asan-llvm', 'llvm')}
# Positive-control statements: one engine's observed value is perturbed for MISMATCH_MARKER,
# and MODEL_MARKER states a wrong expectation.
MISMATCH_MARKER = 'let pc_mismatch = 424242u64\nacc = mix(acc, pc_mismatch)'
MODEL_MARKER = 'acc = mix(acc, expect(out, 999u64, 1u64, 2u64))'
# ASAN_MARKER programs run on the ASan arms with asan.PLANT_ENV set, which fires a double free.
ASAN_MARKER = 'let pc_asan = 515151u64\nacc = mix(acc, pc_asan)'


def rule_family(rule: str) -> str:
    return rule.split('-', 1)[0] if '-' in rule else rule


def manifest(root: Path, source: Path, std: bool = False) -> dict:
    """The generated package over core, and over std's sources when the program decodes data."""
    core = {'name': 'core', 'version': '1'}
    pkg = {'name': 'gen', 'version': '1'}
    packages = [{'package': core, 'sources': library_sources(root / 'core/core', True), 'dependencies': []}]
    dependencies = [core]
    if std:
        library = {'name': 'std', 'version': '1'}
        packages.append({'package': library, 'sources': library_sources(root / 'core/std', False),
                         'dependencies': [core]})
        dependencies.append(library)
    packages.append({'package': pkg, 'dependencies': dependencies,
                     'sources': [{'file': str(source.resolve()), 'modules': ['main'], 'signatures': False}]})
    return {'core': core, 'packages': packages}


def check_source(compiler: Path, root: Path, work: Path, store: Path, source: str,
                 entry: str | None = None, std: bool = False) -> dict:
    work.mkdir(parents=True, exist_ok=True)
    src_file = work / 'main.hirsel'
    src_file.write_text(source)
    inp_file = work / 'inputs.json'
    inp_file.write_text(json.dumps(manifest(root, src_file, std)))
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


def parse_oracle_records(stdout: bytes) -> list[tuple[int, int, int]]:
    """`oracle <tag> <got> <want>` lines: values that differ from the generator's model."""
    found = []
    for line in stdout.decode(errors='replace').splitlines():
        parts = line.split()
        if len(parts) == 4 and parts[0] == 'oracle':
            found.append((int(parts[1]), int(parts[2]), int(parts[3])))
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
    oracle = parse_oracle_records(stdout)
    if outcome.get('kind') == 'returned':
        vals = outcome.get('values') or []
        return {'kind': 'ok', 'wf_checked': True, 'wf_ok': True, 'oracle': oracle,
                'value': vals[0] if vals else None, 'records': records, 'cause': None}
    if outcome.get('kind') == 'failed':
        return {'kind': 'failed', 'wf_checked': True, 'wf_ok': True, 'oracle': oracle,
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
    oracle = parse_oracle_records(proc.stdout)
    stderr_text = proc.stderr.decode(errors='replace')
    # A sanitizer report is a failure whatever the exit status.
    if report := asan.diagnostic(stderr_text):
        return {'kind': 'sanitizer', 'status': proc.returncode, 'detail': report}
    if proc.returncode == 0:
        return {'kind': 'ok', 'records': records, 'oracle': oracle, 'cause': None}
    if proc.returncode == 101:
        cause = stderr_text.strip().splitlines()[:1]
        return {'kind': 'failed', 'records': records, 'oracle': oracle, 'cause': cause[0] if cause else 'failed'}
    if proc.returncode == 102 or 'run stop' in stderr_text:
        return {'kind': 'stopped', 'name': stderr_text.strip()[-200:], 'records': records}
    if proc.returncode < 0 or proc.returncode > 128:
        return {'kind': 'crash', 'status': proc.returncode, 'detail': stderr_text[-500:]}
    return {'kind': 'exit', 'status': proc.returncode, 'detail': stderr_text[-400:], 'records': records}


def seeded_mismatch(args, prog: GeneratedProgram, arm: str, outcome: dict) -> dict:
    """Positive control only: perturb one engine's value for a program carrying the marker."""
    if getattr(args, 'inject_build', None) == arm and MISMATCH_MARKER in prog.stmts and outcome.get('records'):
        outcome = dict(outcome, records={idx: value ^ 1 for idx, value in outcome['records'].items()})
    return outcome


def compare_engine_outcomes(idx: int, seed: int, surface: str, outcomes: dict[str, dict]) -> list[dict]:
    reds = []
    for build, out in outcomes.items():
        if out['kind'] in ('ir-wf-own', 'crash', 'sanitizer', 'stopped', 'timeout', 'harness', 'exit'):
            reds.append({
                'key': f'soundness:{out["kind"]}:{surface}:{build}',
                'seed': seed, 'surface': surface, 'build': build,
                'detail': str(out.get('detail') or out.get('name') or out)[:1200 if out['kind'] == 'sanitizer' else 400],
            })
        elif out.get('oracle'):
            tag, got, want = out['oracle'][0]
            reds.append({
                'key': f'soundness:oracle-mismatch:{surface}:{build}',
                'seed': seed, 'surface': surface, 'build': build,
                'detail': f'statement {tag // 10} check {tag % 10}: got {got}, the model requires {want}',
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


def native_arms(args, batch: list[GeneratedProgram]) -> list[tuple]:
    """(arm, backend, release, runtime, C driver, environment, entries to run) for one batch."""
    everyone = list(range(len(batch)))
    arms = [(build, *NATIVE[build], args.runtime, args.cc, None, everyone) for build in args.builds if build in NATIVE]
    if args.asan_runtime:
        sampled = [idx for idx, prog in enumerate(batch) if args.asan_seeds is None or prog.seed in args.asan_seeds]
        if sampled:
            arms += [(arm, backend, False, args.asan_runtime, args.asan_cc, asan.environment(), sampled)
                     for build, (arm, backend) in ASAN_ARMS.items() if build in args.builds]
    return arms


def execute_accepted(args, batch: list[GeneratedProgram], work: Path,
                     store_root: Path) -> tuple[dict, dict, list[dict]]:
    """Run accepted programs across IR wf::own + Evaluator + Cranelift + LLVM (+ the ASan arms).

    Returns the seeds each arm ran, the wf::own counts and the reds. One program runs as its
    standalone source, the form the reducer writes; several share one native build per arm.
    """
    work.mkdir(parents=True, exist_ok=True)
    single = len(batch) == 1
    std = any(prog.needs_std for prog in batch)
    src_file = work / 'main.hirsel'
    src_file.write_text(batch[0].render_standalone() if single else render_batch(batch))
    inp_file = work / 'inputs.json'
    inp_file.write_text(json.dumps(manifest(args.root, src_file, std)))

    per_entry: list[dict[str, dict]] = [{} for _ in batch]
    reds: list[dict] = []
    ran: dict[str, list[int]] = {}
    wf_counts = Counter()

    if 'evaluator' in args.builds:
        with Evaluator(args.evaluator) as resident:
            for idx, prog in enumerate(batch):
                ev_out = run_evaluator_case(resident, inp_file, f'gen@1/main/case_{idx}')
                per_entry[idx]['evaluator'] = ev_out
                ran.setdefault('evaluator', []).append(prog.seed)
                if ev_out.get('wf_checked'):
                    wf_counts['checked'] += 1
                    wf_counts['passed' if ev_out.get('wf_ok') else 'failed'] += 1

    grants = ['Stdout', 'Tasks']
    for arm, backend, release, runtime_path, cc, extra_env, entries in native_arms(args, batch):
        binary = work / f'{arm}.bin'
        # A std build keeps its own store: the two manifests share no checked product.
        built = native_build(
            args.compiler, runtime_path, inp_file, 'gen@1/main/main', backend,
            store_root / (f'{arm}-std' if std else arm), binary, limit=ROOT_LIMIT, release=release,
            grants=grants, cc=cc,
        )
        if built.returncode != 0 or not binary.exists():
            if not single:
                merged: dict[str, list[int]] = {}
                for prog in batch:
                    one_ran, one_wf, one_reds = execute_accepted(args, [prog], work / f'single-{prog.seed}', store_root)
                    for name, seeds in one_ran.items():
                        merged.setdefault(name, []).extend(seeds)
                    wf_counts.update(one_wf)
                    reds.extend(one_reds)
                return merged, dict(wf_counts), reds
            reds.append({
                'key': f'soundness:build:{batch[0].surface}:{arm}',
                'seed': batch[0].seed, 'surface': batch[0].surface, 'build': arm,
                'detail': (built.stdout + built.stderr).decode(errors='replace')[-600:],
            })
            continue
        if extra_env is not None and arm not in args.asan_instrumented:
            args.asan_instrumented[arm] = asan.instrumented(binary)
            if not args.asan_instrumented[arm]:
                reds.append({
                    'key': f'soundness:asan-uninstrumented:{arm}', 'seed': batch[0].seed,
                    'surface': batch[0].surface, 'build': arm,
                    'detail': 'the linked program carries no AddressSanitizer runtime',
                })
        for idx in entries:
            env = extra_env
            if extra_env is not None and args.asan_plant and ASAN_MARKER in batch[idx].stmts:
                env = dict(extra_env, **{asan.PLANT_ENV: '1'})
            outcome = run_native_case(binary, [] if single else [str(idx)], extra_env=env)
            per_entry[idx][arm] = seeded_mismatch(args, batch[idx], arm, outcome)
            ran.setdefault(arm, []).append(batch[idx].seed)
        binary.unlink(missing_ok=True)

    for idx, prog in enumerate(batch):
        if per_entry[idx]:
            reds.extend(compare_engine_outcomes(idx, prog.seed, prog.surface, per_entry[idx]))
    return ran, dict(wf_counts), reds


def shrink_failure(args, prog: GeneratedProgram, target_key: str, out_dir: Path) -> dict:
    """Two-stage reducer: statement deletion to a fixpoint, then choice-stream lowering."""
    work = out_dir / f'seed-{prog.seed}' / target_key.replace(':', '_')
    shutil.rmtree(work, ignore_errors=True)
    work.mkdir(parents=True, exist_ok=True)
    store = work / 'store'
    runs = 0

    def reproduces(candidate: GeneratedProgram) -> bool:
        nonlocal runs
        if runs >= SHRINK_MAX_RUNS:
            return False
        runs += 1
        chk = check_source(args.compiler, args.root, work / 'chk', store, candidate.render_standalone(),
                           std=candidate.needs_std)
        if target_key.startswith('soundness:unsound-accept:'):
            return chk['status'] == 'ok' and chk['accepted']
        if target_key.startswith('soundness:frontend-'):
            return chk['status'] != 'ok'
        if target_key.startswith(('soundness:generator-invalid:', 'soundness:wrong-rule:')):
            return chk['status'] == 'ok' and not chk['accepted']
        if not chk['accepted']:
            return False
        _, _, reds = execute_accepted(args, [candidate], work / 'run', store)
        return any(r['key'] == target_key for r in reds)

    best = prog
    kept = list(range(len(prog.stmts)))
    changed = True
    while changed and len(best.stmts) > 1 and runs < SHRINK_MAX_RUNS:
        changed = False
        for i in range(len(best.stmts)):
            cand = best.without(i)
            if reproduces(cand):
                best = cand
                del kept[i]
                changed = True
                break

    def regenerate(draws: list) -> GeneratedProgram | None:
        """The program these draws make, cut down to the statements deletion kept."""
        full = generate_program(prog.seed, draws=draws, forced_surface=prog.surface, forced_kind=prog.kind)
        if len(full.stmts) != len(prog.stmts):
            return None
        return GeneratedProgram(
            seed=full.seed, surface=full.surface, kind=full.kind, mutation=full.mutation,
            expected_rules=full.expected_rules, stmts=[full.stmts[i] for i in kept], draws=full.draws,
            bounds=full.bounds, families=[full.families[i] for i in kept],
        )

    # Lower the choice stream only for a program its draws reproduce (not a seeded control).
    draws = list(prog.draws)
    regenerated = regenerate(draws) if draws else None
    if regenerated is not None and regenerated.stmts == best.stmts:
        for i in range(len(draws)):
            if runs >= SHRINK_MAX_RUNS:
                break
            if draws[i] == 0:
                continue
            cand = regenerate(draws[:i] + [0] + draws[i + 1:])
            if cand is not None and cand.stmts != best.stmts and reproduces(cand):
                draws = list(cand.draws)
                best = cand
    min_path = work / 'minimized.hirsel'
    min_path.write_text(f'// seed {prog.seed}: {target_key}\n' + best.render_standalone())
    shutil.rmtree(store, ignore_errors=True)
    shutil.rmtree(work / 'run', ignore_errors=True)
    shutil.rmtree(work / 'chk', ignore_errors=True)
    return {'path': str(min_path), 'seed': prog.seed, 'stmts': len(best.stmts), 'runs': runs,
            'statements': best.stmts}


def family_counts(generated: list[GeneratedProgram], accepted: list[GeneratedProgram],
                  by_seed: dict[int, GeneratedProgram], ran_seeds: dict[str, list[int]]) -> dict:
    """Per statement family: programs led by it, statements made, accepted and run, model checks run."""
    rows = {family: {'programs': 0, 'statements': 0, 'accepted_statements': 0, 'model_checks': 0, 'executed': {}}
            for family in SURFACES}
    for prog in generated:
        rows[prog.surface]['programs'] += 1
        for family in prog.families:
            if family:
                rows[family]['statements'] += 1
    for prog in accepted:
        for family, stmt in zip(prog.families, prog.stmts):
            if family:
                rows[family]['accepted_statements'] += 1
                rows[family]['model_checks'] += stmt.count('expect(out, ')
    for arm, seeds in sorted(ran_seeds.items()):
        for seed in seeds:
            for family in by_seed[seed].families:
                if family:
                    rows[family]['executed'][arm] = rows[family]['executed'].get(arm, 0) + 1
    return rows


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

    if args.asan_status['status'] == 'failed':
        reds.append({'key': 'soundness:asan-runtime-build', 'seed': None, 'surface': 'asan', 'build': 'asan',
                     'detail': args.asan_status['reason']})

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
        prog = args.program_for(seed) if args.program_for else generate_program(seed, forced_surface=args.surface)
        res = check_source(args.compiler, args.root, work_dir / f's-{seed}', worker_store(), prog.render_standalone(),
                           std=prog.needs_std)
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
                        'detail': f'valid program rejected: {res["rules"]} '
                                  f'{[d.get("message") for d in res["diagnostics"] + res["incomplete"]][:3]}',
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

    if args.asan_runtime and args.asan_sample is not None:
        # An even spread of the accepted programs, fixed by the seed list alone.
        stride = max(len(accepted_programs) / max(args.asan_sample, 1), 1.0)
        picks = {int(i * stride) for i in range(args.asan_sample)}
        args.asan_seeds = {prog.seed for idx, prog in enumerate(accepted_programs) if idx in picks}

    ran_seeds: dict[str, list[int]] = {}
    wf_totals: Counter = Counter()
    build_store = work_dir / 'build-store'
    batch_size = max(args.batch_size, 1)
    for b_idx in range(0, len(accepted_programs), batch_size):
        if cpu_budget is not None and (child_cpu() - started_cpu) >= cpu_budget * 0.90:
            break
        batch = accepted_programs[b_idx:b_idx + batch_size]
        ran, wf_counts, batch_reds = execute_accepted(args, batch, work_dir / f'batch-{b_idx}', build_store)
        for arm, arm_seeds in ran.items():
            ran_seeds.setdefault(arm, []).extend(arm_seeds)
        wf_totals.update(wf_counts)
        reds.extend(batch_reds)
    shutil.rmtree(build_store, ignore_errors=True)
    exec_totals = {arm: len(arm_seeds) for arm, arm_seeds in ran_seeds.items()}
    if args.asan_runtime and accepted_programs and not any(arm in exec_totals for arm, _ in ASAN_ARMS.values()):
        reds.append({'key': 'soundness:asan-not-run', 'seed': None, 'surface': 'asan', 'build': 'asan',
                     'detail': 'the ASan arms ran no accepted program'})

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
    asan_arms = [arm for arm, _ in ASAN_ARMS.values()]
    summary = {
        'version': VERSION,
        'builds': args.builds,
        'asan': {
            **args.asan_status,
            'executed': {arm: exec_totals.get(arm, 0) for arm in asan_arms if arm in exec_totals},
            'instrumented': dict(args.asan_instrumented),
            'diagnostics': sum(1 for r in reds if ':sanitizer:' in r['key']),
            'divergences': sum(1 for r in reds if 'diverge' in r['key'] and r['build'] in asan_arms),
        },
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
            'by_family': family_counts(generated_programs, accepted_programs, progs_by_seed, ran_seeds),
            'oracle_mismatches': sum(1 for r in reds if ':oracle-mismatch:' in r['key']),
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
        for family, row in r['by_family'].items():
            if row['statements']:
                print(f"family {family}: programs={row['programs']} statements={row['statements']} "
                      f"accepted={row['accepted_statements']} model_checks={row['model_checks']} "
                      f"executed={row['executed']}")
        print(f"model oracle mismatches: {r['oracle_mismatches']}")
    sanitizer = summary['asan']
    if sanitizer['status'] == 'skipped':
        print(f"asan: SKIPPED ({sanitizer['reason']}); no sanitizer verdict was produced")
    elif sanitizer['status'] == 'ran':
        print(f"asan: ran {sanitizer['executed']} on {sanitizer['toolchain']}; instrumented={sanitizer['instrumented']} "
              f"diagnostics={sanitizer['diagnostics']} divergences={sanitizer['divergences']}")
    print(f"wall {summary['wall_seconds']} s, child CPU {summary['cpu_seconds']} s")
    for red in summary['reds'][:20]:
        print(f"RED {red['key']} (seed={red['seed']}, build={red['build']}): {red['detail'][:200]}")
        mini = summary['minimized'].get(red['key'])
        if mini and mini['seed'] == red['seed']:
            print(f"    minimized to {mini['stmts']} statements in {mini['runs']} runs: {mini['path']}")


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


def prepare_asan(args, parser) -> None:
    """Resolve the ASan arm: ran (runtime built), skipped (nightly component absent) or failed."""
    args.asan_runtime = args.asan_cc = args.asan_seeds = args.asan_plant = None
    args.asan_instrumented = {}
    args.asan_status = {'status': 'not-requested'}
    if not (args.asan or args.sanitized_runtime):
        if args.asan_sample is not None:
            parser.error('--asan-sample needs --asan')
        return
    if args.mode == 'controls':
        parser.error('--asan applies to program runs, not to the controls mode')
    if not any(build in ASAN_ARMS for build in args.builds):
        parser.error('--asan needs cranelift-dev or llvm-dev in --builds')
    if args.asan_sample is not None and args.asan_sample < 1:
        parser.error('--asan-sample takes a positive count')
    ready, info = asan.toolchain()
    if not ready:
        args.asan_status = {'status': 'skipped', 'reason': info}
        return
    try:
        runtime = args.sanitized_runtime or asan.build_runtime(args.asan_target_dir.resolve(), args.jobs)
    except RuntimeError as error:
        args.asan_status = {'status': 'failed', 'reason': str(error)[-1500:]}
        return
    args.asan_runtime = runtime
    if args.mode == 'positive-control':
        args.asan_plant = asan.plant_object(args.cc, args.out.resolve() / 'asan')
    args.asan_cc = asan.linker_wrapper(args.cc, args.out.resolve() / 'asan', args.asan_plant)
    args.asan_status = {'status': 'ran', 'toolchain': info, 'runtime': str(runtime), 'options': asan.OPTIONS,
                        'sample': args.asan_sample}


def run_positive_control(args) -> int:
    """Seed one cross-engine mismatch and one model mismatch among clean seeds, and with --asan
    one memory error on the ASan arms.

    Each must be reported at exactly its seed and reduced to the seeded statement alone.
    """
    base = args.seed if args.seed is not None else 1
    seeds = list(range(base, base + 6))
    target = [build for build in args.builds if build in NATIVE][-1]
    planted = {seeds[1]: MISMATCH_MARKER, seeds[4]: MODEL_MARKER}
    asan_arms = sorted(arm for build, (arm, _) in ASAN_ARMS.items() if build in args.builds) if args.asan_plant else []
    if asan_arms:
        planted[seeds[3]] = ASAN_MARKER

    def program_for(seed: int) -> GeneratedProgram:
        prog = generate_program(seed, forced_surface=args.surface, forced_kind='valid')
        if seed in planted:
            at = len(prog.stmts) // 2
            prog.stmts.insert(at, planted[seed])
            prog.families.insert(at, None)
        return prog

    args.program_for, args.inject_build, args.shrink = program_for, target, True
    summary = run_campaign(args, seeds)
    print_summary(summary)
    problems = []
    by_seed: dict = {}
    for red in summary['reds']:
        by_seed.setdefault(red['seed'], []).append(red)
    for seed in sorted(by_seed, key=str):
        if seed not in planted:
            problems.append(f'unplanted seed {seed} is red: {by_seed[seed][0]["key"]}')
    mismatch = [red for red in by_seed.get(seeds[1], []) if ':diverge-value:' in red['key']]
    if [red['build'] for red in mismatch] != [target] or len(by_seed.get(seeds[1], [])) != 1:
        problems.append(f'seed {seeds[1]}: expected one diverge-value red on {target}, got {by_seed.get(seeds[1])}')
    model = by_seed.get(seeds[4], [])
    if sorted(red['build'] for red in model) != sorted(args.builds + asan_arms) or any(':oracle-mismatch:' not in red['key'] for red in model):
        problems.append(f'seed {seeds[4]}: expected an oracle-mismatch red on every build, got {model}')
    if asan_arms:
        memory = by_seed.get(seeds[3], [])
        if sorted(red['build'] for red in memory) != asan_arms or any(':sanitizer:' not in red['key'] for red in memory):
            problems.append(f'seed {seeds[3]}: expected a sanitizer red on each of {asan_arms}, got {memory}')
    reduced = []
    for seed, marker in planted.items():
        for red in by_seed.get(seed, []):
            mini = summary['minimized'].get(red['key'])
            if not mini or mini['seed'] != seed:
                problems.append(f'seed {seed}: no minimized reproducer for {red["key"]}')
            elif mini['statements'] != [marker]:
                problems.append(f'seed {seed}: reduced to {mini["stmts"]} statements, not the seeded one alone')
            else:
                reduced.append({'seed': seed, 'key': red['key'], 'runs': mini['runs'], 'path': mini['path'],
                                'from_statements': len(program_for(seed).stmts)})
    verdict = {'passed': not problems, 'seeds': seeds, 'perturbed_build': target, 'asan': summary['asan'],
               'planted': {str(seed): marker for seed, marker in planted.items()},
               'reduced': reduced, 'problems': problems}
    (args.out / 'positive-control.json').write_text(json.dumps(verdict, indent=2) + '\n')
    for row in reduced:
        print(f"positive control: seed {row['seed']} {row['key']} reduced {row['from_statements']} -> 1 "
              f"statements in {row['runs']} runs")
    print('positive control: ' + ('PASS' if not problems else 'FAIL ' + '; '.join(problems)))
    if problems:
        return 1
    # A requested sanitizer control that could not run did not pass.
    return asan.EX_DEFERRED if summary['asan']['status'] == 'skipped' else 0


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('mode', choices=['controls', 'once', 'range', 'campaign', 'nightly', 'positive-control'])
    parser.add_argument('--seed', type=int)
    parser.add_argument('--seeds', help='A..B (end exclusive), comma list, or single integer')
    parser.add_argument('--count', type=int, default=240, help='randomized program count (campaign/nightly)')
    parser.add_argument('--base-seed', type=int, default=1, help='starting seed when --seeds is omitted')
    parser.add_argument('--builds', default='evaluator,cranelift-dev,llvm-dev')
    parser.add_argument('--compiler', type=Path, required=True)
    parser.add_argument('--runtime', '--runtime-archive', dest='runtime', type=Path,
                        help='PIC native runtime archive (libhirsel_core_rt.a)')
    parser.add_argument('--asan', action='store_true',
                        help='also run the native programs on an AddressSanitizer runtime built with the nightly '
                             'toolchain of tools/sanitizers/run.py; exits 75 when that toolchain is absent')
    parser.add_argument('--asan-sample', type=int, metavar='N',
                        help='run only N evenly spread accepted programs on the ASan arms (default: all)')
    parser.add_argument('--asan-target-dir', type=Path, default=ROOT / 'target',
                        help='cargo target root of the ASan runtime build (default: the sweep\'s target/)')
    parser.add_argument('--sanitized-runtime', type=Path,
                        help='a prebuilt ASan hirsel-core-rt archive to use instead of building one (implies --asan)')
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
    parser.add_argument('--surface', choices=SURFACES, help='restrict generation to one surface')
    parser.add_argument('--out', type=Path, required=True)
    return parser


def main(argv=None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    args.builds = [b.strip() for b in args.builds.split(',') if b.strip()]
    unknown = [b for b in args.builds if b not in BUILDS]
    if unknown:
        parser.error(f'unknown builds {unknown}; choose from {", ".join(BUILDS)}')
    args.root = args.root.resolve()
    args.compiler = args.compiler.resolve()
    args.program_for = args.inject_build = None
    if args.sanitized_runtime is not None:
        args.sanitized_runtime = args.sanitized_runtime.resolve()
        if not args.sanitized_runtime.is_file():
            parser.error(f'--sanitized-runtime does not exist: {args.sanitized_runtime}')
    args.out.mkdir(parents=True, exist_ok=True)
    prepare_asan(args, parser)
    if args.mode == 'controls':
        controls = run_negative_controls(args, args.out)
        summary = {
            'version': VERSION, 'builds': [], 'asan': {'status': 'not-requested'},
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

    if args.mode == 'positive-control':
        if not any(b in NATIVE for b in args.builds) or len(args.builds) < 2:
            parser.error('positive-control needs a native build and a second engine to disagree with')
        return run_positive_control(args)
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
    if summary['reds']:
        return 1
    # A campaign that could not produce its requested sanitizer verdict did not pass.
    return asan.EX_DEFERRED if summary['asan']['status'] == 'skipped' else 0


if __name__ == '__main__':
    raise SystemExit(main())
