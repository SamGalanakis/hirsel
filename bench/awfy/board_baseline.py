"""Classify baseline refusals and publish validated boards."""
from pathlib import Path
import re
from shutil import copy2
import tempfile

import receipt
from receipt import measurement_slot

COMPARISON_PASSED = "Instructions within +2%, peak RSS within +5%"


def rows_by_label(report):
    rows = {}
    for row in report["rows"]:
        label = (row["goal"], row["row"])
        if label in rows:
            raise ValueError(f"duplicate board row {'/'.join(label)}")
        rows[label] = row
    return rows


def refused_identity_fields(report, keys, size_identity):
    instruction = report["instruction_comparison"]
    size, checkbox = report["size_comparison"], report["checkbox_comparison"]
    if (instruction["status"] not in (COMPARISON_PASSED, "Refused")
            or any(line.endswith(" FAIL") for line in (*instruction["details"], *size, *checkbox))
            or any("REFUSED" in line for line in checkbox)):
        return set()

    changed = set()
    previous_version = instruction.get("previous_receipt_version")
    details = instruction["details"]
    if (type(previous_version) is int and 0 < previous_version < receipt.VERSION
            and details == [f"REFUSED ({receipt.OlderBoardReceipt(previous_version)})"]):
        changed.add("instruction receipt version")
        details = []
    for line in details:
        if "REFUSED" not in line:
            continue
        match = re.fullmatch(r"(.+) REFUSED \(identity differs: \1: (.+)\)", line)
        if not match or match[1] not in keys:
            return set()
        fields = set(match[2].split(", "))
        if not fields.issubset((*receipt.WORKLOAD_IDENTITY, "marker", "stdout")):
            return set()
        changed.update(fields)
    if bool(changed) != (instruction["status"] == "Refused"):
        return set()

    for line in size:
        if "REFUSED" not in line:
            continue
        match = re.fullmatch(r"REFUSED \(size identity differs: (\w+)\)", line)
        if not match or match[1] not in size_identity:
            return set()
        changed.add(match[1])
    return changed


def promote(path, base, expected_baseline=None, expected_inode=None):
    commit = receipt.read(path)["commit"]
    if not re.fullmatch(r"[0-9a-f]{40}", commit):
        raise ValueError("invalid baseline commit")
    base = base if base is not None else Path.home() / ".local/share/hirsel-board"
    baselines = base / "baselines"
    baselines.mkdir(parents=True, exist_ok=True)
    baseline = base / "last-complete.json"
    snapshot = baselines / f"{commit}.json"
    with measurement_slot(base / "baseline.lock"):
        if expected_baseline is not None:
            target = baseline.resolve() if baseline.exists() or baseline.is_symlink() else ""
            if target != expected_baseline:
                raise ValueError("baseline changed since the nightly comparison")
            if expected_inode and target:
                stat = target.stat()
                if f"{stat.st_dev}:{stat.st_ino}" != expected_inode:
                    raise ValueError("baseline changed since the nightly comparison")
        with tempfile.TemporaryDirectory(dir=base) as scratch:
            scratch = Path(scratch)
            copy2(path, scratch / "board.json")
            markdown = path.with_suffix(".md")
            if markdown.is_file():
                copy2(markdown, scratch / "board.md")
                (scratch / "board.md").replace(snapshot.with_suffix(".md"))
            else:
                snapshot.with_suffix(".md").unlink(missing_ok=True)
            link = scratch / "baseline"
            link.symlink_to(snapshot.relative_to(base))
            (scratch / "board.json").replace(snapshot)
            link.replace(baseline)
