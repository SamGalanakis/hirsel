"""Append the outcome of one nightly run, including runs that never dispatched."""

from collections import Counter, defaultdict
from datetime import datetime, timezone
import fcntl
import json
import os
from pathlib import Path
import subprocess
import sys

from sim_campaign import CANARY
from vopr_failure_groups import deferred, expected_canary, publish, result_rows
from vopr_nightly_negative import CONTROL_NAMES


NOT_COVERED = (
    "real-disk power loss, device write reordering, and sector damage",
    "the W2 crash-cut and Lean suites",
    "laws, fault classes, and schedules outside this run's recorded evidence",
)
PROBE_DIVISOR = 20_000
STORE_CRASH_PROBES = {
    "crash before the rename": "power-cut-before-rename",
    "crash after the rename": "power-cut-after-rename",
}


def frequency_missed(hits, runs):
    return hits * PROBE_DIVISOR <= runs


def code_revision(binary):
    """Read the 32-byte entry revision from the prepared Linux ELF executable."""
    table = subprocess.check_output(["readelf", "-Wls", binary], text=True)
    addresses = set()
    segments = []
    for line in table.splitlines():
        fields = line.split()
        if len(fields) == 8 and fields[-1] == "hirsel_code_revision" and fields[2] == "32":
            addresses.add(int(fields[1], 16))
        if fields and fields[0] == "LOAD":
            offset, address, _, size = (int(field, 16) for field in fields[1:5])
            segments.append((offset, address, size))
    if len(addresses) == 1:
        address, = addresses
        for offset, start, size in segments:
            if start <= address and address + 32 <= start + size:
                with open(binary, "rb") as file:
                    file.seek(offset + address - start)
                    revision = file.read(32)
                if len(revision) == 32:
                    return revision.hex()
    raise ValueError(f"{binary}: missing file-backed hirsel_code_revision")


def read_evidence(receipt, errors):
    try:
        if receipt.exists():
            yield from result_rows(receipt)
    except (OSError, EOFError, ValueError) as error:
        errors.append(f"{receipt.name}: {error}")


def summarize(run, manifest, commit):
    errors = []
    prepared = {}
    revisions = {}
    if manifest.is_file():
        try:
            for law in json.loads(manifest.read_text()):
                if law["build"] != commit or law["law"] in prepared:
                    raise ValueError(f"unexpected prepared law: {law['law']}")
                prepared[law["law"]] = law
        except (OSError, KeyError, TypeError, ValueError, subprocess.CalledProcessError) as error:
            errors.append(f"{manifest.name}: {error}")
    for name, law in prepared.items():
        revisions[name] = {}
        for leg, key in (("normal", "executable"), ("recording", "recording")):
            try:
                revisions[name][leg] = code_revision(law[key])
            except (OSError, KeyError, ValueError, subprocess.CalledProcessError) as error:
                errors.append(f"{name} {leg}: {error}")

    inventory = run / "inventory"
    unprepared = {law: "no prepared executable" for law in inventory.read_text().splitlines()
                  if law not in prepared} if inventory.exists() else {}
    for row in read_evidence(run / "prepare.jsonl", errors):
        if row["build"] == commit:
            unprepared[row["law"]] = row["reason"]

    counts = Counter()
    misses = set()
    faults = set()
    seen = set()
    failed_at_zero = set()
    canary_red = None
    runs = Counter()
    observed_runs = Counter()
    probes = defaultdict(Counter)
    declarations = run / "probe-declarations.json"
    declared = {}
    if declarations.exists():
        try:
            declared = json.loads(declarations.read_text())
        except (OSError, ValueError) as error:
            errors.append(f"{declarations.name}: {error}")
    for law, labels in declared.items():
        for label in labels:
            probes[law, label]
    for law, entry in prepared.items():
        for label in entry.get("required_witnesses", []):
            probes[law, label]
    batches_deferred = 0
    batches = sorted(run.glob("*.jsonl.gz"))
    for batch in batches:
        canaries = []
        rows = list(read_evidence(batch, errors))
        batch_deferred = deferred(rows)
        batches_deferred += batch_deferred
        for row in rows:
            if row.get("kind") == "batch_deferred":
                continue
            law, seed = row["law"], row.get("seed")
            if row.get("build") != commit or law not in prepared:
                errors.append(f"{batch.name}: unexpected result for {law}")
                continue
            if row.get("kind") == "reach_red":
                misses.add((law, row["label"]))
            if row.get("kind") == "trace_diversity_red":
                counts["trace_diversity_reds"] += 1
            if seed == 0 and row["status"]:
                failed_at_zero.add(law)
            if seed is None or seed <= 0:
                continue
            if seed in seen:
                errors.append(f"{batch.name}: duplicate fresh seed {seed}")
                continue
            seen.add(seed)
            counts["seeds_executed"] += 1
            counts["replays_diverged"] += bool(row.get("tape_divergence") or row.get("same_build_nondeterminism"))
            expected_red = law == CANARY and expected_canary(row)
            if law == CANARY:
                canaries.append((seed % 2, expected_red))
            counts["seeds_failed"] += bool(row["status"] and not expected_red)
            runs[law] += 1
            if (witnesses := row.get("witnesses")) is not None:
                observed_runs[law] += 1
                for label, calls in witnesses.items():
                    probe = probes[law, label]
                    probe["hits"] += calls > 0
                    probe["true_calls"] += calls
                    if calls:
                        if label.startswith("fault:"):
                            faults.add(label.removeprefix("fault:"))
                        elif law == "store@1/store/test_l69_random_arm" and label in STORE_CRASH_PROBES:
                            faults.add(STORE_CRASH_PROBES[label])
        if batch_deferred:
            continue
        good = sorted(canaries) == [(0, True), (1, True)]
        canary_red = good if canary_red is None else canary_red and good
    frequencies = []
    for (law, label), count in sorted(probes.items()):
        row = {"law": law, "label": label, "runs": runs[law], "observed_runs": observed_runs[law],
               "hits": count["hits"], "true_calls": count["true_calls"]}
        if label in declared.get(law, {}):
            rare = declared[law][label]
            # With no batch swept, a law that never ran has no reach evidence; the
            # deferral or the unswept night is the red.
            missed = (frequency_missed(row["hits"], row["runs"])
                      and (row["runs"] or batches_deferred < len(batches)))
            row.update(rare=rare, divisor=PROBE_DIVISOR, severity=(30 if rare else 40) if missed else None)
            if missed:
                misses.add((law, label))
        frequencies.append(row)
    cross_build = None
    for row in read_evidence(run / "cross-build.jsonl", errors):
        if row.get("kind") == "summary":
            cross_build = {
                field: row[field]
                for field in ("status", "saved_commit", "saved_format", "current_format", "checked", "failed")
                if field in row
            }
    ace = None
    ace_path = run / "ace.json"
    if ace_path.is_file():
        try:
            raw_ace = json.loads(ace_path.read_text())
            ace = {
                field: raw_ace[field]
                for field in ("alphabet_size", "max_len", "total_workloads",
                              "clean_cuts_run", "sector_cuts_run", "total_cuts_run",
                              "failed", "wall_seconds")
                if field in raw_ace
            }
        except (OSError, ValueError) as error:
            errors.append(f"{ace_path.name}: {error}")
    return {
        "laws_prepared": sorted(prepared),
        "laws_unprepared": unprepared,
        "code_revision": revisions,
        "batches_deferred": batches_deferred,
        "seeds_executed": counts["seeds_executed"],
        "seeds_failed": counts["seeds_failed"],
        "deterministic_reds": sorted(failed_at_zero),
        "trace_diversity_reds": counts["trace_diversity_reds"],
        "replays_diverged": counts["replays_diverged"],
        "probes": frequencies,
        "probes_missed": sorted(misses),
        "fault_classes_sampled": sorted(faults),
        "canary_red": canary_red,
        "negative_controls": [
            {field: row[field] for field in ("oracle", "rejected", "exit_status")}
            for row in read_evidence(run / "negative.jsonl", errors)
        ],
        "cross_build_reopen": cross_build,
        "ace_enumeration": ace,
        "second_witness": next(read_evidence(run / "second-witness.jsonl", errors), None),
        "evidence_errors": errors,
    }


def append_once(path, receipt):
    with path.open("a+") as log:
        fcntl.flock(log, fcntl.LOCK_EX)
        log.seek(0)
        for line in log:
            existing = json.loads(line)
            if existing["night"] == receipt["night"]:
                return existing["status"]
        print(json.dumps(receipt, sort_keys=True), file=log)
        log.flush()
        os.fsync(log.fileno())
    return receipt["status"]


def finish(run, manifest, commit, phase, status):
    evidence = summarize(run, manifest, commit)
    focused = (run / "focused").exists()
    negative_passed = {row["oracle"] for row in evidence["negative_controls"] if row["rejected"] is True}
    negatives_missing = [] if focused else sorted(set(CONTROL_NAMES) - negative_passed)
    evidence["negative_controls_unproven"] = negatives_missing
    if phase in ("finished", "sweep-failed", "sweep-deferred", "sweep-not-run"):
        probe_reds = ({**probe, "kind": "reach_red", "build": commit} for probe in evidence["probes"]
                      if probe.get("severity") is not None)
        try:
            group_status = publish(run, require_canary=True, probe_reds=probe_reds,
                                   not_run=phase == "sweep-not-run")
            if phase == "finished" and group_status in (0, 3):
                marker = "focused-complete" if focused else "complete"
                (run / marker).touch()
            status = group_status or status
        except (OSError, ValueError, KeyError, TypeError) as error:
            evidence["evidence_errors"].append(f"failure groups: {error}")
    if evidence["evidence_errors"] or evidence["canary_red"] is not True:
        status = status or 2
    if any(evidence[field] for field in (
        "seeds_failed", "deterministic_reds", "trace_diversity_reds", "replays_diverged", "probes_missed"
    )):
        status = status or 1
    if negatives_missing:
        status = status or 1
    if evidence.get("cross_build_reopen") and evidence["cross_build_reopen"].get("failed", 0) > 0:
        status = status or 1
    if evidence.get("ace_enumeration") and evidence["ace_enumeration"].get("failed", 0) > 0:
        status = status or 1
    receipt = {
        "night": run.name,
        "commit": commit,
        "finished_utc": datetime.now(timezone.utc).isoformat(),
        "scope": "focused" if focused else "full",
        "phase": phase,
        "status": status,
        "not_covered": NOT_COVERED,
        **evidence,
    }
    return append_once(run.parent / "receipts.ndjson", receipt)


if __name__ == "__main__":
    run, manifest, commit, phase, status = sys.argv[1:]
    sys.exit(finish(Path(run), Path(manifest), commit, phase, int(status)))
