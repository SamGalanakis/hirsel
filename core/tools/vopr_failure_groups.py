"""Publish one entry per failure group from a nightly VOPR sweep."""

from collections import Counter
import gzip
import json
from pathlib import Path
import re
import sys

from sim_campaign import CANARY


# The runtime's cause line (crates/hirsel-core-rt/src/abi_host.rs format_cause_at):
# `Kind("message") at site` or `Kind at site`. The message is Rust `{:?}` output,
# so it may itself contain quotes, `)` or ` at `; the site is the last token.
CAUSE = re.compile(r'^(Overflow|Bounds|Division|Unwrap|Fail|Cancelled|Capacity|OutOfTime)'
                   r'(?:\(".*"\))? at (\S+)$')
# Keep path:line:col sites; blank quoted values and every other digit run.
TAIL = re.compile(r'(\S+:\d+:\d+\b)|"(?:[^"\\]|\\.)*"|\d+')


def blank(text):
    return TAIL.sub(lambda part: part[1] or ('"_"' if part[0][0] == '"' else 'N'), text)


def site_detail(row):
    """Return the failure site: the runtime cause when one is reported, else the first line, blanked."""
    lines = (row["stderr"].strip() or row["stdout"].strip()).splitlines()
    for line in (row["stderr"] + "\n" + row["stdout"]).splitlines():
        cause = CAUSE.match(line.strip())
        if cause:
            return f"{cause[1]} at {cause[2]}"
    return blank(lines[0]) if lines else ""


def group(row):
    kind = row.get("kind", "seed_red")
    detail = row.get("label") or row.get("failure_class")
    if detail is None and kind in ("seed_red", "deterministic_red"):
        detail = f"exit {row['status']} {site_detail(row)}".rstrip()
    return row["law"], kind, detail


def result_rows(receipt):
    stream = gzip.open(receipt, "rt") if receipt.suffix == ".gz" else receipt.open()
    with stream:
        for line in stream:
            yield json.loads(line)


def expected_canary(row):
    seed = row["seed"]
    return (seed > 0 and row["status"] == seed % 2
            and row["stdout"] == ""
            and row["stderr"] == ("error: ()\n" if seed % 2 else "")
            and (row.get("tape_replay") or {}).get("status") == 0
            and not any(key in row for key in ("recorded_run", "tape_divergence", "timed_out")))


def publish(run, *, require_canary=False, probe_reds=()):
    """Write all failure groups. Exit 2 for an unverified sweep, 3 for preparation reds."""
    previous = set()
    for report in run.parent.glob("*/failure-groups.jsonl"):
        if report.parent != run:
            with report.open() as stream:
                previous.update(tuple(json.loads(line)["group"]) for line in stream)

    samples = {}
    counts = Counter()
    prepare = run / "prepare.jsonl"
    batches = sorted(run.glob("*.jsonl.gz"))
    receipts = ([prepare] if prepare.is_file() else []) + batches
    canary_ok = not require_canary or bool(batches)
    for receipt in receipts:
        canary = []
        for row in result_rows(receipt):
            if require_canary and row["law"] == CANARY and row.get("seed", 0) > 0:
                canary.append(row)
                if expected_canary(row):
                    continue
            if row.get("status", 0) or (row.get("kind") and row["kind"] != "prepare_skip"):
                key = group(row)
                counts[key] += 1
                samples.setdefault(key, {"receipt": receipt.name, "build": row.get("build"),
                                         "digest": row.get("digest"), "seed": row.get("seed")})
                if "reason" in row:
                    samples[key].setdefault("reason", row["reason"])
        if require_canary and receipt.suffix == ".gz":
            passed = (len(canary) == 2 and {row["seed"] % 2 for row in canary} == {0, 1}
                      and all(expected_canary(row) for row in canary))
            if not passed:
                print(f"{receipt}: missing canary pass or expected red", file=sys.stderr)
                canary_ok = False
    if require_canary and not canary_ok:
        print(f"{run}: no verified canary result", file=sys.stderr)
        key = CANARY, "canary_missing", None
        counts[key] = 1
        samples[key] = {"receipt": None, "build": None, "digest": None, "seed": None}
    for row in probe_reds:
        key = group(row)
        counts[key] += 1
        samples[key] = {"receipt": "receipts.ndjson", "build": row["build"],
                        "seed": None, "hits": row["hits"], "runs": row["runs"],
                        "severity": row["severity"]}
    negatives = run / "negative.jsonl"
    if negatives.is_file():
        for row in result_rows(negatives):
            if not row["rejected"]:
                key = row["oracle"], "negative_control_red", None
                counts[key] = 1
                samples[key] = {"receipt": negatives.name, "seed": None, "exit_status": row["exit_status"]}
    output = run / "failure-groups.tmp"
    with output.open("w") as report:
        for key in counts:
            print(json.dumps({"group": key, "new": key not in previous, "count": counts[key],
                              **samples[key]}), file=report)
    output.replace(run / "failure-groups.jsonl")
    if not canary_ok:
        return 2
    return 3 if any(key[1] == "prepare_red" for key in counts) else 0


if __name__ == "__main__":
    sys.exit(publish(Path(sys.argv[1]).resolve(), require_canary=True))
