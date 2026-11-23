#!/usr/bin/env python3
"""Publish the nightly board status and alert on missed complete runs."""
import argparse
from datetime import datetime, timezone
import fcntl
import json
from pathlib import Path
import subprocess
import tempfile
import time

MAX_AGE = 26 * 3600


def read_report(path):
    try:
        return json.loads(path.read_text())
    except FileNotFoundError:
        return {}


def last_complete(base):
    reports = []
    promoted = (base / "last-complete.json").resolve()
    for path in {promoted, *base.glob("runs/*/board/board.json")}:
        try:
            path = path.resolve()
            reports.append((path.stat().st_mtime, path))
        except FileNotFoundError:
            continue
    for finished, path in sorted(reports, reverse=True):
        report = read_report(path)
        if (report.get("complete") is True
                and (report.get("exit_status") in (0, 3) or path == promoted)
                and path.with_name("board.md").is_file()):
            return finished, report["commit"], path.with_name("board.md")
    return None


def board_alert(complete, now):
    if complete is None:
        return "BOARD-STALE age unavailable (no complete board)"
    age = now - complete[0]
    if age > MAX_AGE:
        return f"BOARD-STALE {hours(age)} old (>{hours(MAX_AGE)}; {complete[1][:12]})"
    return None


def hours(seconds):
    return f"{seconds / 3600:.1f} h"


def timestamp(seconds):
    return datetime.fromtimestamp(seconds, timezone.utc).strftime("%Y-%m-%d %H:%M:%S UTC")


def main_distance(base, commit):
    result = subprocess.run(
        ["git", "-C", base / "main", "rev-list", "--first-parent", "--count",
         f"{commit}..origin/main"], capture_output=True, text=True,
    )
    return f"{result.stdout.strip()} commits behind origin/main" if result.returncode == 0 else (
        "origin/main distance unavailable"
    )


def latest_attempt(base):
    for run in sorted(base.glob("runs/*"), reverse=True):
        if attempt := read_report(run / "attempt.json"):
            return attempt
        if report := read_report(run / "board" / "board.json"):
            started = datetime.strptime(run.name.split("-", 1)[0], "%Y%m%dT%H%M%SZ")
            status = report.get("exit_status")
            failures = ", ".join(report.get("failures", {}))
            return {
                "started": started.replace(tzinfo=timezone.utc).timestamp(),
                "commit": report.get("commit"), "status": status,
                "step": failures or ("board comparison" if status in (2, 3) else "board"),
            }
    return None


def render(base, attempt, complete, now):
    lines = ["# Current Hirsel board", "", f"Freshness as of {timestamp(now)}.", ""]
    if attempt is None:
        lines.append("**Latest attempt** · No attempt recorded.")
    else:
        status = attempt["status"]
        step = attempt["step"]
        if status is None:
            outcome = f"Running · {step}"
        elif status == 0:
            outcome = "Rebaselined · comparison refused" if step == "rebaseline" else "Success"
        else:
            comparison = {2: "Refused", 3: "Regression"} if step == "board comparison" else {}
            outcome = f"{comparison.get(status, 'Failed')} · {step} (exit {status})"
        commit = attempt["commit"] or "commit unavailable"
        lines.append(f"**Latest attempt** · `{commit}` · {timestamp(attempt['started'])} · {outcome}")
        auxiliary = [f"{label} exit {attempt[field]}" for field, label in
                     (("boat_status", "Boat"), ("vopr_status", "VOPR")) if attempt.get(field) is not None]
        if auxiliary:
            lines += ["", " · ".join(auxiliary)]
    lines.append("")
    if complete is None:
        lines.append("**Last complete board** · None.")
    else:
        finished, commit, path = complete
        lines += [f"**Last complete board** · `{commit}` · {timestamp(finished)} · "
                  f"{hours(now - finished)} old · {main_distance(base, commit)}.",
                  f"Stale after {timestamp(finished + MAX_AGE)}."]
    if alert := board_alert(complete, now):
        lines += ["", f"**{alert}**"]
    if complete is not None:
        lines += ["", "---", "", complete[2].read_text().rstrip()]
    return "\n".join(lines) + "\n"


def atomic_write(path, content):
    with tempfile.TemporaryDirectory(dir=path.parent) as scratch:
        pending = Path(scratch) / path.name
        pending.write_text(content)
        pending.replace(path)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", type=Path, required=True)
    parser.add_argument("--current", type=Path)
    parser.add_argument("--alert", action="store_true")
    parser.add_argument("--record", type=Path, help="attempt run directory")
    parser.add_argument("--started", type=int)
    parser.add_argument("--commit")
    parser.add_argument("--step")
    parser.add_argument("--status", type=int, help="exit status, omitted while running")
    parser.add_argument("--boat-status", type=int)
    parser.add_argument("--vopr-status", type=int)
    args = parser.parse_args()
    args.base.mkdir(parents=True, exist_ok=True)
    with (args.base / "freshness.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        if args.record:
            if args.started is None or not args.step:
                parser.error("--record requires --started and --step")
            if args.step == "board" and args.status:
                report = read_report(args.record / "board" / "board.json")
                args.step = ", ".join(report.get("failures", {})) or args.step
            attempt = {"started": args.started, "commit": args.commit,
                       "step": args.step, "status": args.status,
                       "boat_status": args.boat_status, "vopr_status": args.vopr_status}
            atomic_write(args.record / "attempt.json", json.dumps(attempt) + "\n")
        complete = last_complete(args.base)
        now = time.time()
        if args.current:
            atomic_write(args.current, render(args.base, latest_attempt(args.base), complete, now))
        if args.alert and (alert := board_alert(complete, now)):
            print(alert)


if __name__ == "__main__":
    main()
