#!/usr/bin/env python3
"""Check full-gate coverage from Buck2's combined test output, per profile."""
import argparse
from collections import Counter
from pathlib import Path
import re
import subprocess
import sys

ANSI = re.compile(r'\x1b\[[0-9;]*m')
TIMESTAMP = re.compile(r'^\[[^\]]+\]\s*')
RESULT = re.compile(r'^test (\S+)(?: - should panic)? \.\.\. (ok|FAILED|ignored)\b')


class Counts:
    def __init__(self):
        self.expected = set()
        self.actual = Counter()
        self.binary = None
        self.failures = []

    def observe(self, line):
        line = TIMESTAMP.sub('', ANSI.sub('', line)).strip()
        if line == '---- STDOUT ----':
            self.binary = None
        elif line.startswith('HIRSEL_FULL_GATE_BINARY '):
            self.binary = line.split()[1]
        elif line.startswith('HIRSEL_FULL_GATE_EXPECT '):
            _, binary, case = line.split()
            self.expected.add((binary, case))
        elif match := RESULT.match(line):
            case, status = match.groups()
            if self.binary is None:
                self.failures.append(f'{case}: missing binary inventory marker')
            elif status != 'ignored':
                self.actual[self.binary, case] += 1
                if status != 'ok':
                    self.failures.append(f'{self.binary} {case}: {status}')

    def finish(self):
        problems = list(self.failures)
        if not self.expected:
            problems.append('no compiled full-gate inventory found')
        for binary, case in sorted(self.expected | self.actual.keys()):
            count = self.actual[binary, case]
            if (binary, case) not in self.expected:
                problems.append(f'{binary} {case}: unexpected execution ({count})')
            elif count != 1:
                problems.append(f'{binary} {case}: expected once, observed {count}')
        if problems:
            raise ValueError('; '.join(problems))
        return len(self.expected)


def retained_inventory(root=None):
    root = root or Path(__file__).resolve().parents[2]
    groups = {}
    for line in (root / 'tools/retained-rust.tsv').read_text().splitlines()[1:]:
        binary, case = line.split('\t')
        groups.setdefault(binary, []).append(case)
    if not groups or any(len(set(cases)) != len(cases) for cases in groups.values()):
        raise ValueError('empty or duplicate retained Rust inventory')
    return {f'{binary}-retained-{i+1}': cases[i::4]
            for binary, cases in groups.items() for i in range(min(4, len(cases)))}


def retained_counts(outputs, inventory):
    expected = {(target, case) for target, cases in inventory.items() for case in cases}
    actual = Counter()
    errors = []
    seen = set()
    for name, output in outputs.items():
        match = re.search(r'(hirsel-[\w-]+-retained-\d+)', name)
        if not match:
            continue
        target = match[1]
        seen.add(target)
        for line in output.splitlines():
            line = TIMESTAMP.sub('', ANSI.sub('', line)).strip()
            if result := RESULT.match(line):
                case, status = result.groups()
                actual[target, case] += 1
                if status != 'ok':
                    errors.append(f'{target} {case}: {status}')
    if seen != set(inventory):
        errors.append(f'retained target inventory differs: missing {sorted(set(inventory)-seen)}, extra {sorted(seen-set(inventory))}')
    for key in sorted(expected | actual.keys()):
        if key not in expected or actual[key] != 1:
            errors.append(f'{key}: expected once, observed {actual[key]}')
    if errors:
        raise ValueError('; '.join(errors))
    return len(expected)


def run_retained():
    import event_logs
    from conformance import BUCK2, CONFIG, ROOT, shard_outputs, summarize
    inventory = retained_inventory(ROOT)
    targets = [*inventory, *[f'gate-conformance-shard-{i}' for i in range(1, 5)],
               *[f'gate-retained-{engine}-shard-{i}' for engine in ('heap', 'llvm', 'cranelift') for i in range(1, 5)]]
    with event_logs.recorded('retained-matrix') as log:
        status = subprocess.call([str(BUCK2), 'test', *CONFIG, '--event-log', str(log),
                                  *['//:' + target for target in targets]], cwd=ROOT)
        outputs = shard_outputs(log)
    try:
        count = retained_counts(outputs, inventory)
        print(f'retained Rust: {count}/{sum(map(len, inventory.values()))} bodies executed exactly once')
        corpus = {k: v for k, v in outputs.items() if 'gate-conformance-shard-' in k}
        print('\n'.join(summarize(corpus, 4)))
        for engine in ('heap', 'llvm', 'cranelift'):
            reports = [re.search(r'retained ' + engine + r': (\d+)/(\d+) passed', v)
                       for k, v in outputs.items() if f'gate-retained-{engine}-shard-' in k]
            if len(reports) != 4 or any(m is None or int(m[2]) == 0 for m in reports):
                raise ValueError(f'{engine}: missing or empty retained shard counts')
            passed = sum(int(m[1]) for m in reports)
            total = sum(int(m[2]) for m in reports)
            expected = len((ROOT / 'native-tests/regressions/smoke.txt').read_text().splitlines())
            print(f'retained {engine}: {passed}/{total} passed')
            if total != expected or passed != total:
                raise ValueError(f'{engine}: incomplete retained matrix')
    except ValueError as error:
        print(f'retained matrix: {error}', file=sys.stderr)
        return status or 1
    return status


def main():
    if sys.argv[1:] == ["--retained"]:
        return run_retained()
    parser = argparse.ArgumentParser(description=__doc__,
                                     usage='%(prog)s LOG | --run COMMAND ...')
    parser.add_argument('log', type=Path)
    # Forward the child argv verbatim: argparse treats Buck2's `--` separator
    # specially even with REMAINDER on an optional argument.
    command = sys.argv[2:] if sys.argv[1:2] == ['--run'] else None
    if command == []:
        parser.error('--run requires a command')
    log_path = None if command is not None else parser.parse_args().log
    counts = Counts()
    if command is not None:
        with subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                              text=True, errors='replace') as child:
            for line in child.stdout:
                print(line, end='', flush=True)
                counts.observe(line)
            status = child.wait()
        if status:
            return status
    else:
        with log_path.open(errors='replace') as log:
            for line in log:
                counts.observe(line)
    try:
        total = counts.finish()
    except ValueError as error:
        print(f'full gate coverage: {error}', file=sys.stderr)
        return 1
    print(f'full gate coverage: {total} compiled cases each executed exactly once across pool and host')
    return 0


if __name__ == '__main__':
    sys.exit(main())
