#!/usr/bin/env python3
"""One serial Rust sample per workload/layout/profile/operation/cache configuration."""
import argparse
import csv
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[1]
REPO = ROOT.parent
TOOLCHAIN = '1.99.0'
TARGET = 'x86_64-unknown-linux-gnu'


def source_lines(path):
    """Rust physical lines with code, excluding blank and comment-only lines.

    Strings, raw strings and character literals remain code. Block comments nest.
    Lifetimes are ordinary tokens. No macro expansion or generated code is counted.
    """
    s = path.read_text()
    code, block, quote, raw, escape = [], 0, None, None, False
    line_has_code, i = False, 0
    while i < len(s):
        c = s[i]
        if c == '\n':
            code.append(line_has_code)
            line_has_code = quote is not None or raw is not None
            i += 1
            continue
        if block:
            if s.startswith('/*', i):
                block += 1
                i += 2
            elif s.startswith('*/', i):
                block -= 1
                i += 2
            else:
                i += 1
            continue
        if raw is not None:
            if s.startswith('"' + raw, i):
                i += 1 + len(raw)
                raw = None
            else:
                i += 1
            continue
        if quote:
            if escape:
                escape = False
            elif c == '\\':
                escape = True
            elif c == quote:
                quote = None
            i += 1
            continue
        if s.startswith('//', i):
            end = s.find('\n', i)
            i = len(s) if end < 0 else end
        elif s.startswith('/*', i):
            block = 1
            i += 2
        else:
            if not c.isspace():
                line_has_code = True
            match = re.match(r'(?:br|cr|r)(#*)"', s[i:])
            if match:
                raw = match[1]
                i += len(match[0])
                continue
            if c == '"':
                quote = c
            elif c == "'" and re.match(r"'(?:\\(?:u\{[0-9a-fA-F_]+\}|x[0-9a-fA-F]{2}|.)|[^'\\\n])'", s[i:]):
                quote = c
            i += 1
    if line_has_code:
        code.append(True)
    return sum(code)


def sources(name, layout):
    base = ROOT/'twins'
    if layout == 'matched':
        modules = {'json': ['matched_json.rs', 'matched_float.rs', 'matched_sort.rs'],
                   'pipeline': ['matched_sort.rs']}
        return [base/name/'matched.rs', base/'matched_support.rs', base/'matched_storage.rs',
                *([base/'matched_map.rs'] if name in ['lru', 'request-scratch'] else []),
                *(base/name/module for module in modules.get(name, []))]
    return [base/name/(layout+'.rs'), base/name/'program.rs', base/'support.rs']


def sample(command, directory):
    directory.mkdir()
    perf = directory/'perf.csv'
    stats = directory/'time.txt'
    # perf runs INSIDE capped. Wrapping a systemd launcher with perf loses its reparented children.
    launch = [str(REPO/'tools/capped'), 'perf', 'stat', '-x', ';', '-e', 'instructions:u,duration_time', '-o', str(perf), '--', '/usr/bin/time', '-f', '%e\t%M', '-o', str(stats), *map(str, command)]
    started = time.monotonic()
    with (directory/'stdout').open('wb') as out, (directory/'stderr').open('wb') as err:
        result = subprocess.run(launch, stdout=out, stderr=err)
    elapsed = time.monotonic()-started
    instructions, seconds, rss = '', '', ''
    status = 'ok' if result.returncode == 0 else f'exit:{result.returncode}'
    if stats.exists():
        fields = stats.read_text().strip().splitlines()[-1].split('\t')
        if len(fields) == 2:
            seconds, rss = fields
    if perf.exists():
        for line in perf.read_text().splitlines():
            fields = line.split(';')
            if len(fields) > 2 and fields[2] == 'duration_time' and fields[0].strip().isdigit():
                seconds = f'{int(fields[0])/1e9:.9f}'
            if len(fields) > 2 and fields[2] == 'instructions:u':
                if fields[0].strip().isdigit():
                    instructions = int(fields[0])
                else:
                    status = 'missing-counter' if status == 'ok' else status
    if not instructions:
        status = 'missing-counter' if status == 'ok' else status
    stderr = (directory/'stderr').read_text()
    if 'kernel killed' in stderr or result.returncode in (137, -9):
        status = 'memory-censored'
    peak = re.search(r'capped: peak (\d+) MiB of', stderr)
    return dict(status=status, instructions=instructions, seconds=seconds, max_rss_kib=rss, tree_peak_mib=peak[1] if peak else '', wrapper_seconds=f'{elapsed:.6f}', directory=directory)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--out', type=Path, required=True, help='new evidence directory outside the repo')
    parser.add_argument('--table', type=Path, default=ROOT/'BASELINE.tsv')
    parser.add_argument('--only', nargs='+')
    args = parser.parse_args()
    out = args.out.resolve()
    if out.exists():
        parser.error('--out must be new; never overwrite an earlier sample')
    if out.is_relative_to(REPO):
        parser.error('evidence goes outside the repository')
    # The serial instruction baseline uses one permitted CPU. Rust worker/link children inherit it.
    if 16 not in os.sched_getaffinity(0):
        parser.error('this baseline requires host CPU 16')
    os.sched_setaffinity(0, {16})
    os.environ['HIRSEL_MEMORY_LIMIT'] = '2G'
    os.environ['HIRSEL_JOBS'] = '1'
    out.mkdir(parents=True)
    rustc = subprocess.check_output(['rustup', 'which', '--toolchain', TOOLCHAIN, 'rustc'], text=True).strip()
    identity = subprocess.check_output([rustc, '-vV'], text=True)
    metadata = dict(toolchain=identity, target=TARGET, cpu=16, uname=dict(system=os.uname().sysname, release=os.uname().release, machine=os.uname().machine), measured_base=subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=REPO, text=True).strip(), source_state='working tree; final source committed with table', flags=['edition=2024', 'target-cpu=x86-64', 'overflow-checks=on', 'codegen-units=1', 'incremental=off'], sample_policy='one per configuration; no timeout; no system page-cache flush', cpu_model=subprocess.check_output(['lscpu'], text=True))
    (out/'environment.json').write_text(json.dumps(metadata, indent=2)+'\n')
    rows = []
    for name in sorted(p.name for p in (ROOT/'twins').iterdir() if (p/'Cargo.toml').exists()):
        if args.only and name not in args.only:
            continue
        for layout in ('idiomatic', 'matched'):
            line_count = sum(source_lines(p) for p in sources(name, layout))
            for profile, opt in [('dev', '0'), ('release', '3')]:
                binaries = {}
                for operation in ('check', 'build', 'run'):
                    for cache in ('cold', 'warm'):
                        tag = f'{name}-{layout}-{profile}-{operation}-{cache}'
                        directory = out/tag
                        if operation == 'run':
                            fixture = ROOT/'corpus/m1'/name
                            command = [binaries.get('warm', binaries.get('cold', '')), fixture/'inputs/normal.txt', 'normal']
                        else:
                            artifact = out/(tag+'.artifact')
                            command = [rustc, str(sources(name, layout)[0]), '--crate-name', 'm1_'+name.replace('-', '_'), '--edition=2024', '--target', TARGET, '-C', 'target-cpu=x86-64', '-C', 'overflow-checks=on', '-C', 'codegen-units=1', '-C', f'opt-level={opt}', '-C', 'debug-assertions='+('yes' if profile == 'dev' else 'no'), '-C', 'debuginfo=0', '--emit='+('metadata' if operation == 'check' else 'link'), '-o', str(artifact)]
                        if operation == 'run' and not command[0]:
                            record = dict(status='missing-build', instructions='', seconds='', max_rss_kib='', tree_peak_mib='', wrapper_seconds='')
                        else:
                            record = sample(command, directory)
                        if operation == 'build' and record['status'] == 'ok':
                            binaries[cache] = artifact
                        if operation == 'run' and record['status'] == 'ok':
                            if (directory/'stdout').read_bytes() != (fixture/'EXPECTED/normal.stdout').read_bytes():
                                record['status'] = 'wrong-output'
                        row = dict(workload=name, layout=layout, profile=profile, operation=operation, cache=cache, lines=line_count, input_bytes=(ROOT/'corpus/m1'/name/'inputs/normal.txt').stat().st_size, **{k:v for k,v in record.items() if k != 'directory'})
                        row['instructions_per_line'] = f'{row["instructions"]/line_count:.3f}' if row['instructions'] and operation != 'run' else ''
                        rows.append(row)
                        print(f'{tag}: {row["status"]}, {row["instructions"]} instructions, {row["seconds"]} s', flush=True)
                        with args.table.open('w') as stream:
                            writer = csv.DictWriter(stream, fieldnames=[k for k in rows[0] if k != 'wrapper_seconds']+['wrapper_seconds'], delimiter='\t', lineterminator='\n')
                            writer.writeheader()
                            writer.writerows(rows)
                for artifact in out.glob(f'{name}-{layout}-{profile}-*.artifact'):
                    artifact.unlink()
    (ROOT/'BASELINE-ENVIRONMENT.json').write_text(json.dumps(metadata, indent=2)+'\n')
    print(f'{len(rows)} configurations, {sum(r["status"] == "ok" for r in rows)} complete')
    return int(any(r['status'] != 'ok' for r in rows))


if __name__ == '__main__':
    sys.exit(main())
