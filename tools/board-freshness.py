#!/usr/bin/env python3
"""Publish the nightly board status and alert on missed complete runs."""
import argparse
from datetime import datetime, timezone
import fcntl
import json
import os
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


def last_complete(base, *, accepted_only=False):
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
        status = report.get("exit_status")
        markdown = path.with_suffix(".md")
        if (report.get("complete") is True and status in (0, 2, 3)
                and (not accepted_only or status != 2 or path == promoted)
                and markdown.is_file()):
            return finished, report["commit"], markdown
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


def attempt_order(run):
    second = run.name.split("-", 1)[0]
    for path in (run / "attempt.json", run / "board" / "board.json"):
        try:
            return second, path.stat().st_mtime_ns
        except FileNotFoundError:
            continue
    return second, 0


def latest_attempt(base):
    for run in sorted(base.glob("runs/*"), key=attempt_order, reverse=True):
        report = read_report(run / "board" / "board.json")
        if attempt := read_report(run / "attempt.json"):
            return attempt, report
        if report:
            started = datetime.strptime(run.name.split("-", 1)[0], "%Y%m%dT%H%M%SZ")
            status = report.get("exit_status")
            failures = ", ".join(report.get("failures", {}))
            return {
                "started": started.replace(tzinfo=timezone.utc).timestamp(),
                "commit": report.get("commit"), "status": status,
                "step": failures or ("board comparison" if status in (2, 3) else "board"),
            }, report
    return None, {}


def verdict(value):
    return "pending" if value is None else "yes" if value else "no"


def render(base, attempt, report, complete, now):
    lines = ["# Current Hirsel board", "", f"Freshness as of {timestamp(now)}.", ""]
    alert = board_alert(complete, now)
    board_status = report.get("exit_status")
    finished = board_status is not None or (attempt is not None and attempt["status"] is not None)
    recovered = board_status == 2 and attempt is not None and attempt["step"] == "rebaseline"
    lines += [
        f"- **Fresh** · {verdict(alert is None)}",
        f"- **Complete** · {verdict(report.get('complete', False if finished else None))}",
        f"- **Passed** · {verdict(board_status == 0 if finished else None)}",
        f"- **Refused** · {verdict(board_status == 2 if finished else None)}",
        f"- **Recovered** · {verdict(recovered)}",
        "",
    ]
    if attempt is None:
        lines.append("**Latest attempt** · No attempt recorded.")
    else:
        status = attempt["status"]
        step = attempt["step"]
        if status is None:
            outcome = f"Running auxiliary checks · {step}" if board_status is not None else f"Running · {step}"
        elif status == 0:
            outcome = f"Finished · {step} (exit 0)"
        else:
            comparison = {2: "Refused", 3: "Regression"} if step == "board comparison" else {}
            outcome = f"{comparison.get(status, 'Failed')} · {step} (exit {status})"
        commit = attempt["commit"] or "commit unavailable"
        lines.append(f"**Latest attempt** · `{commit}` · {timestamp(attempt['started'])} · {outcome}")
        auxiliary = [f"{label} exit {attempt[field]}" for field, label in
                     (("boat_status", "Boat"), ("vopr_status", "VOPR"), ("law_status", "Laws"),
                     ("media_status", "Media vectors"))
                     if attempt.get(field) is not None]
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
    if alert:
        lines += ["", f"**{alert}**"]
    if complete is not None:
        lines += ["", "---", "", complete[2].read_text().rstrip()]
    return "\n".join(lines) + "\n"


def atomic_write(path, content):
    with tempfile.TemporaryDirectory(dir=path.parent) as scratch:
        pending = Path(scratch) / path.name
        pending.write_text(content)
        pending.replace(path)


def service_failure(base, followups, tier="nightly"):
    now = datetime.now(timezone.utc)
    status = os.environ.get("MONITOR_EXIT_STATUS", "")
    if os.environ.get("MONITOR_EXIT_CODE") == "exited" and status in ("1", "2", "3"):
        outcome = {"1": "incomplete", "2": "refused", "3": "regressed"}[status]
        detail = f"{outcome} (exit {status})"
    else:
        result = os.environ.get("MONITOR_SERVICE_RESULT", "unknown")
        detail = f"failed ({result}{', status ' + status if status else ''})"
    invocation = os.environ.get("MONITOR_INVOCATION_ID")
    suffix = "-weekly" if tier == "weekly" else ""
    journal = (f"journalctl --user _SYSTEMD_INVOCATION_ID={invocation}" if invocation
               else f"journalctl --user -u hirsel-board{suffix}.service")
    line = f"{timestamp(now.timestamp())} Hirsel {tier} board {detail}. See `{journal}`."
    alerts = base / "alerts"
    alerts.mkdir(exist_ok=True)
    with (alerts / f"{now.date()}{suffix}.txt").open("a") as stream:
        stream.write(line + "\n")
    with followups.open("a") as stream:
        stream.write(f"\n- {line}\n")
    print(line)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", type=Path, required=True)
    parser.add_argument("--current", type=Path)
    parser.add_argument("--alert", action="store_true")
    parser.add_argument("--service-failure", type=Path, metavar="FOLLOWUPS",
                        help="record the failing systemd invocation")
    parser.add_argument("--service-failure-tier", choices=("nightly", "weekly"))
    parser.add_argument("--last-complete-commit", action="store_true")
    parser.add_argument("--record", type=Path, help="attempt run directory")
    parser.add_argument("--started", type=int)
    parser.add_argument("--commit")
    parser.add_argument("--step")
    parser.add_argument("--status", type=int, help="exit status, omitted while running")
    parser.add_argument("--boat-status", type=int)
    parser.add_argument("--vopr-status", type=int)
    parser.add_argument("--law-status", type=int)
    parser.add_argument("--media-status", type=int)
    args = parser.parse_args()
    if args.service_failure_tier and not args.service_failure:
        parser.error("--service-failure-tier requires --service-failure")
    args.base.mkdir(parents=True, exist_ok=True)
    with (args.base / "freshness.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        if args.service_failure:
            service_failure(args.base, args.service_failure, args.service_failure_tier or "nightly")
            return
        if args.record:
            if args.started is None or not args.step:
                parser.error("--record requires --started and --step")
            if args.step == "board" and args.status:
                report = read_report(args.record / "board" / "board.json")
                args.step = ", ".join(report.get("failures", {})) or args.step
            attempt = {"started": args.started, "commit": args.commit,
                       "step": args.step, "status": args.status,
                       "boat_status": args.boat_status, "vopr_status": args.vopr_status,
                       "law_status": args.law_status, "media_status": args.media_status}
            atomic_write(args.record / "attempt.json", json.dumps(attempt) + "\n")
        if args.last_complete_commit and (accepted := last_complete(args.base, accepted_only=True)):
            print(accepted[1])
        if args.current or args.alert:
            complete = last_complete(args.base)
            now = time.time()
            if args.current:
                attempt, report = latest_attempt(args.base)
                atomic_write(args.current, render(args.base, attempt, report, complete, now))
            if args.alert and (alert := board_alert(complete, now)):
                print(alert)


if __name__ == "__main__":
    main()
