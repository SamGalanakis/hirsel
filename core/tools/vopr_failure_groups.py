"""Publish one entry per failure group from a nightly VOPR sweep."""

from collections import Counter
import gzip
import json
from pathlib import Path
import re
import sys


def group(row):
    kind = row.get("kind", "seed_red")
    detail = row.get("label") or row.get("failure_class")
    if detail is None and kind in ("seed_red", "deterministic_red"):
        diagnostic = (row["stderr"].strip() or row["stdout"].strip()).splitlines()
        first_line = re.sub(r":\d+:\d+\b", "", diagnostic[0]) if diagnostic else ""
        detail = f"exit {row['status']} {first_line}".rstrip()
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
