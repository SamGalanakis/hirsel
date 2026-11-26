#!/usr/bin/env python3
"""Locate the first-parent instruction gate crossing with matched Boat screen jobs."""
import argparse
import csv
from decimal import Decimal
import json
from pathlib import Path
import re
import subprocess
import sys
import tempfile

import screen
from board import INSTRUCTION_LIMIT

ROOT = Path(__file__).resolve().parents[2]
PREPARE = ("import runpy,sys; sys.path.insert(0, 'bench/awfy'); "
           "sys.argv.pop(0); runpy.run_path(sys.argv[0], run_name='__main__')")
FAILED = re.compile(r"(.+) ([\d,]+) -> ([\d,]+) \([+-][\d.]+%\) FAIL")


def git(*args, cwd=ROOT):
    return subprocess.check_output(["git", *args], cwd=cwd, text=True).strip()


def revisions(good, bad):
    good, bad = (git("rev-parse", "--verify", "--end-of-options", f"{ref}^{{commit}}")
                 for ref in (good, bad))
    chain = git("rev-list", "--first-parent", "--reverse", f"{good}..{bad}").splitlines()
    if not chain or git("rev-parse", f"{chain[0]}^1") != good:
        raise ValueError("baseline must precede the board commit on its first-parent history")
    return [good, *chain]


def board_rows(baseline, candidate):
    good, bad = (json.loads(path.read_text()) for path in (baseline, candidate))
    if good["complete"] is not True or bad["exit_status"] != 3:
        raise ValueError("expected a complete baseline and a board gate failure")
    counts = [{row["key"]: row.get("sample", {}).get("instructions")
               for row in board.get("instruction_receipt", {}).get("rows", [])} for board in (good, bad)]
    rows = []
    for line in bad.get("instruction_comparison", {}).get("details", []):
        match = FAILED.fullmatch(line)
        if match and match[1].partition("/")[0] in ("awfy", "pool", "arena"):
            observed = [int(value.replace(",", "")) for value in match.groups()[1:]]
            if observed != [arm.get(match[1]) for arm in counts]:
                raise ValueError(f"{match[1]}: comparison counts differ from the supplied board receipts")
            rows.append(screen.workload(match[1]))
    return good["commit"], bad["commit"], rows


class ScreenProbes:
    def __init__(self, args, good, scratch):
        self.args = args
        self.good = good
        self.scratch = scratch
        self.checkout = scratch / "checkout"

    def prepare(self, commit, selected):
        out = self.scratch / commit
        if not self.checkout.exists():
            git("worktree", "add", "--detach", self.checkout, commit)
        else:
            git("switch", "--detach", commit, cwd=self.checkout)
        rows = [part for suite, name in selected for part in ("--row", f"{suite}/{name}")]
        with (self.args.out / f"{commit[:12]}-build.log").open("w") as log:
            # Use each revision's compiler and workload adapters with the current screen driver.
            subprocess.run([sys.executable, "-c", PREPARE, screen.__file__, "prepare", out,
                            "--awfy", self.args.awfy, "--langarena", self.args.langarena, *rows],
                           cwd=self.checkout, stdout=log, stderr=subprocess.STDOUT, check=True)
        return out

    def close(self):
        if self.checkout.exists():
            buck = self.checkout / ".buck2/bin/buck2"
            try:
                if buck.exists():
                    subprocess.run([buck, "kill"], cwd=self.checkout, check=True)
            finally:
                git("worktree", "remove", "--force", self.checkout)

    def measure(self, base, candidate, commit, rows, tag):
        print(f"Boat screen {self.good[:12]}..{commit[:12]}: {', '.join('/'.join(row) for row in rows)}",
              flush=True)
        with tempfile.TemporaryDirectory(dir=self.scratch) as temporary:
            bundle = Path(temporary)
            args = argparse.Namespace(base=base, candidate=candidate, awfy=self.args.awfy)
            job = screen.screen_job(args, bundle, rows, instructions_only=True)
            out = self.args.out / tag
            subprocess.run([ROOT / "tools/boat-dispatch", "--log-dir", self.args.out / "boat-logs",
                            "--fetch", f"instructions.tsv:{out}",
                            job["lane"], job["bundle"], job["command"]], check=True)
            return read_counts(out / "instructions.tsv", dict(base=self.good, candidate=commit), rows)


def read_counts(path, commits, rows):
    counts = {}
    expected = {"/".join(row) for row in rows}
    with path.open() as source:
        reader = csv.DictReader(source, delimiter="\t")
        if reader.fieldnames != ["workload", "arm", "commit", "instructions"]:
            raise ValueError(f"{path}: unexpected Boat screen columns")
        for record in reader:
            row, arm = record["workload"], record["arm"]
            count = int(record["instructions"])
            if (row not in expected or commits.get(arm) != record["commit"] or
                    (row, arm) in counts or count <= 0):
                raise ValueError(f"{path}: unexpected Boat screen row {record}")
            counts[row, arm] = count
    if len(counts) != len(rows) * len(commits):
        raise ValueError(f"{path}: incomplete Boat screen")
    return {row: tuple(counts["/".join(row), arm] for arm in screen.ARMS) for row in rows}


def bisect(commits, rows, measure, limit):
    """Bisect a gate that stays failed once crossed on the first-parent history."""
    def failed(pair):
        return 100 * (pair[1] - pair[0]) > limit * pair[0]

    end = len(commits) - 1
    tip = measure(commits[end], rows)
    bounds = {}
    for row, pair in tip.items():
        if failed(pair):
            bounds[row] = (0, end)
        else:
            yield False, f"Unresolved {'/'.join(row)}: Boat did not reproduce the gate ({pair[0]} -> {pair[1]} instructions)"
    while bounds:
        row = next(iter(bounds))
        lo, hi = bounds[row]
        if hi == lo + 1:
            yield True, f"Culprit {commits[hi]} {'/'.join(row)} (first-parent +{limit}% instruction crossing)"
            del bounds[row]
            continue
        mid = (lo + hi) // 2
        selected = [key for key, (left, right) in bounds.items()
                    if right > left + 1 and (left + right) // 2 == mid]
        for key, pair in measure(commits[mid], selected).items():
            left, right = bounds[key]
            bounds[key] = (left, mid) if failed(pair) else (mid, right)


def run(args, good, bad, rows):
    commits = revisions(good, bad)
    args.out.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="board-bisect-", dir=args.out) as temporary:
        probes = ScreenProbes(args, commits[0], Path(temporary))
        try:
            base = probes.prepare(commits[0], rows)
            job = 0

            def measure(commit, selected):
                nonlocal job
                job += 1
                candidate = probes.prepare(commit, selected)
                return probes.measure(base, candidate, commit, selected, f"probe-{job:03d}-{commit[:12]}")

            yield from bisect(commits, rows, measure, args.limit)
        finally:
            probes.close()


def announce(path, line):
    print(line, flush=True)
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a") as alert:
        alert.write(line + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("out", type=lambda value: Path(value).resolve())
    parser.add_argument("--alert", type=Path, required=True)
    parser.add_argument("--awfy", type=lambda value: Path(value).resolve(), required=True)
    parser.add_argument("--langarena", type=lambda value: Path(value).resolve(), required=True)
    parser.add_argument("--limit", type=Decimal, default=INSTRUCTION_LIMIT)
    parser.add_argument("--board", type=Path)
    parser.add_argument("--baseline", type=Path)
    parser.add_argument("--good")
    parser.add_argument("--bad")
    parser.add_argument("--row", type=screen.workload, action="append")
    args = parser.parse_args()
    if not args.limit.is_finite() or args.limit <= 0:
        parser.error("--limit must be finite and positive")
    try:
        if args.board and args.baseline and not any((args.good, args.bad, args.row)):
            good, bad, rows = board_rows(args.baseline, args.board)
        elif args.good and args.bad and args.row and not any((args.board, args.baseline)):
            good, bad, rows = args.good, args.bad, args.row
        else:
            parser.error("supply --baseline and --board, or --good, --bad and --row")
        if not rows:
            print("No runtime instruction rows to bisect.")
            return 0
        status = 0
        for found, line in run(args, good, bad, sorted(set(rows))):
            announce(args.alert, line)
            if not found:
                status = 1
        return status
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
        announce(args.alert, f"Unresolved board bisect: {error}")
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
