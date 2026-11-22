"""Read a complete, same-revision export from the owner of the units benchmarks."""
from datetime import datetime, time
import hashlib
import json
import math
from pathlib import Path
import re
import sys

import build
import comparison
import receipt

sys.path.insert(0, str(build.ROOT))
from bench.multicore import regimes

ARMS = ("hirsel", "tokio")
OWNER_SIDES = dict(zip(ARMS, ("untied", "tokiow8"), strict=True))
CASES = tuple(case for case in regimes.WORKLOADS if case != "calibrate")
HEADLINE = {"independent16", "pairs", "fanin", "fanout", "pipeline", "grain_spawn_chain",
            "spawn_tree", "channel_hub", "arc_hub", "timers", "sockets64", "sockets1024", "checkbox"}
ROUNDS = dict(zip(regimes.GRAINS, (0, 64, 640, 6400, 64000), strict=True))
POPS = (256, 2048, 8192)
WIDTHS = (1, 4, 8)
CAP = 2 << 30
SYNC_POLICY = {"wait_us": 400, "max_writes": 4096, "durable_syncs_per_group": 1}
CEILING_METRICS = ("offer_turns_per_s", "goodput_turns_per_s", "intended_arrival_p99_ns")
MATCHED_METRICS = ("intended_arrival_p99_ns", "cpu_us_per_turn")
REGIME_METRICS = ("work_per_s", "speedup_vs_w1")
QUANTILES = {
    "intended_arrival_p99_9_ns": "e2e_p999_ns", "intended_arrival_max_ns": "e2e_max_ns",
    "server_added_p99_9_ns": "p999_ns", "server_added_max_ns": "max_ns",
}
ALLOWED_CPUS = set(range(4)) | set(range(8, 12)) | set(range(16, 20)) | set(range(24, 28))


def expected(*, tmpfs_matched):
    a1 = [(storage, size, "hardware") for storage in ("ext4", "tmpfs") for size in POPS]
    a1.append(("ext4", 8192, "software"))
    rows = {}
    for storage, size, sha in a1:
        label = f"{storage}/{size}/{sha}"
        rows["A1", label] = (storage, size, sha, False)
        if storage == "ext4" or tmpfs_matched:
            rows["A1", label + "/matched"] = (storage, size, sha, True)
    rows.update({("Regimes", f"{case}/{grain}ns/w{width}"): (case, grain, width)
                 for case in CASES for grain in ((0,) if case == "checkbox" else regimes.GRAINS)
                 for width in WIDTHS})
    return rows


def board_label(key):
    goal, name = key
    if goal == "Regimes":
        return "Units regime", name.replace("checkbox/0ns/", "checkbox/native/")
    if name.endswith("/matched"):
        return "Units A1", name.removesuffix("/matched") + " / matched offer"
    return "Units A1", name + " / ceiling"


def missing_row(key, reason, *, status="pending"):
    goal, name = board_label(key)
    metrics = REGIME_METRICS if key[0] == "Regimes" else (
        MATCHED_METRICS if key[1].endswith("/matched") else CEILING_METRICS)
    return {"goal": goal, "row": name, "metrics": metrics, "samples": {},
            "ratios": {"tokio": {}}, "status": status, "issues": {"export": reason}}


def require(predicate, message):
    if not predicate:
        raise ValueError(message)


def unique_rows(rows):
    labels = {}
    for row in rows:
        key = (row["goal"], row["row"])
        require(key not in labels, f"duplicate units row {key}")
        labels[key] = row
    return labels


def evidence_path(root, name):
    path = (root / name).resolve()
    require(not Path(name).is_absolute() and path.is_relative_to(root) and path.is_file(),
            f"missing or external units evidence {name}")
    return path


def close(left, right):
    return comparison.positive(left) and comparison.positive(right) and math.isclose(
        left, right, rel_tol=1e-9)


def clean_hold(hold, started, updated):
    begin, end = (datetime.fromisoformat(hold[field]) for field in ("started", "finished"))
    return (begin.tzinfo is not None and end.tzinfo is not None and started <= begin <= end <= updated
            and begin.date() == end.date()
            and (end.time() < time(2, 45) or begin.time() >= time(4))
            and hold["board_window_open"] is True and hold["cc1plus_start"] == hold["cc1plus_end"] == []
            and 0 < hold["hold_s"] < 240 and hold["load_start"] <= 8 and hold["load_end"] <= 8
            and not ({"load_exceeded", "port_conflict"} & hold.keys()))


def validate_config(export, commit):
    require(export["commit"] == commit, "units revision mismatch")
    config = export["config"]
    require(export["version"] == 1 and config["mode"] == "full" and config["repeats"] == 1
            and sorted(config["families"]) == ["a1", "regimes"]
            and sorted(config["regimes"]) == sorted(CASES),
            "units requires one full, untaped A1 and 17-regime export")
    require(export["complete"] is True and not export["failures"], "units export is incomplete")
    tmpfs = config.get("tmpfs_matched", False)
    require(type(tmpfs) is bool, "invalid tmpfs matched-offer mode")
    keys = expected(tmpfs_matched=tmpfs)
    declared, seen = (unique_rows(export[name]) for name in ("expected_rows", "rows"))
    require(declared.keys() == seen.keys() == keys.keys(), "incomplete units label set")
    require(all(row["status"] == "ok" for row in declared.values()), "units expected row failed")
    require(type(config["reverse"]) is bool and config["build_cpus"] == [8, 9, 10, 11]
            and config["build_jobs"] == 4 and config["build_memory_limit_bytes"] == 8 << 30
            and config["measurement_memory_limit_bytes"] == CAP and config["sync_policy"] == SYNC_POLICY,
            "units export lacks the required build, measurement cap or sync policy")
    topology = [config[f"{role}_cpus"] for role in ("server", "load", "helper", "controller")]
    cpus = [cpu for group in topology for cpu in group]
    require(len(topology[0]) == 8 and all(topology) and len(cpus) == len(set(cpus))
            and all(type(cpu) is int for cpu in cpus) and set(cpus) <= ALLOWED_CPUS,
            "invalid units CPU topology")
    require(set(export["binary_sha256"]) >= {
        "a1/bin/session_u", "a1/bin/twin", "a1/software/twin", "a1/bin/loadgen", "a1/bin/toolsrv",
        "regimes/hirsel/regimes", "regimes/rust/regimes", "regimes/checkbox/checkbox",
        "regimes/checkbox/wants.bin", "regimes/wire/release/wire-reference",
        "regimes/wire/release/wire-load",
    } and all(re.fullmatch(r"[0-9a-f]{64}", digest) for digest in export["binary_sha256"].values()),
            "missing units binary identities")
    started, updated = (datetime.fromisoformat(export[field]) for field in ("started", "updated"))
    require(started.tzinfo is not None and updated.tzinfo is not None and started <= updated,
            "invalid units timestamps")
    return keys, seen, config, started, updated


def a1_samples(row, key, root, started, updated):
    storage, clients, sha, matched = key
    require((row["filesystem"], row["connections"], row["kind"]) ==
            (storage, clients, "matched" if matched else "ceiling"), "A1 row identity mismatch")
    path = evidence_path(root, row["raw"])
    raw = [json.loads(line) for line in path.read_text().splitlines()]
    out, pair = {}, []
    for arm, owner_side in OWNER_SIDES.items():
        sample = row["samples"][arm]
        require(sample["status"] == "ok" and sample["sha_backend"] == sha, f"{arm} A1 SHA or status mismatch")
        candidates = [trial for trial in raw if trial.get("trial") == sample["trial"]
                      and trial.get("side") == owner_side]
        require(len(candidates) == 1, f"{arm} A1 raw trial missing or duplicated")
        trial = candidates[0]
        require(trial["discarded"] is False and trial["passed"] is True and trial["hold"] == sample["hold"]
                and clean_hold(trial, started, updated), f"{arm} A1 hold is unqualified")
        pair.append(trial)
        cell, load = trial["cell"], trial["load"]
        require(cell["journal"] == storage and cell["sessions"] == clients and cell["fair"] is True
                and cell["rec"] is False and cell["software_sha"] is (sha == "software"),
                f"{arm} A1 workload mismatch")
        turn_line = next((line for line in trial["server_err"] if line.startswith(("session turns=", "twin turns="))), "")
        server_turns = re.match(r"(?:session|twin) turns=(\d+)(?: |$)", turn_line)
        require(trial["load_rc"] == trial["server_rc"] == 0 and trial["load_timeout"] is False
                and load["failures"] == load["timeouts"] == 0
                and load["completed"] == load["issued"] == sample["completed"] > 0
                and server_turns and int(server_turns[1]) == load["issued"]
                and 0 < load["e2e_p99_ns"] <= 5_000_000,
                f"{arm} A1 trial did not complete all verified turns within the latency bound")
        require(sample["intended_arrival_p99_ns"] == load["e2e_p99_ns"]
                and sample["offer_turns_per_s"] == trial["rate"]
                and close(sample["goodput_turns_per_s"], load["completed_after_warm_in_run"] * 1e9 /
                          (load["end_ns"] - load["warm_ns"])), f"{arm} A1 summary differs from raw trial")
        if matched:
            require(close(sample["cpu_us_per_turn"], trial["server_measured_cpu_us_per_turn"])
                    and close(sample["sync_groups_per_turn"], trial["sync_groups"] / load["issued"]),
                    f"{arm} A1 CPU or sync groups mismatch")
        else:
            summaries = [item for item in raw if item.get("summary") and item["side"] == owner_side]
            require(len(summaries) == 1 and summaries[0]["incomplete"] is False
                    and round(summaries[0]["max_rate"]) == trial["rate"]
                    and trial["rate"] == max(item["rate"] for item in raw if item.get("trial")
                                            and item["side"] == owner_side and item["passed"]
                                            and not item["discarded"]), f"{arm} A1 search ceiling is incomplete")
        out[arm] = sample | {field: load[source] for field, source in QUANTILES.items() if source in load}
    require(pair[0]["seed"] == pair[1]["seed"], "A1 arms used different seeds")
    if matched:
        sync = comparison.divide(out["hirsel"]["sync_groups_per_turn"], out["tokio"]["sync_groups_per_turn"])
        require(pair[0]["rate"] == pair[1]["rate"] and pair[0]["hold"] == pair[1]["hold"]
                and row["sync_groups_match"] is True and sync is not None and abs(sync - 1) <= .05,
                "A1 matched offer, hold or sync grouping differs")
    return out


def regime_samples(row, key, config, root, started, updated, baselines):
    case, grain, width = key
    require((row["case"], row["grain_ns"], row["width"], row["kind"]) ==
            (case, grain, width, "work_rate"), "regime row identity mismatch")
    work = regimes.WORKLOADS[case]
    count, checksum = regimes.receipt(case, work.size, ROUNDS[grain])
    expected_output = f"{count} {checksum}\n".encode()
    cpus = ",".join(map(str, config["server_cpus"][:width]))
    out, pair_paths = {}, set()
    for arm in ARMS:
        summary = row["samples"][arm]
        require(summary["status"] == "ok" and len(summary["arms"]) == 1, f"{arm} regime arm incomplete")
        sample, = summary["arms"]
        path = evidence_path(root, str(Path(sample["path"]) / "stdout"))
        pair_paths.add(path.parent.parent)
        require(sample["status"] == "ok" and sample["cpus"] == cpus and sample["iterations"] == count
                and sample["checksum"] == checksum and sample["sha256"] == hashlib.sha256(expected_output).hexdigest()
                and path.read_bytes() == expected_output, f"{arm} regime count, topology or output mismatch")
        if arm == "hirsel":
            require(sample["pool"]["workers"] == width, "Hirsel regime uses the wrong worker pool")
        timed_service = bool(work.connections or case == "checkbox")
        if timed_service:
            server_codes = (0, 130) if case == "checkbox" else (0,)
            require(sample["server_status"] in server_codes and sample["load_status"] == 0
                    and sample["load_cpus"] == ",".join(map(str, config["load_cpus"])),
                    f"{arm} regime server or loader failed")
        rate = count / sample["service_seconds" if timed_service else "seconds"]
        baseline = rate if width == 1 else baselines[case, grain, arm]
        speedup = rate / baseline
        require(close(sample["work_per_s"], rate) and close(summary["work_per_s"], rate)
                and close(summary["speedup_vs_w1"], speedup), f"{arm} regime work rate or speedup is invalid")
        if width == 1:
            baselines[case, grain, arm] = rate
        out[arm] = summary | {"work_per_s": rate, "speedup_vs_w1": speedup,
                              "timing": "service seconds" if timed_service else "process seconds"}
    require(len(pair_paths) == 1, "regime arms are from different holds")
    pair_path, = pair_paths
    pair = receipt.read(evidence_path(root, str(pair_path.relative_to(root) / "pair.json")))
    hold = pair["hold"]
    require(hold["clean"] is True and clean_hold(hold, started, updated)
            and (hold["case"], hold["grain_ns"], hold["width"]) == key
            and sorted(hold["order"]) == list(ARMS)
            and pair["samples"] == {arm: out[arm]["arms"][0] for arm in ARMS},
            "regime pair receipt or hold is unqualified")
    return out


def import_rows(path, commit):
    path = path.resolve()
    export = receipt.read(path)
    keys, seen, config, started, updated = validate_config(export, commit)
    rows, baselines = [], {}
    for key, identity in keys.items():
        row = seen[key]
        try:
            require(row["commit"] == commit and row["status"] == "ok" and set(row["samples"]) == set(ARMS),
                    "units row revision or status mismatch")
            if key[0] == "A1":
                samples = a1_samples(row, identity, path.parent, started, updated)
                fields = MATCHED_METRICS if identity[-1] else CEILING_METRICS
                quantiles = tuple(field for field in QUANTILES if any(field in sample for sample in samples.values()))
                notes = "SHA-256 " + identity[2] + ", one durable sync per group."
            else:
                samples = regime_samples(row, identity, config, path.parent, started, updated, baselines)
                fields = REGIME_METRICS
                quantiles = ()
                case = identity[0]
                notes = "Headline case." if case in HEADLINE else ""
                if case in ("arc_hub", "checkbox"):
                    notes += " One runnable unit."
            goal, name = board_label(key)
            result = comparison.result_row(goal, name, fields, samples, samples, {}, peers=("tokio",))
            for field in quantiles:
                if ratio := comparison.divide(samples["hirsel"].get(field), samples["tokio"].get(field)):
                    result["ratios"]["tokio"][field] = ratio
            result["metrics"] += quantiles
            rows.append(result | {"notes": notes, "mode": "full", "commit": commit})
        except (OSError, ValueError, KeyError, TypeError, ArithmeticError) as error:
            rows.append(missing_row(key, str(error), status="incomplete"))
    for key in expected(tmpfs_matched=True):
        if key not in keys:
            rows.append(missing_row(key, "owner did not export the tmpfs matched offer"))
    return rows
