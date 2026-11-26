"""Validate nightly size rows and diagnose archived ELF growth."""
from pathlib import Path
import re

import awfy_measure
from board_baseline import rows_by_label
import elf_census

SIZE_ROW = "AWFY original-14 harness"
SIZE_IDENTITY = ("strip_version", "strip_flags", "verified_workloads", "pins")


def size_changes(reference, candidate):
    previous_measurement = reference.get("size_measurement", {})
    current_measurement = candidate.get("size_measurement", {})
    for field in SIZE_IDENTITY:
        previous = previous_measurement.get(field)
        if not previous or previous != current_measurement.get(field):
            raise ValueError(f"size identity differs: {field}")
    left, right = (rows_by_label(report)["Size", SIZE_ROW] for report in (reference, candidate))
    changes = []
    for side in awfy_measure.SIDES:
        previous, current = (row["samples"][side] for row in (left, right))
        for sample in (previous, current):
            if (sample["status"] != "ok" or type(sample["bytes"]) is not int or sample["bytes"] <= 0
                    or not all(re.fullmatch("[a-f0-9]{64}", sample[field])
                               for field in ("image_sha256", "binary_sha256"))):
                raise ValueError(f"{side} lacks a verified size sample")
        before, after = previous["bytes"], current["bytes"]
        if previous["image_sha256"] == current["image_sha256"] and before != after:
            raise ValueError(f"{side} has inconsistent byte counts for the same stripped image")
        changes.append((side, before, after))
    return changes


def size_census(baseline, candidate, reference, report, side):
    baseline_dir = baseline.parent
    if baseline.name != "board.json":
        # A promoted receipt is copied without its workload logs or binaries.
        bounce = rows_by_label(reference)["AWFY original", "Bounce"]["samples"][side]
        baseline_dir = Path(bounce["directory"]).parents[2]
    archives = [path / "awfy" / f"{side}-harness" for path in (baseline_dir, candidate.parent)]
    result = elf_census.census(*archives)
    for name, board in (("baseline", reference), ("candidate", report)):
        sample = rows_by_label(board)["Size", SIZE_ROW]["samples"][side]
        image = result[name]
        if (image["unstripped"]["sha256"] != sample["binary_sha256"]
                or image["stripped"]["sha256"] != sample["image_sha256"]
                or image["stripped"]["file_bytes"] != sample["bytes"]):
            raise ValueError(f"{name} archived ELF does not match its size sample")
    return elf_census.largest_deltas(result)


def census_markdown(census):
    lines = ["", "### Hirsel size failure ELF census", ""]
    if "error" in census:
        return lines + [f"Unavailable ({census['error']})."]
    lines += ["Largest stripped section deltas:", ""]
    lines += [f"- `{row['section']}` {row['bytes']:+,} B" for row in census["sections"]]
    lines += ["", "Largest file-backed symbol deltas (unstripped):", ""]
    lines += [f"- `{row['symbol']}` in `{row['section']}` {row['bytes']:+,} B"
              for row in census["symbols"]]
    return lines
