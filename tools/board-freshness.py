#!/usr/bin/env python3
"""Publish nightly board status, alert when it goes stale, and prune old runs."""
import argparse
from contextlib import closing
from datetime import datetime, timezone
import fcntl
import json
import os
from pathlib import Path
import re
import shutil
import sqlite3
import stat
import subprocess
import tempfile
import time

MAX_AGE = 26 * 3600
NIGHTLY_DIR = Path(os.environ.get("HIRSEL_BRIEFS", "/workspace/code/hirsel-briefs")) / "vopr/nightly"
KEEP_RUNS = 3
RUN_NAME = r"[0-9]{8}T[0-9]{6}Z-[A-Za-z0-9]{6}"
RUN_REFERENCE = re.compile(rf"(?<![\w.-])runs/({RUN_NAME})(?![\w.-])")
AUXILIARY_STAGES = {
    "boat": "Boat", "vopr": "VOPR", "law": "Laws", "media": "Media vectors",
    "bootstrap": "Fresh worktree bootstrap", "toolchain": "Toolchain build",
    "cgroup": "Launcher cgroup law", "containment": "Containment generator",
    "ownership": "Ownership soundness",
    "w2": "W2 damage sweep", "n1": "N1 browser_room", "sanitizers": "Sanitizers & Miri",
    "decoder": "Decoder discovery",
}
DEFERRED_STAGES = {"vopr", "w2", "sanitizers", "decoder"}


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


def nightly_alert(directory, now):
    newest = None
    for result in directory.glob("*/complete"):
        try:
            finished = result.stat().st_mtime
        except FileNotFoundError:
            continue
        if newest is None or finished > newest[0]:
            newest = finished, result.parent.name
    unswept = unswept_night(directory, newest)
    if newest is None:
        return f"NIGHTLY-STALE age unavailable (no complete VOPR sweep{unswept})"
    age = now - newest[0]
    if age > MAX_AGE:
        return f"NIGHTLY-STALE {hours(age)} old (>{hours(MAX_AGE)}; {newest[1]}{unswept})"
    return None


UNSWEPT = {"sweep-deferred": "deferred by Boat", "sweep-not-run": "not run"}


def unswept_night(directory, newest):
    """Name a later night that swept no seeds, so the alert says why freshness did not move.

    A complete night is one that swept seeds, whatever its reds. A night Boat deferred, or
    whose sweep never dispatched a batch, is a receipt of that and not a fresh sweep."""
    try:
        lines = (directory / "receipts.ndjson").read_text().splitlines()
    except FileNotFoundError:
        return ""
    for line in reversed(lines):
        receipt = json.loads(line)
        if receipt.get("scope") != "full":
            continue
        if newest is not None and receipt["night"] == newest[1]:
            return ""
        reason = UNSWEPT.get(receipt.get("phase"))
        return f"; latest night {receipt['night']} {reason}, no seeds ran" if reason else ""
    return ""


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
    startup = read_report(base / "startup-failure.json")
    for run in sorted(base.glob("runs/*"), key=attempt_order, reverse=True):
        report = read_report(run / "board" / "board.json")
        attempt = read_report(run / "attempt.json")
        if not attempt and report:
            started = datetime.strptime(run.name.split("-", 1)[0], "%Y%m%dT%H%M%SZ")
            status = report.get("exit_status")
            failures = ", ".join(report.get("failures", {}))
            attempt = {
                "started": started.replace(tzinfo=timezone.utc).timestamp(),
                "commit": report.get("commit"), "status": status,
                "step": failures or ("board comparison" if status in (2, 3) else "board"),
            }
        if attempt:
            if startup and startup["started"] > attempt["started"]:
                break
            attempt["run"] = run.name
            return attempt, report
    return startup or None, {}


def auxiliary_exits(attempt):
    for stage, label in AUXILIARY_STAGES.items():
        if (status := attempt.get(f"{stage}_status")) is not None:
            yield label, status, status == 75 and stage in DEFERRED_STAGES


def attempt_alerts(attempt):
    if attempt is None:
        return
    if attempt.get("service_failure"):
        yield f"BOARD-FAILED {attempt['step']} ({attempt['service_failure']})"
    elif attempt["step"] == "board comparison" and attempt["status"] == 2:
        yield "BOARD-REFUSED board comparison (exit 2)"
    elif attempt["status"]:
        yield f"BOARD-FAILED {attempt['step']} (exit {attempt['status']})"
    for label, status, deferred in auxiliary_exits(attempt):
        if status and not deferred:
            yield f"BOARD-AUX-FAILED {label} (exit {status})"


def print_new_attempt_alerts(base, attempt):
    if not (alerts := list(attempt_alerts(attempt))):
        return
    path = base / "round-alerts.json"
    previous = read_report(path)
    key = attempt.get("run", attempt["started"])
    shown = set(previous.get("alerts", ())) if previous.get("attempt") == key else set()
    for alert in alerts:
        if alert not in shown:
            print(alert)
    if set(alerts) - shown:
        atomic_write(path, json.dumps({"attempt": key, "alerts": sorted(shown.union(alerts))}) + "\n")


def metadata_pins(base):
    pins = set()

    def scan(text):
        pins.update(RUN_REFERENCE.findall(text))

    def inspect(path, *, contents=True):
        if path.is_symlink():
            scan(str(path.parent / path.readlink()))
            scan(str(path.resolve()))
        if contents and path.is_file():
            scan(path.read_text(errors="surrogateescape"))

    def walk_error(error):
        raise error

    for name in ("last-complete.json", "last-complete.v2.json", "last-screen-inputs"):
        inspect(base / name)
    for name in ("baselines", "weekly-main"):
        root = base / name
        inspect(root, contents=False)
        if root.is_dir():
            for directory, dirs, files in os.walk(root, onerror=walk_error):
                if name == "weekly-main" and ".git" in dirs:
                    dirs.remove(".git")
                for entry in (*dirs, *files):
                    inspect(Path(directory) / entry, contents=name == "baselines")

    history = base / "history.sqlite"
    inspect(history, contents=False)
    if history.exists():
        # Run names are SQLite keys. Only paths in a stored contract can point to files.
        with closing(sqlite3.connect(f"{history.as_uri()}?mode=ro", uri=True)) as db:
            for (contract,) in db.execute("SELECT DISTINCT contract FROM samples WHERE contract IS NOT NULL"):
                scan(contract)
    return pins


def prune_candidates(base, current=None):
    base = base.resolve()
    pins = metadata_pins(base)
    if current is not None:
        pins.add(current.name)
    for accepted_only in (False, True):
        if complete := last_complete(base, accepted_only=accepted_only):
            markdown = complete[2]
            pins.update(RUN_REFERENCE.findall(f"{markdown}\n{markdown.resolve()}"))
    runs = [path for path in (base / "runs").glob("*") if re.fullmatch(RUN_NAME, path.name)
            and not path.is_symlink() and path.is_dir()]
    newest_first = sorted(runs, key=lambda path: (attempt_order(path), path.name), reverse=True)
    return [path for path in newest_first[KEEP_RUNS:] if path.name not in pins]


def remove_run(run):
    # Buck seals cached toolchain directories. Leave linked files and directories alone.
    for _, _, _, descriptor in os.fwalk(run):
        mode = stat.S_IMODE(os.fstat(descriptor).st_mode)
        if not mode & stat.S_IWUSR:
            os.fchmod(descriptor, mode | stat.S_IWUSR)
    shutil.rmtree(run)


def verdict(value):
    return "pending" if value is None else "yes" if value else "no"


def render(base, attempt, report, complete, now, nightly=None):
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
        exits = list(auxiliary_exits(attempt))
        if attempt.get("service_failure"):
            outcome = f"Failed · {step} ({attempt['service_failure']})"
        elif status is None:
            outcome = f"Running auxiliary checks · {step}" if board_status is not None else f"Running · {step}"
        elif status == 0:
            outcome = ("Finished with auxiliary failures" if any(code and not deferred for _, code, deferred in exits)
                       else f"Finished · {step} (exit 0)")
        else:
            comparison = {2: "Refused", 3: "Regression"} if step == "board comparison" else {}
            outcome = f"{comparison.get(status, 'Failed')} · {step} (exit {status})"
        commit = attempt["commit"] or "commit unavailable"
        lines.append(f"**Latest attempt** · `{commit}` · {timestamp(attempt['started'])} · {outcome}")
        auxiliary = [f"{label} exit {status}" + (" (deferred)" if deferred else " (failed)" if status else "")
                     for label, status, deferred in exits]
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
    for warning in (alert, nightly):
        if warning:
            lines += ["", f"**{warning}**"]
    if complete is not None:
        lines += ["", "---", "", complete[2].read_text().rstrip()]
    return "\n".join(lines) + "\n"


def atomic_write(path, content):
    with tempfile.TemporaryDirectory(dir=path.parent) as scratch:
        pending = Path(scratch) / path.name
        pending.write_text(content)
        pending.replace(path)


def service_failure(base, tier):
    now = datetime.now(timezone.utc)
    status = os.environ.get("MONITOR_EXIT_STATUS", "")
    exited = os.environ.get("MONITOR_EXIT_CODE") == "exited"
    if exited and status in ("1", "2", "3", "4"):
        outcome = {"1": "incomplete", "2": "refused", "3": "regressed", "4": "auxiliary checks failed"}[status]
        detail = f"{outcome} (exit {status})"
    else:
        result = os.environ.get("MONITOR_SERVICE_RESULT", "unknown")
        detail = f"failed ({result}{', status ' + status if status else ''})"
    invocation = os.environ.get("MONITOR_INVOCATION_ID")
    vopr = os.environ.get("MONITOR_UNIT") == "hirsel-vopr-nightly.service"
    suffix = "-vopr" if vopr else "-weekly" if tier == "weekly" else ""
    unit = "hirsel-vopr-nightly.service" if vopr else f"hirsel-board{suffix}.service"
    journal = (f"journalctl --user _SYSTEMD_INVOCATION_ID={invocation}" if invocation
               else f"journalctl --user -u {unit}")
    if vopr:
        subject = "nightly VOPR sweep"
        if exited and status == "75":
            detail = "swept no seeds: Boat deferred it or no batch ran (exit 75)"
        else:
            detail = f"failed (exit {status})" if status else detail
    else:
        subject = f"{tier} board"
    if not vopr and tier == "nightly":
        raw_detail = f"exit {status}" if exited and status else detail
        attempt = record_service_failure(base, invocation, now.timestamp(), raw_detail)
        if attempt.get("service_failure"):
            detail = f"failed at {attempt['step']} ({raw_detail})"
        else:
            if attempt["status"] and attempt["step"] != "board comparison":
                detail = f"failed at {attempt['step']} ({raw_detail})"
            failures = [f"{label} (exit {code})" for label, code, deferred in auxiliary_exits(attempt)
                        if code and not deferred]
            if failures:
                detail += ". " + " · ".join(failures)
    line = f"{timestamp(now.timestamp())} Hirsel {subject} {detail}. See `{journal}`."
    alerts = base / "alerts"
    alerts.mkdir(exist_ok=True)
    with (alerts / f"{now.date()}{suffix}.txt").open("a") as stream:
        stream.write(line + "\n")
    print(line)
    return not vopr and tier == "nightly"


def record_service_failure(base, invocation, now, detail):
    if invocation:
        for path in base.glob("runs/*/attempt.json"):
            attempt = read_report(path)
            if attempt.get("invocation") == invocation:
                if attempt["status"] is None:
                    attempt.update(status=1, service_failure=detail)
                    atomic_write(path, json.dumps(attempt) + "\n")
                return attempt
    # Startup can fail before the script creates even its first attempt record.
    attempt = {"started": now, "commit": None, "step": "service startup", "status": 1,
               "service_failure": detail}
    atomic_write(base / "startup-failure.json", json.dumps(attempt) + "\n")
    return attempt


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", type=Path, required=True)
    parser.add_argument("--current", type=Path)
    parser.add_argument("--alert", action="store_true", help="print new attempt failures and current staleness")
    parser.add_argument("--nightly-dir", type=Path, default=NIGHTLY_DIR)
    parser.add_argument("--service-failure-tier", choices=("nightly", "weekly"),
                        help="record the failing systemd invocation for this board tier")
    parser.add_argument("--last-complete-commit", action="store_true")
    parser.add_argument("--prune", type=Path, metavar="CURRENT_RUN", help=argparse.SUPPRESS)
    parser.add_argument("--record", type=Path, help="attempt run directory")
    parser.add_argument("--started", type=int)
    parser.add_argument("--commit")
    parser.add_argument("--step")
    parser.add_argument("--status", type=int, help="exit status, omitted while running")
    for stage in AUXILIARY_STAGES:
        parser.add_argument(f"--{stage}-status", type=int)
    args = parser.parse_args()
    args.base.mkdir(parents=True, exist_ok=True)
    with (args.base / "freshness.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        if args.prune is not None:
            # The nightly wrapper already holds nightly.lock and baseline.lock.
            for run in prune_candidates(args.base, args.prune):
                remove_run(run)
                print(f"Pruned board run {run}")
            return
        if args.service_failure_tier and not service_failure(args.base, args.service_failure_tier):
            return
        if args.record:
            if args.started is None or not args.step:
                parser.error("--record requires --started and --step")
            if args.step == "board" and args.status:
                report = read_report(args.record / "board" / "board.json")
                args.step = ", ".join(report.get("failures", {})) or args.step
            attempt = {"started": args.started, "commit": args.commit,
                       "step": args.step, "status": args.status,
                       "invocation": os.environ.get("INVOCATION_ID")}
            attempt.update((f"{stage}_status", getattr(args, f"{stage}_status")) for stage in AUXILIARY_STAGES)
            atomic_write(args.record / "attempt.json", json.dumps(attempt) + "\n")
        if args.last_complete_commit and (accepted := last_complete(args.base, accepted_only=True)):
            print(accepted[1])
        if args.current or args.alert:
            complete = last_complete(args.base)
            now = time.time()
            nightly = nightly_alert(args.nightly_dir, now)
            attempt, report = latest_attempt(args.base)
            if args.current:
                atomic_write(args.current, render(args.base, attempt, report, complete, now, nightly))
            if args.alert:
                print_new_attempt_alerts(args.base, attempt)
                for alert in (board_alert(complete, now), nightly):
                    if alert:
                        print(alert)


if __name__ == "__main__":
    main()
