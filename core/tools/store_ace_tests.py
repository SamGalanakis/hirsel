#!/usr/bin/env python3
"""M #4 ACE/B3 bounded-exhaustive crash workload enumeration (<= 3 ops)
over the existing W2 cut and sector-damage machinery (store_crash_tests.py).
"""
import argparse
from concurrent.futures import ProcessPoolExecutor, as_completed
from dataclasses import dataclass
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import time

sys.path.insert(0, str(Path(__file__).resolve().parent))
from std_native import ROOT
from store_crash_tests import (
    SECTOR_KINDS,
    SECTOR_SEEDS,
    bridge_env,
    build_crash,
    count_cuts,
)
from store_tests import crash_inputs

# 15-operation store alphabet covering inline/blob puts, deletes, checkpoints,
# pins, branches, cells, logs, trims, schema migrations, and durability syncs.
OPS = (
    'put_a', 'put_b', 'del_a', 'blob', 'cut',
    'pin', 'unpin', 'fork', 'put_branch', 'del_branch',
    'cell', 'log', 'trim', 'schema', 'sync',
)


@dataclass(frozen=True)
class PreState:
    has_a: bool = False
    has_b: bool = False
    pinned: bool = False
    branched: bool = False
    log_len: int = 0
    has_schema: bool = False
    dirty_journal: bool = False
    dirty_cut: bool = True


def apply_precondition(state: PreState, op: str):
    """Return the next abstract precondition state, or None if `op` is ill-formed
    or symmetric/redundant from `state`."""
    if op == 'put_a':
        return PreState(True, state.has_b, state.pinned, state.branched,
                        state.log_len, state.has_schema, True, True)
    if op == 'put_b':
        # Key symmetry: populate key 0 before key 1.
        if not state.has_a and not state.has_b:
            return None
        return PreState(state.has_a, True, state.pinned, state.branched,
                        state.log_len, state.has_schema, True, True)
    if op == 'del_a':
        if not state.has_a:
            return None
        return PreState(False, state.has_b, state.pinned, state.branched,
                        state.log_len, state.has_schema, True, True)
    if op == 'blob':
        return PreState(True, state.has_b, state.pinned, state.branched,
                        state.log_len, state.has_schema, True, True)
    if op == 'cut':
        if not state.dirty_cut:
            return None
        return PreState(state.has_a, state.has_b, state.pinned, state.branched,
                        state.log_len, state.has_schema, False, False)
    if op == 'pin':
        if state.pinned:
            return None
        return PreState(state.has_a, state.has_b, True, state.branched,
                        state.log_len, state.has_schema, False, True)
    if op == 'unpin':
        if not state.pinned:
            return None
        return PreState(state.has_a, state.has_b, False, state.branched,
                        state.log_len, state.has_schema, False, True)
    if op == 'fork':
        if state.branched:
            return None
        return PreState(state.has_a, state.has_b, state.pinned, True,
                        state.log_len, state.has_schema, False, True)
    if op == 'put_branch':
        if not state.branched:
            return None
        return PreState(state.has_a, state.has_b, state.pinned, state.branched,
                        state.log_len, state.has_schema, True, True)
    if op == 'del_branch':
        if not state.branched:
            return None
        return PreState(state.has_a, state.has_b, state.pinned, False,
                        state.log_len, state.has_schema, False, True)
    if op == 'cell':
        return PreState(state.has_a, state.has_b, state.pinned, state.branched,
                        state.log_len, state.has_schema, True, True)
    if op == 'log':
        return PreState(state.has_a, state.has_b, state.pinned, state.branched,
                        state.log_len + 1, state.has_schema, True, True)
    if op == 'trim':
        if state.log_len == 0:
            return None
        return PreState(state.has_a, state.has_b, state.pinned, state.branched,
                        0, state.has_schema, True, True)
    if op == 'schema':
        if state.has_schema:
            return None
        return PreState(state.has_a, state.has_b, state.pinned, state.branched,
                        state.log_len, True, False, True)
    if op == 'sync':
        if not state.dirty_journal:
            return None
        return PreState(state.has_a, state.has_b, state.pinned, state.branched,
                        state.log_len, state.has_schema, False, state.dirty_cut)
    raise ValueError(f'unknown ACE op: {op}')


def enumerate_workloads(max_len=3):
    """Bounded-exhaustive DFS of every valid workload of length 1..=max_len."""
    workloads = []

    def dfs(seq, state):
        if seq:
            workloads.append(seq)
        if len(seq) == max_len:
            return
        for op in OPS:
            nxt = apply_precondition(state, op)
            if nxt is not None:
                dfs((*seq, op), nxt)

    dfs((), PreState())
    return workloads


def workload_name(seq):
    return 'ace:' + ','.join(seq)


def run_one_workload(task):
    executable, bridge, seq, damage_seeds = task
    scenario = workload_name(seq)
    env = bridge_env(Path(bridge))
    trace_env = dict(env, HIRSEL_W2_TRACE_CUTS='1')
    ran = subprocess.run([executable, scenario, 'count'], env=trace_env,
                         capture_output=True, text=True, timeout=300)
    match = re.fullmatch(r'cuts ([0-9]+)\n', ran.stdout)
    if ran.returncode != 0 or match is None or int(match[1]) == 0:
        return {
            'scenario': scenario,
            'clean_cuts': 0,
            'sector_cuts': 0,
            'failures': [f'{scenario} count failed ({ran.returncode}): {ran.stderr.strip()}'],
            'seen': {},
        }
    count = int(match[1])
    failures = []
    clean_cuts = 0
    for cut in range(count + 1):
        res = subprocess.run([executable, scenario, str(cut)], env=env,
                             capture_output=True, text=True, timeout=300)
        clean_cuts += 1
        if res.returncode != 0:
            failures.append(f'{scenario} cut {cut} exited {res.returncode}: {res.stderr.strip()}')
            break
    sector_cuts = 0
    seen = {kind: [] for kind in SECTOR_KINDS}
    if not failures and damage_seeds > 0:
        cuts = [
            (int(cut), kind)
            for cut, kind in re.findall(r'^w2-cut ([0-9]+) ([a-z-]+) ', ran.stderr, re.M)
            if kind in SECTOR_KINDS
        ]
        for seed in range(damage_seeds):
            for cut, kind in cuts:
                res = subprocess.run([executable, scenario, str(cut), '0', str(seed)], env=env,
                                     capture_output=True, text=True, timeout=300)
                sector_cuts += 1
                if res.returncode != 0:
                    failures.append(
                        f'{scenario} cut {cut} damage {seed} exited {res.returncode}: {res.stderr.strip()}'
                    )
                    break
                for img in re.findall(r'^w2-image ([A-Za-z]+)$', res.stderr, re.M):
                    if img not in seen[kind]:
                        seen[kind].append(img)
            if failures:
                break
    return {
        'scenario': scenario,
        'clean_cuts': clean_cuts,
        'sector_cuts': sector_cuts,
        'failures': failures,
        'seen': seen,
    }


def compile_ace(root, compiler, runtime, bridge, work, engine):
    work.mkdir(parents=True, exist_ok=True)
    manifest = work / 'inputs.json'
    manifest.write_text(json.dumps(crash_inputs(root)), encoding='utf-8')
    return build_crash(
        root, compiler, runtime, bridge, None, manifest, work, engine,
        'store@1/store/ace_crash_main', None, 'store_crash.c',
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, default=ROOT)
    parser.add_argument('--compiler', type=Path)
    parser.add_argument('--runtime', type=Path)
    parser.add_argument('--bridge', type=Path)
    parser.add_argument('--executable', type=Path)
    parser.add_argument('--engine', choices=['cranelift', 'llvm', 'llvm-release'], default='cranelift')
    parser.add_argument('--max-len', type=int, default=3)
    parser.add_argument('--damage-seeds', type=int, default=len(SECTOR_SEEDS))
    parser.add_argument('--workers', type=int, default=min(8, os.cpu_count() or 4))
    parser.add_argument('--list-counts', action='store_true')
    parser.add_argument('--report', type=Path)
    args = parser.parse_args()

    workloads = enumerate_workloads(args.max_len)
    by_len = {}
    for seq in workloads:
        by_len[str(len(seq))] = by_len.get(str(len(seq)), 0) + 1
    if args.list_counts:
        print(json.dumps({'alphabet': len(OPS), 'ops': OPS, 'by_length': by_len, 'total': len(workloads)}, indent=2))
        return 0
    if args.bridge is None:
        parser.error('--bridge is required')

    started = time.monotonic()
    with tempfile.TemporaryDirectory(prefix='hirsel-ace-') as tmp:
        if args.executable:
            executable = args.executable.resolve()
        else:
            if args.compiler is None or args.runtime is None:
                parser.error('supply --executable or --compiler and --runtime')
            executable = compile_ace(args.root.resolve(), args.compiler, args.runtime,
                                     args.bridge, Path(tmp) / 'build', args.engine)
        tasks = [(str(executable), str(args.bridge.resolve()), seq, args.damage_seeds) for seq in workloads]
        clean_cuts = 0
        sector_cuts = 0
        failures = []
        seen = {kind: set() for kind in SECTOR_KINDS}
        with ProcessPoolExecutor(max_workers=max(1, args.workers)) as pool:
            futures = [pool.submit(run_one_workload, t) for t in tasks]
            for fut in as_completed(futures):
                res = fut.result()
                clean_cuts += res['clean_cuts']
                sector_cuts += res['sector_cuts']
                failures.extend(res['failures'])
                for kind, imgs in res['seen'].items():
                    seen[kind].update(imgs)

    if not failures and args.damage_seeds >= len(SECTOR_SEEDS) and args.max_len >= 3:
        for kind, wanted in SECTOR_KINDS.items():
            if wanted not in seen[kind]:
                failures.append(f'vacuous sector coverage: {kind} missed {wanted} in {sorted(seen[kind])}')
    wall = round(time.monotonic() - started, 2)
    summary = {
        'alphabet_size': len(OPS),
        'max_len': args.max_len,
        'workloads_by_length': by_len,
        'total_workloads': len(workloads),
        'damage_seeds': args.damage_seeds,
        'clean_cuts_run': clean_cuts,
        'sector_cuts_run': sector_cuts,
        'total_cuts_run': clean_cuts + sector_cuts,
        'failed': len(failures),
        'failures': failures[:20],
        'seen_images': {k: sorted(v) for k, v in seen.items()},
        'wall_seconds': wall,
    }
    if args.report:
        args.report.parent.mkdir(parents=True, exist_ok=True)
        args.report.write_text(json.dumps(summary, indent=2, sort_keys=True) + '\n', encoding='utf-8')
    print(
        f'ACE <= {args.max_len} ops: {len(workloads)} workloads, '
        f'{clean_cuts} clean cuts + {sector_cuts} sector-damaged cuts '
        f'({clean_cuts + sector_cuts} total), {len(failures)} failed in {wall}s',
        flush=True,
    )
    if failures:
        for line in failures[:20]:
            print(f'  FAIL: {line}', file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
