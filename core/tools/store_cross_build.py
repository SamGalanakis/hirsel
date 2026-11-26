#!/usr/bin/env python3
"""T7 cross-build reopen: keep last-green W2 crash-cut and mid-import states
and verify tonight's build opens each one as a valid prefix with walker+verify
clean. Keeps only the last green set, bounded on disk.
"""
import argparse
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

sys.path.insert(0, str(Path(__file__).resolve().parent))
from std_native import ROOT
from store_crash_tests import bridge_env, compile_damage, count_cuts
from store_freeze_corpus import current_format

# Disk scenarios cover Directory cuts, migrations, branches, Sums/Many index
# rebuilds, and prepared-export/mid-import/cutover states (`install`).
DISK_SCENARIOS = (
    'checkpoint', 'gap', 'oversize', 'schema', 'refusal',
    'genesis', 'branches', 'migrate', 'install', 'index-build',
)
JOURNAL_SCENARIOS = ('journal', 'journal-group', 'journal-rotate', 'journal-fork')
SCENARIO_CUT_CAPS = {'install': 24}
DEFAULT_CUT_CAP = 16
MAX_DISK_BYTES = 64 * 1024 * 1024


def select_cuts(total, cap):
    """Deterministically pick up to `cap` cuts across 0..=total, always keeping
    boundaries (0, 1, total - 1, total) and evenly spaced interior cuts."""
    if total + 1 <= cap:
        return list(range(total + 1))
    picked = {0, 1, total - 1, total}
    slots = cap - len(picked)
    if slots > 0:
        for i in range(1, slots + 1):
            cut = round(i * total / (slots + 1))
            picked.add(cut)
    return sorted(picked)


def dir_size(path):
    return sum(p.stat().st_size for p in path.rglob('*') if p.is_file())


def build_w2_binaries(compiler, runtime, bridge, out_dir, engine='cranelift'):
    out_dir.mkdir(parents=True, exist_ok=True)
    built = {}
    with tempfile.TemporaryDirectory(prefix='w2-xbuild-') as scratch:
        ns = argparse.Namespace(compiler=compiler, runtime=runtime, bridge=bridge)
        for kind in ('disk', 'journal'):
            exe = compile_damage(ns, Path(scratch) / kind, kind, engine)
            dst = out_dir / kind
            shutil.copy2(exe, dst)
            built[kind] = dst
    return built


def resolve_binaries(args):
    if args.disk_bin and args.journal_bin:
        return {'disk': args.disk_bin.resolve(), 'journal': args.journal_bin.resolve()}
    if not (args.compiler and args.runtime):
        raise ValueError('supply either --disk-bin/--journal-bin or --compiler/--runtime')
    target = args.stage_bins.resolve() if args.stage_bins else Path(tempfile.mkdtemp(prefix='w2-bins-'))
    return build_w2_binaries(args.compiler, args.runtime, args.bridge, target, args.engine)


def run_capture(args):
    bins = resolve_binaries(args)
    env = bridge_env(args.bridge)
    stage = args.stage.resolve()
    if stage.exists():
        shutil.rmtree(stage)
    stage.mkdir(parents=True)
    fmt = current_format(args.root.resolve())
    cases = []
    total_bytes = 0
    capped = False
    plan = [('disk', s) for s in DISK_SCENARIOS] + [('journal', s) for s in JOURNAL_SCENARIOS]
    for kind, scenario in plan:
        exe = bins[kind]
        count, _ = count_cuts(exe, scenario, args.engine, env)
        cap = min(SCENARIO_CUT_CAPS.get(scenario, DEFAULT_CUT_CAP), args.max_cuts)
        for cut in select_cuts(count, cap):
            case_id = f'{kind}-{scenario}-cut{cut}'
            dump_dir = stage / case_id
            run_env = dict(env, HIRSEL_W2_DUMP_DIR=str(dump_dir))
            ran = subprocess.run(
                [str(exe), scenario, str(cut)],
                env=run_env, capture_output=True, text=True, timeout=300,
            )
            if ran.returncode != 0:
                raise RuntimeError(
                    f'capture {case_id} exited {ran.returncode}\n{ran.stdout}\n{ran.stderr}'
                )
            if not (dump_dir / 'meta.txt').is_file():
                raise RuntimeError(f'capture {case_id} produced no snapshot metadata')
            size = dir_size(dump_dir)
            if total_bytes + size > args.max_bytes:
                shutil.rmtree(dump_dir)
                capped = True
                break
            total_bytes += size
            cases.append({
                'id': case_id,
                'kind': kind,
                'scenario': scenario,
                'cut': cut,
                'bytes': size,
            })
        if capped:
            break
    if not cases:
        raise RuntimeError('capture produced zero cases within disk budget')
    manifest = {
        'commit': args.commit,
        'format': fmt,
        'captured_utc': datetime.now(timezone.utc).isoformat(),
        'total_bytes': total_bytes,
        'max_bytes': args.max_bytes,
        'capped': capped,
        'cases': cases,
    }
    (stage / 'manifest.json').write_text(json.dumps(manifest, indent=2, sort_keys=True) + '\n', encoding='utf-8')
    print(f'captured {len(cases)} W2 states ({total_bytes} B, format {fmt}) -> {stage}', flush=True)
    return 0


def run_reopen(args):
    bins = resolve_binaries(args)
    env = bridge_env(args.bridge)
    saved = args.saved.resolve()
    output = args.output.resolve()
    output.parent.mkdir(parents=True, exist_ok=True)
    manifest_path = saved / 'manifest.json'
    fmt = current_format(args.root.resolve())

    if not manifest_path.is_file():
        row = {'kind': 'summary', 'status': 'bootstrap_empty', 'current_format': fmt,
               'checked': 0, 'failed': 0}
        output.write_text(json.dumps(row, sort_keys=True) + '\n', encoding='utf-8')
        print('cross-build reopen: no prior last-green set (bootstrap)', flush=True)
        return 0

    manifest = json.loads(manifest_path.read_text(encoding='utf-8'))
    saved_fmt = manifest['format']
    saved_commit = manifest.get('commit', '')
    cases = manifest.get('cases', [])

    if saved_fmt != fmt:
        # No-shims policy: a retired format must refuse cleanly (fail closed),
        # never silently open. Check one saved non-empty case and allow the
        # green night to replace last-green-w2 with the new format.
        refused = True
        sample = next((c for c in cases if (saved / c['id'] / 'fs' / 'MANIFEST').is_file()), None)
        if sample is not None:
            run_env = dict(env, HIRSEL_W2_LOAD_DIR=str(saved / sample['id']))
            ran = subprocess.run(
                [str(bins[sample['kind']]), sample['scenario'], 'count'],
                env=run_env, capture_output=True, text=True, timeout=300,
            )
            refused = ran.returncode != 0
        status = 'format_retired' if refused else 'retired_format_accepted'
        row = {
            'kind': 'summary',
            'status': status,
            'saved_commit': saved_commit,
            'saved_format': saved_fmt,
            'current_format': fmt,
            'checked': 1 if sample is not None else 0,
            'failed': 0 if refused else 1,
        }
        output.write_text(json.dumps(row, sort_keys=True) + '\n', encoding='utf-8')
        print(f'cross-build reopen: format {saved_fmt} -> {fmt} ({status})', flush=True)
        return 0 if refused else 1

    failed = 0
    with output.open('w', encoding='utf-8') as log:
        for case in cases:
            case_dir = saved / case['id']
            run_env = dict(env, HIRSEL_W2_LOAD_DIR=str(case_dir))
            exe = bins[case['kind']]
            try:
                ran = subprocess.run(
                    [str(exe), case['scenario'], 'count'],
                    env=run_env, capture_output=True, text=True, timeout=300,
                )
                ok = ran.returncode == 0
                row = {
                    'kind': 'case',
                    'id': case['id'],
                    'scenario': case['scenario'],
                    'cut': case['cut'],
                    'passed': ok,
                    'exit_status': ran.returncode,
                }
                if not ok:
                    row['stderr'] = ran.stderr[-2000:]
            except subprocess.SubprocessError as error:
                ok = False
                row = {'kind': 'case', 'id': case['id'], 'passed': False, 'error': str(error)}
            if not ok:
                failed += 1
            print(json.dumps(row, sort_keys=True), file=log, flush=True)
        summary = {
            'kind': 'summary',
            'status': 'ok' if failed == 0 else 'failed',
            'saved_commit': saved_commit,
            'saved_format': saved_fmt,
            'current_format': fmt,
            'checked': len(cases),
            'failed': failed,
        }
        print(json.dumps(summary, sort_keys=True), file=log, flush=True)
    print(f'cross-build reopen ({saved_commit[:8]}): {len(cases) - failed}/{len(cases)} passed', flush=True)
    return int(failed != 0)


def promote(stage, target, max_bytes=MAX_DISK_BYTES):
    stage = stage.resolve()
    target = target.resolve()
    manifest_path = stage / 'manifest.json'
    if not manifest_path.is_file():
        raise ValueError(f'missing manifest in staging dir: {stage}')
    size = dir_size(stage)
    if size > max_bytes:
        raise ValueError(f'staged set ({size} B) exceeds disk cap ({max_bytes} B)')
    target.parent.mkdir(parents=True, exist_ok=True)
    incoming = target.parent / f'.{target.name}.next-{os.getpid()}'
    previous = target.parent / f'.{target.name}.prev-{os.getpid()}'
    if incoming.exists():
        shutil.rmtree(incoming)
    if previous.exists():
        shutil.rmtree(previous)
    shutil.copytree(stage, incoming)
    if target.exists():
        target.rename(previous)
    try:
        incoming.rename(target)
    except BaseException:
        if previous.exists() and not target.exists():
            previous.rename(target)
        raise
    if previous.exists():
        shutil.rmtree(previous)
    # Remove any leftover sibling backups so only the last green set remains.
    for leftover in target.parent.glob(f'.{target.name}.prev-*'):
        if leftover.is_dir():
            shutil.rmtree(leftover)
    print(f'promoted last-green W2 set ({size} B) -> {target}', flush=True)
    return 0


def add_bin_args(parser):
    parser.add_argument('--root', type=Path, default=ROOT)
    parser.add_argument('--bridge', type=Path, required=True)
    parser.add_argument('--compiler', type=Path)
    parser.add_argument('--runtime', type=Path)
    parser.add_argument('--disk-bin', type=Path)
    parser.add_argument('--journal-bin', type=Path)
    parser.add_argument('--stage-bins', type=Path)
    parser.add_argument('--engine', choices=['cranelift', 'llvm', 'llvm-release'], default='cranelift')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='command', required=True)

    p_cap = sub.add_parser('capture')
    add_bin_args(p_cap)
    p_cap.add_argument('--commit', required=True)
    p_cap.add_argument('--stage', type=Path, required=True)
    p_cap.add_argument('--max-bytes', type=int, default=MAX_DISK_BYTES)
    p_cap.add_argument('--max-cuts', type=int, default=24)

    p_reopen = sub.add_parser('reopen')
    add_bin_args(p_reopen)
    p_reopen.add_argument('--saved', type=Path, required=True)
    p_reopen.add_argument('--output', type=Path, required=True)

    p_prom = sub.add_parser('promote')
    p_prom.add_argument('--stage', type=Path, required=True)
    p_prom.add_argument('--target', type=Path, required=True)
    p_prom.add_argument('--max-bytes', type=int, default=MAX_DISK_BYTES)

    args = parser.parse_args()
    if args.command == 'capture':
        return run_capture(args)
    if args.command == 'reopen':
        return run_reopen(args)
    return promote(args.stage, args.target, args.max_bytes)


if __name__ == '__main__':
    raise SystemExit(main())
