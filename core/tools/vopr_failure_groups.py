"""Publish one entry per failure group from a nightly VOPR sweep."""

from collections import Counter
import gzip
import json
from pathlib import Path
import re
import sys


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


def publish(run):
    previous = set()
    for report in run.parent.glob("*/failure-groups.jsonl"):
        if report.parent != run:
            with report.open() as stream:
                previous.update(tuple(json.loads(line)["group"]) for line in stream)

    samples = {}
    counts = Counter()
    for batch in sorted(run.glob("*.jsonl.gz")):
        with gzip.open(batch, "rt") as stream:
            for line in stream:
                row = json.loads(line)
                if row.get("status", 0) or row.get("kind"):
                    key = group(row)
                    counts[key] += 1
                    samples.setdefault(key, {"receipt": batch.name, "build": row.get("build"),
                                             "seed": row.get("seed")})
    output = run / "failure-groups.tmp"
    with output.open("w") as report:
        for key in counts:
            print(json.dumps({"group": key, "new": key not in previous, "count": counts[key],
                              **samples[key]}), file=report)
    output.replace(run / "failure-groups.jsonl")


if __name__ == "__main__":
    publish(Path(sys.argv[1]).resolve())
