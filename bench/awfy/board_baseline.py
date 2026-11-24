"""Classify baseline refusals and publish validated boards."""
from pathlib import Path
import re
import tempfile

import receipt
from receipt import measurement_slot


def refused_identity_fields(report, keys, size_identity):
    instruction = report["instruction_comparison"]
    size, checkbox = report["size_comparison"], report["checkbox_comparison"]
    if (instruction["status"] not in ("Within +2% on every row", "Refused")
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


def promote(path, base, expected_baseline):
    base = base if base is not None else Path.home() / ".local/share/hirsel-board"
    base.mkdir(parents=True, exist_ok=True)
    baseline = base / "last-complete.json"
    with measurement_slot(base / "baseline.lock"):
        if expected_baseline is not None and baseline.resolve() != expected_baseline:
            raise ValueError("baseline changed since the nightly comparison")
        with tempfile.TemporaryDirectory(dir=base) as scratch:
            link = Path(scratch) / "baseline"
            link.symlink_to(path)
            link.replace(baseline)
