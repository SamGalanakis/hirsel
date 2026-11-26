#!/usr/bin/env python3
"""Append board runs to SQLite and detect per-row changes."""
import argparse
from collections import defaultdict, deque
from contextlib import closing
from datetime import datetime, timezone
import json
import math
import os
from pathlib import Path
import re
import sqlite3
from statistics import median
import sys

SCHEMA = """
CREATE TABLE IF NOT EXISTS runs (
    name TEXT PRIMARY KEY, started TEXT NOT NULL, commit_sha TEXT,
    complete INTEGER NOT NULL, exit_status INTEGER
);
CREATE TABLE IF NOT EXISTS samples (
    run TEXT REFERENCES runs(name) ON DELETE CASCADE,
    goal TEXT, row TEXT, side TEXT, metric TEXT, contract TEXT,
    value NUMERIC NOT NULL, reference NUMERIC,
    PRIMARY KEY (run, goal, row, side, metric)
);
"""
SUITES = {"AWFY original": "awfy", "AWFY pool/index": "pool", "LangArena": "arena",
          "Compile": "compile"}
NUMERIC = r"([0-9,.]+)"
COMPARE = re.compile(rf"(.+) {NUMERIC} -> {NUMERIC}( B/session| B| KiB)? \(.*")
TIME_WINDOW = 7
WEEKLY_GOAL = "Weekly capacity"


def read(path):
    try:
        return json.loads(path.read_text())
    except FileNotFoundError:
        return {}


def number(value):
    text = str(value).replace(",", "")
    if text.isdigit():
        return int(text)
    try:
        result = float(text)
        return result if math.isfinite(result) else None
    except ValueError:
        return None


def reference_pairs(report):
    lines = [*report.get("instruction_comparison", {}).get("details", []),
             *report.get("size_comparison", []), *report.get("checkbox_comparison", [])]
    pairs = {}
    for line in lines:
        match = COMPARE.fullmatch(line)
        if match:
            label, before, after, unit = match.groups()
            pairs[label, unit] = (number(before), number(after))
    return pairs


def samples(report):
    pairs = reference_pairs(report)
    receipts = {row["key"]: row for row in report.get("instruction_receipt", {}).get("rows", [])}
    for row in report.get("rows", []):
        goal, name = row["goal"], row["row"]
        key = f"{SUITES.get(goal, '')}/{name}"
        receipt = receipts.get(key, {})
        for side, sample in row["samples"].items():
            if sample.get("status") != "ok":
                continue
            fields = set(row["metrics"]) | ({"seconds", "max_rss_kib"} & sample.keys())
            contract = json.dumps([
                report.get("pins"),
                [receipt.get(field) for field in ("mode", "argv", "flags", "fixture_sha256")],
                sample.get("seed_version"),
                "server_idle_rss_bytes" in sample,
            ], sort_keys=True)
            for field in sorted(fields):
                metric = field
                value = number(sample.get(field))
                pair = None
                if field == "instructions" and side == "hirsel":
                    pair = pairs.get((key, None))
                elif field == "max_rss_kib" and side == "hirsel":
                    pair = pairs.get((f"{key} peak RSS", " KiB"))
                elif field == "bytes" and goal == "Size":
                    pair = pairs.get((side, " B"))
                elif field == "server_rss_bytes" and goal == "Checkbox":
                    metric = "rss_bytes_per_session"
                    pair = pairs.get((f"{name}/{side}", " B/session"))
                    if value is not None and sample.get("connections", 0) > 0:
                        value /= sample["connections"]
                    else:
                        value = None
                reference = None
                if pair and value is not None and math.isclose(value, pair[1], rel_tol=0, abs_tol=0.0051):
                    reference = pair[0]
                if value is not None and value >= 0:
                    yield goal, name, side, metric, contract, value, reference


def weekly_samples(report):
    contract = json.dumps(report["contract"], sort_keys=True)
    for label, item in report["metrics"].items():
        row, metric = label.rsplit("/", 1)
        value = number(item)
        if value is not None and value >= 0:
            yield WEEKLY_GOAL, row, "observed", metric, contract, value, None


def append(db, runs):
    for run in runs:
        if not run.is_dir():
            raise ValueError(f"{run}: expected a board run directory")
        with db:
            db.execute("BEGIN IMMEDIATE")
            if weekly := read(run / "weekly.json"):
                if "finished" not in weekly:
                    continue
                db.execute("INSERT OR REPLACE INTO runs VALUES (?, ?, ?, ?, ?)", (
                    run.name, weekly["started"], weekly["commit"], weekly["complete"], int(not weekly["complete"]),
                ))
                db.executemany("INSERT INTO samples VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                               ((run.name, *sample) for sample in weekly_samples(weekly)))
                continue
            report = read(run / "board/board.json")
            attempt = read(run / "attempt.json")
            started = attempt.get("started")
            if started is None:
                started = datetime.strptime(run.name.split("-", 1)[0], "%Y%m%dT%H%M%SZ").replace(
                    tzinfo=timezone.utc).timestamp()
            started = datetime.fromtimestamp(started, timezone.utc).isoformat()
            status = attempt.get("status")
            if status is None:
                status = report.get("exit_status")
            db.execute("INSERT OR REPLACE INTO runs VALUES (?, ?, ?, ?, ?)", (
                run.name, started, report.get("commit") or attempt.get("commit"),
                report.get("complete") is True, status,
            ))
            db.executemany("INSERT INTO samples VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                           ((run.name, *sample) for sample in samples(report)))


def step(metric, before, after):
    change = abs(after - before)
    if metric == "bytes":
        return change >= 2048
    limit = {"rss_bytes_per_session": 10, "max_rss_kib": 5}.get(metric, 2)
    return 100 * change > limit * before


def trend(values):
    if len(values) < TIME_WINDOW:
        return 0
    pairs = [(right - left, j - i) for i, left in enumerate(values)
             for j, right in enumerate(values) if j > i]
    score = sum((delta > 0) - (delta < 0) for delta, _ in pairs)
    slope = median(delta / span for delta, span in pairs)
    # Require agreement across the window and a change large enough to matter.
    if abs(score) >= 15 and abs(slope) * (TIME_WINDOW - 1) > 0.05 * median(values):
        return (slope > 0) - (slope < 0)
    return 0


def weekly_change(values):
    if len(values) < TIME_WINDOW:
        return 0, None, None
    earlier = list(values)
    old, new = earlier[:4], earlier[4:]
    direction = int(min(new) > max(old)) - int(max(new) < min(old))
    return direction, median(old), median(new)


def detect(db, only=None):
    windows = defaultdict(lambda: deque(maxlen=TIME_WINDOW))
    directions = {}
    query = """
        SELECT run, commit_sha, goal, row, side, metric, contract, value, reference
        FROM samples JOIN runs ON runs.name = samples.run
        ORDER BY runs.started, run, goal, row, side, metric
    """
    for run, commit, goal, row, side, metric, contract, value, reference in db.execute(query):
        key = (goal, row, side, metric, contract)
        window = windows[key]
        if goal == WEEKLY_GOAL:
            window.append(value)
            direction, before, value = weekly_change(window)
            changed = direction and direction != directions.get(key)
            directions[key] = direction
            kind = "change-point"
        elif metric in ("seconds", "wall_seconds"):
            window.append(value)
            direction = trend(window)
            changed = direction and direction != directions.get(key)
            directions[key] = direction
            before = window[0]
            kind = "trend"
        else:
            before = reference if reference is not None else (window[-1] if window else None)
            changed = before is not None and step(metric, before, value)
            window.append(value)
            kind = "step"
        if changed and (only is None or run == only):
            print(json.dumps(dict(run=run, commit=commit, goal=goal, row=row, side=side,
                                  metric=metric, kind=kind, before=before, after=value)))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    default = Path(os.environ.get("HIRSEL_BOARD_BASE", Path.home() / ".local/share/hirsel-board"))
    for command in ("append", "detect"):
        subparser = commands.add_parser(command)
        subparser.add_argument("--base", type=Path, default=default)
        if command == "append":
            subparser.add_argument("run", type=Path, nargs="?", help="omit to backfill nightly and weekly runs")
        else:
            subparser.add_argument("--run", help="print changes seen at one run")
    args = parser.parse_args()
    try:
        args.base.mkdir(parents=True, exist_ok=True)
        with closing(sqlite3.connect(args.base / "history.sqlite")) as db:
            db.execute("PRAGMA foreign_keys = ON")
            db.executescript(SCHEMA)
            if args.command == "append":
                weekly_runs = [path.parent for path in args.base.glob("weekly-runs/*/weekly.json")]
                append(db, [args.run] if args.run else sorted([*args.base.glob("runs/*/"), *weekly_runs]))
            else:
                detect(db, args.run)
    except (OSError, ValueError, KeyError, TypeError, sqlite3.Error) as error:
        print(f"board history: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
