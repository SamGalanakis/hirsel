#!/usr/bin/env python3
"""Sample checked release rows and compare their user-instruction receipts."""
import argparse
from contextlib import contextmanager, nullcontext
from decimal import Decimal, InvalidOperation
import fcntl
import hashlib
import json
import math
import os
from pathlib import Path
import random
import re
from statistics import median
import subprocess
import sys
import tempfile

import build
from build import sha256
import elf_census
import measure
import profile_diff
from bench_cpus import positive_int, sibling_cpus, smt_activity
from measurement_artifacts import SHIPPING, check

VERSION = 3
REFUSED = 2
INSTRUCTION_LIMIT = Decimal("0.5")
CYCLE_RIDE_RATIO = Decimal("1.01")
INVALID_RECEIPT = (KeyError, OSError, ValueError, TypeError, AttributeError, re.error)
ARTIFACT_HASHES = ("compiler_sha256", "runtime_archive_sha256")
CANDIDATE_HASHES = (*ARTIFACT_HASHES, "std_sha256")
WORKLOAD_IDENTITY = ("suite", "mode", "certified", "flags", "argv", "affinity", "fixture_sha256")
IDENTITY = (*WORKLOAD_IDENTITY, *CANDIDATE_HASHES)


def read(path):
    return json.loads(path.read_text())


def write(path, data):
    temporary = path.with_suffix(".tmp")
    temporary.write_text(json.dumps(data, indent=2, default=str) + "\n")
    temporary.replace(path)


@contextmanager
def measurement_slot(path):
    with path.open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        yield


def release_tools(artifacts):
    if (artifacts["compiler_build_profile"] != "release" or artifacts["runtime_archive_profile"] != SHIPPING
            or artifacts["diagnostic"]):
        raise ValueError("expected release compiler and shipping fat-LTO runtime")
    for role in ("compiler", "runtime"):
        check(Path(artifacts[role]), artifacts)
    return {key: artifacts[key] for key in ARTIFACT_HASHES}


def fixture_identity(manifest, runtime_fixture):
    sources = build.manifest_digests(manifest)
    digest = sha256(runtime_fixture)
    return {
        **sources,
        "fixture_sha256": hashlib.sha256(bytes.fromhex(sources["workload_sources_sha256"] + digest)).hexdigest(),
        "fixture_files": {str(runtime_fixture): digest},
    }


def runtime_fixture(suite, directory, upstream):
    if suite == "arena":
        return fixture_identity(directory / "inputs.json", directory / "one-iteration.json")
    return fixture_identity(directory / "hirsel-inputs.json", upstream / "rebench.conf")


def build_record(report):
    native = report["native"]
    return {
        **report["measurement"],
        "accepted": report["accepted"] is True and not report.get("stopped"),
        "linked": native["linked"],
        "certified": native.get("step_charging_elided", False),
    }


def verified_build(row):
    record = row.get("build_record")
    if not isinstance(record, dict) or record.get("accepted") is not True or record.get("linked") is not True:
        return False
    for field in (*ARTIFACT_HASHES, *build.MANIFEST_HASHES, "image_sha256"):
        value = row.get(field)
        if not isinstance(value, str) or not re.fullmatch("[a-f0-9]{64}", value) or record.get(field) != value:
            return False
    return record.get("certified") is row["certified"] and record.get("flags") == row["flags"]


def runtime_case(suite, name, directory, command, marker, fixture, tools):
    binary = command[0]
    path = binary.with_name(f"{binary.name}-build.json")
    report = read(path)
    if report.get("accepted") is not True or report.get("stopped") or not report["native"]["linked"]:
        raise ValueError(f"build not accepted and linked: {path}")
    native = report["native"]
    if Path(native["executable"]).resolve() != binary.resolve():
        raise ValueError(f"{binary}: native build receipt names a different executable")
    provenance = report.get("measurement")
    if provenance is None:
        raise ValueError(f"{path}: build with the release row adapter to record measurement provenance")
    image = sha256(binary)
    checked = tools | {field: fixture[field] for field in build.MANIFEST_HASHES} | {"image_sha256": image}
    for field, value in checked.items():
        if provenance[field] != value:
            raise ValueError(f"{path}: {field} differs from the build")
    certified = native.get("step_charging_elided", False)
    return {
        "key": f"{suite}/{name}",
        "suite": suite,
        "mode": "certified" if certified else "bounded",
        "certified": certified,
        "flags": provenance["flags"],
        "argv": [f"./{binary.name}", *map(str, command[1:])],
        "cwd": str(directory),
        "image_sha256": image,
        "build_record": build_record(report),
        "marker": marker.decode(),
        **fixture,
        **tools,
    }


def counter_ready(sample, event):
    counters = sample.get("pmu_times_ns")
    times = counters.get(event) if isinstance(counters, dict) else None
    return isinstance(times, list) and bool(times) and all(
        isinstance(pair, list) and len(pair) == 2 and all(type(value) is int for value in pair)
        and pair[0] == pair[1] > 0 for pair in times
    )


def observe(row, target, env, siblings, *, cycles=False, label=None, quiet_window=False):
    row["affinity"] = sorted(os.sched_getaffinity(0))
    activity = nullcontext() if quiet_window else smt_activity(siblings, label)
    options = {"smt_siblings": siblings} if quiet_window else {}
    with activity as busy:
        sample = measure.core_measure.sample(row["argv"], target, env=env, cwd=row["cwd"], cycles=cycles,
                                            pmu_times=True, **options)
    if not quiet_window:
        sample["smt_busy_percent"] = busy
    sample["verified"] = (
        sample["status"] in ("ok", "missing-counter")
        and re.fullmatch(row["marker"].encode(), (target / "stdout").read_bytes()) is not None
    )
    if sample["status"] == "ok":
        if not sample["verified"]:
            sample["status"] = "wrong-result"
        elif not all(counter_ready(sample, event + ("" if quiet_window else ":u"))
                     for event in (("instructions", "cycles") if cycles else ("instructions",))):
            sample["status"] = "inactive-counter"
    row["stdout"] = (target / "stdout").read_text(errors="replace")
    row["sample"] = sample
    return sample


def make_receipt(expected):
    return {"version": VERSION, "expected_rows": list(expected), "rows": [], "failures": {}}


def runtime_rows(args):
    for suite, directory in (("awfy", args.awfy_build), ("pool", args.pool_build), ("arena", args.arena_build)):
        if directory is not None:
            cases = list(measure.runtime_cases(suite, directory, args.upstream))
            if sorted(name for name, _ in cases) != sorted(measure.EXPECTED[measure.SUITES[suite]]):
                raise ValueError(f"{suite}: fixture does not have the expected rows")
            for name, commands in cases:
                yield f"{suite}/{name}", (suite, name, directory, *commands["hirsel"])


def sample(args):
    if not any((args.awfy_build, args.pool_build, args.arena_build)):
        raise ValueError("choose at least one build directory")
    if (args.awfy_build or args.pool_build) and args.upstream is None:
        raise ValueError("--upstream is required for the AWFY workload sizes")
    out = args.out.resolve()
    if out.is_relative_to(build.ROOT):
        raise ValueError("use a new evidence directory outside the checkout")
    if args.cpu not in os.sched_getaffinity(0):
        raise ValueError(f"CPU {args.cpu} is outside the assigned affinity")
    siblings = sibling_cpus((args.cpu,))
    cases = dict(runtime_rows(args))
    if args.only:
        if len(set(args.only)) != len(args.only) or set(args.only) - cases.keys():
            raise ValueError("unknown or repeated row selection")
        cases = {key: cases[key] for key in args.only}
    if not cases:
        raise ValueError("no runtime rows selected")
    out.mkdir(parents=True)
    receipt = make_receipt(cases)
    target = out / "receipt.json"
    write(target, receipt)
    tools = release_tools(read(args.artifacts))
    fixtures = {}
    rows = {}
    for key, (suite, name, directory, command, marker) in cases.items():
        try:
            if suite not in fixtures:
                fixtures[suite] = runtime_fixture(suite, directory, args.upstream)
            rows[key] = runtime_case(suite, name, directory, command, marker, fixtures[suite], tools)
        except (OSError, ValueError, KeyError) as error:
            receipt["failures"][key] = str(error)
    env = build.capped_env(out)
    with measurement_slot(args.lock):
        os.sched_setaffinity(0, {args.cpu})
        for key, row in rows.items():
            try:
                observed = observe(row, out / key.replace("/", "-", 1), env, siblings, label=key)
                receipt["rows"].append(row)
                if observed["status"] != "ok":
                    receipt["failures"][key] = observed["status"]
            except (OSError, ValueError, subprocess.SubprocessError) as error:
                receipt["failures"][key] = str(error)
            write(target, receipt)
    write(target, receipt)
    for key, reason in receipt["failures"].items():
        print(f"{key}: {reason}", file=sys.stderr)
    print(target)
    return int(bool(receipt["failures"]))


def instruction_rows(path, expected=None, *, allow_incomplete=False):
    data = read(path)
    if "instruction_receipt" in data:
        if data["complete"] is not True and not allow_incomplete:
            raise ValueError(f"{path}: incomplete board")
        data = data["instruction_receipt"]
    if data["version"] != VERSION:
        raise ValueError(f"{path}: unsupported instruction receipt")
    declared = data["expected_rows"]
    if (not isinstance(declared, list) or not declared
            or not all(isinstance(key, str) and key for key in declared)
            or len(set(declared)) != len(declared)):
        raise ValueError(f"{path}: invalid expected rows")

    keys = set(declared) if expected is None else set(expected)
    rows, refusals = {}, {}
    for index, row in enumerate(data["rows"], 1):
        key = row.get("key") if isinstance(row, dict) else None
        if not isinstance(key, str) or not key:
            refusals[f"{path} row {index}"] = "missing or invalid row key"
            continue
        if key in rows:
            refusals[key] = f"{path}: duplicate row"
        rows[key] = row
    for key in rows.keys() - keys:
        refusals[key] = f"{path}: unexpected row"
    for key in keys - rows.keys():
        refusals[key] = f"{path}: missing row (missing: {[key]})"
    for key in keys ^ set(declared):
        refusals[key] = f"{path}: row declaration differs"
    for key, reason in data["failures"].items():
        refusals[key] = f"{path}: failed sample: {reason}"
    return rows, refusals


def validate_row(key, row):
    for field in IDENTITY:
        if field not in row or row[field] is None or (field != "certified" and not row[field]):
            raise ValueError(f"missing {field}")
    suite, mode, certified = row["suite"], row["mode"], row["certified"]
    if (suite not in measure.SUITES or not key.startswith(suite + "/")
            or type(certified) is not bool
            or (mode not in ("cold", "edit") if suite == "compile"
                else mode != ("certified" if certified else "bounded"))
            or any(not re.fullmatch("[a-f0-9]{64}", row[field])
                   for field in (*CANDIDATE_HASHES, "fixture_sha256"))
            or not all(isinstance(row[field], list)
                       and all(isinstance(value, str) and value for value in row[field])
                       for field in ("argv", "flags"))
            or not isinstance(row["affinity"], list)
            or any(type(cpu) is not int or cpu < 0 for cpu in row["affinity"])):
        raise ValueError("invalid identity")
    sample = row["sample"]
    if sample["status"] == "inactive-counter" or not counter_ready(sample, "instructions:u"):
        raise ValueError("inactive-counter (instructions:u must be fully active)")
    if (sample["status"] != "ok" or sample["verified"] is not True
            or type(sample["instructions"]) is not int or sample["instructions"] <= 0
            or not row["stdout"] or not re.fullmatch(row["marker"], row["stdout"])):
        raise ValueError("lacks a verified instruction sample")


def rows_from(path):
    rows, refusals = instruction_rows(path)
    if refusals:
        raise ValueError(str(refusals))
    for key, row in rows.items():
        validate_row(key, row)
    return rows


def match_rows(key, left, right):
    fields = [field for field in (*WORKLOAD_IDENTITY, "marker", "stdout") if left[field] != right[field]]
    if fields:
        raise ValueError(f"identity differs: {key}: {', '.join(fields)}")
    base_hashes = tuple(left[field] for field in CANDIDATE_HASHES)
    candidate_hashes = tuple(right[field] for field in CANDIDATE_HASHES)
    if base_hashes != candidate_hashes and not all(verified_build(row) for row in (left, right)):
        raise ValueError(f"{key} requires a verified build record for a candidate pair")
    return base_hashes, candidate_hashes


def compare(before_path, after_path, threshold, *, aa=False, expected=None, blocked=None):
    before, before_refusals = instruction_rows(before_path, expected)
    after, after_refusals = instruction_rows(after_path, expected, allow_incomplete=expected is not None)
    refusals = (blocked or {}) | before_refusals | after_refusals
    matched = []
    candidate_pairs = set()
    for key in sorted(before.keys() | after.keys() | refusals.keys()):
        try:
            if key in refusals:
                raise ValueError(refusals[key])
            if key not in before or key not in after:
                raise ValueError("row sets differ")
            left, right = before[key], after[key]
            for path, row in ((before_path, left), (after_path, right)):
                try:
                    validate_row(key, row)
                except INVALID_RECEIPT as error:
                    raise ValueError(f"{path}: {error}") from error
            candidate_pairs.add(match_rows(key, left, right))
        except INVALID_RECEIPT as error:
            refusals[key] = str(error)
            print(f"{key} REFUSED ({error})", file=sys.stderr)
            continue

        matched.append((key, left["sample"]["instructions"], right["sample"]["instructions"]))

    changed_pairs = sorted((left, right) for left, right in candidate_pairs if left != right)
    if aa and changed_pairs:
        fields = [field for index, field in enumerate(CANDIDATE_HASHES)
                  if any(left[index] != right[index] for left, right in changed_pairs)]
        print(f"--aa requires identical candidate hashes. Differing hashes: {', '.join(fields)}", file=sys.stderr)
        return REFUSED
    if not aa and not changed_pairs and not refusals:
        print("nothing differs: A/A. Use --aa for deliberate A/A comparisons.", file=sys.stderr)
        for hashes, _ in sorted(candidate_pairs):
            print(", ".join(f"{field}={value}" for field, value in zip(CANDIDATE_HASHES, hashes)), file=sys.stderr)
        return REFUSED

    failed = False
    ratios = []
    for key, baseline, candidate in matched:
        percent = Decimal(100) * (candidate - baseline) / baseline
        breach = percent > threshold
        failed |= breach
        ratios.append(candidate / baseline)
        print(f"{key} {baseline:,} -> {candidate:,} ({percent:+.3f}%){' FAIL' if breach else ''}")
    for (base_compiler, base_runtime, base_std), (candidate_compiler, candidate_runtime, candidate_std) in changed_pairs:
        if base_compiler != candidate_compiler:
            print(f"Variable compiler_sha256 {base_compiler} -> {candidate_compiler}")
        runtime = "unchanged" if base_runtime == candidate_runtime else f"{base_runtime} -> {candidate_runtime}"
        print(f"Paired runtime_archive_sha256 {runtime}")
        if base_std != candidate_std:
            print(f"Variable std_sha256 {base_std} -> {candidate_std}")
    if ratios:
        geomean = math.expm1(math.fsum(map(math.log, ratios)) / len(ratios)) * 100
        print(f"{len(ratios)} rows compared, geomean {geomean:+.3f}%, threshold +{threshold}%")
    return REFUSED if refusals else int(failed)


def select_pair_rows(value, cases):
    keys = []
    for name in value.split(","):
        matches = [key for key in cases if key == name or key.split("/", 1)[1] == name]
        if len(matches) != 1 or matches[0] in keys:
            raise ValueError(f"unknown, ambiguous or repeated row {name!r}; use SUITE/ROW to disambiguate")
        keys.append(matches[0])
    return keys


def verify_runtime_files(row, label):
    if not row["argv"][0].startswith("./"):
        raise ValueError(f"{label}: expected a runtime executable relative to cwd")
    path = (Path(row["cwd"]) / row["argv"][0]).resolve()
    if sha256(path) != row["image_sha256"]:
        raise ValueError(f"{label}: executable no longer matches the measured image")
    for fixture, digest in row["fixture_files"].items():
        if sha256(Path(fixture)) != digest:
            raise ValueError(f"{label}: measured fixture has changed: {fixture}")
    return path


def receipt_in(directory):
    path = directory / "receipt.json"
    return instruction_rows(path if path.is_file() else directory / "board.json")


def quiet_cycle_samples(base, candidate, args, siblings, env):
    kept = []
    for attempt in range(1, args.max_attempts + 1):
        sides = [("base", base), ("candidate", candidate)]
        random.shuffle(sides)
        samples = {}
        with tempfile.TemporaryDirectory() as temporary:
            for side, row in sides:
                sample = observe(row.copy(), Path(temporary) / side, env, siblings, cycles=True, quiet_window=True)
                if sample["status"] != "ok":
                    detail = (Path(temporary) / side / "stderr").read_text().strip()
                    raise ValueError(f"attempt {attempt}, {side}: {sample['status']}\n{detail}")
                samples[side] = sample
        if all((busy := sample["smt_busy_percent"][cpu]) is not None and busy < args.sibling_busy_threshold
               for sample in samples.values() for cpu in siblings):
            kept.append((samples["base"], samples["candidate"]))
            if len(kept) == args.pairs:
                break
    return kept, attempt


def report_cycle_pairs(key, kept, attempted, wanted):
    if kept:
        cycles, instructions = (
            [Decimal(right[event]) / left[event] for left, right in kept] for event in ("cycles", "instructions")
        )
        center = median(cycles)
        print(f"{key}: {len(kept)}/{attempted} quiet pairs, cycles {center:.6f}x "
              f"[{min(cycles):.6f}x, {max(cycles):.6f}x], instructions {median(instructions):.6f}x")
        for cpu in kept[0][0]["smt_busy_percent"]:
            rates = [sample["smt_busy_percent"][cpu] for pair in kept for sample in pair]
            print(f"  CPU {cpu} busy {min(rates):.2f}% to {max(rates):.2f}% in kept runs")
    else:
        print(f"{key}: 0/{attempted} quiet pairs")
    if len(kept) < wanted:
        verdict, status = "TOO FEW QUIET PAIRS", REFUSED
    elif center > CYCLE_RIDE_RATIO:
        verdict, status = "FAILS", 1
    else:
        verdict, status = "RIDES", 0
    print(f"{key} {verdict}", flush=True)
    return status


def cycle_pairs(args):
    if not 0 < args.sibling_busy_threshold <= 100:
        raise ValueError("sibling busy threshold must be above 0 and at most 100 percent")
    affinity = os.sched_getaffinity(0)
    if args.cpu not in affinity:
        raise ValueError(f"CPU {args.cpu} is outside the assigned affinity")
    with measurement_slot(args.lock), tempfile.TemporaryDirectory() as temporary:
        siblings = sibling_cpus((args.cpu,))
        base, base_refusals = receipt_in(args.baseline)
        after, after_refusals = receipt_in(args.candidate)
        keys = select_pair_rows(args.rows, base)
        env = build.capped_env(Path(temporary))
        print(f"CPU {args.cpu}; SMT siblings {', '.join(map(str, siblings)) or 'none'}; "
              f"quiet means below {args.sibling_busy_threshold}% busy per run", flush=True)
        status = 0
        try:
            os.sched_setaffinity(0, {args.cpu})
            for key in keys:
                try:
                    if key in base_refusals or key in after_refusals:
                        raise ValueError((base_refusals | after_refusals)[key])
                    left, right = base[key].copy(), after[key].copy()
                    for label, row in (("base", left), ("candidate", right)):
                        validate_row(key, row)
                        if row["suite"] == "compile" or not verified_build(row):
                            raise ValueError(f"{label}: expected a verified runtime build")
                        verify_runtime_files(row, label)
                        row["affinity"] = [args.cpu]
                    match_rows(key, left, right)
                    same = all(left[field] == right[field] for field in (*CANDIDATE_HASHES, "image_sha256"))
                    if same != args.aa:
                        raise ValueError("--aa requires identical builds and images" if args.aa
                                         else "identical builds and images; pass --aa for a deliberate A/A run")
                    kept, attempted = quiet_cycle_samples(left, right, args, siblings, env)
                    status = max(status, report_cycle_pairs(key, kept, attempted, args.pairs))
                except (*INVALID_RECEIPT, subprocess.SubprocessError) as error:
                    print(f"{key} REFUSED ({error})", file=sys.stderr)
                    status = REFUSED
        finally:
            os.sched_setaffinity(0, affinity)
        return status


def prepare_pair_rows(cases, keys, artifacts, upstream):
    tools = release_tools(read(artifacts))
    fixtures = {}
    rows = {}
    for key in keys:
        suite, name, directory, command, marker = cases[key]
        if suite not in fixtures:
            fixtures[suite] = runtime_fixture(suite, directory, upstream)
        rows[key] = runtime_case(suite, name, directory, command, marker, fixtures[suite], tools)
    return rows


def cycle_count(key, row):
    validate_row(key, row)
    value = row["sample"].get("cycles")
    if type(value) is not int or value <= 0 or not counter_ready(row["sample"], "cycles:u"):
        raise ValueError(f"{key}: cycles:u must be positive and fully active")
    return value


def change(before, after):
    return Decimal(100) * (after - before) / before


def aa_spread(key, pairs, baseline, cpu, count):
    if len(pairs) != count:
        raise ValueError(f"{key}: A/A control must have {count} pairs")
    changes = []
    for pair in pairs:
        sides = pair["base"], pair["candidate"]
        values = []
        for row in sides:
            values.append(cycle_count(key, row))
            if row["affinity"] != [cpu] or any(
                row[field] != baseline[field] for field in (*IDENTITY, "image_sha256", "marker")
                if field != "affinity"
            ):
                raise ValueError(f"{key}: A/A does not match the baseline workload, image and CPU")
        changes.append(change(*values))
    return min(changes), max(changes)


def summarize_pairs(key, pairs, noise, limit):
    rates = {}
    shifted = False
    for pair in pairs:
        left, right = (pair[side]["sample"]["smt_busy_percent"] for side in ("base", "candidate"))
        for cpu, before in left.items():
            after = right[cpu]
            rates.setdefault(cpu, []).extend((before, after))
            shifted |= round(abs(after - before), 9) > 20
    ranges = {cpu: (min(values), max(values)) for cpu, values in rates.items()}
    activity = ", ".join(f"CPU {cpu} {low:.2f}–{high:.2f}% busy" for cpu, (low, high) in ranges.items())
    summary = {"smt_busy_range_percent": ranges}
    for event in ("instructions", "cycles"):
        left = [pair["base"]["sample"][event] for pair in pairs]
        right = [pair["candidate"]["sample"][event] for pair in pairs]
        deltas = [change(a, b) for a, b in zip(left, right)]
        center, low, high = median(deltas), min(deltas), max(deltas)
        verdict = ""
        if event == "instructions" and limit is not None and center > limit:
            verdict = "FAIL"
        detail = ""
        if event == "cycles":
            if shifted:
                verdict = "sibling busy"
            elif noise is not None:
                verdict = "within noise" if noise[0] <= center <= noise[1] else "outside A/A spread"
            if noise is not None:
                detail += f" (A/A {noise[0]:+.3f}% to {noise[1]:+.3f}%)"
            if activity:
                detail += f" | SMT {activity}"
        print(f"{key} {event}: {median(left):,} -> {median(right):,}, paired median {center:+.3f}% "
              f"[{low:+.3f}%, {high:+.3f}%] {verdict}{detail}", flush=True)
        summary[event] = {"base_median": median(left), "candidate_median": median(right),
                          "median_percent": str(center), "min_percent": str(low), "max_percent": str(high),
                          "verdict": verdict}
    return summary


def pair_samples(args):
    candidate = argparse.Namespace(
        upstream=args.upstream, awfy_build=args.candidate_awfy_build,
        pool_build=args.candidate_pool_build, arena_build=args.candidate_arena_build)
    comparing = args.candidate_artifacts is not None
    if comparing != bool(args.aa) or (not comparing and any(
            (candidate.awfy_build, candidate.pool_build, candidate.arena_build))):
        raise ValueError("A/B requires --candidate-artifacts, candidate build directories and --aa")
    base_cases = dict(runtime_rows(args))
    keys = select_pair_rows(args.rows, base_cases)
    base = prepare_pair_rows(base_cases, keys, args.artifacts, args.upstream)
    after, noise = base, {}
    if comparing:
        after = prepare_pair_rows(dict(runtime_rows(candidate)), keys, args.candidate_artifacts, args.upstream)
        control = read(args.aa)
        if control["comparison"] != "A/A":
            raise ValueError("--aa must name an A/A pair receipt")
        for key in keys:
            if key in control["failures"]:
                raise ValueError(f"{key}: A/A control failed: {control['failures'][key]}")
            noise[key] = aa_spread(key, control["rows"][key], base[key], args.cpu, args.pairs)

    out = args.out.resolve()
    if out.is_relative_to(build.ROOT):
        raise ValueError("use a new evidence directory outside the checkout")
    if args.cpu not in os.sched_getaffinity(0):
        raise ValueError(f"CPU {args.cpu} is outside the assigned affinity")
    siblings = sibling_cpus((args.cpu,))
    env = build.capped_env(out)
    report = {"comparison": "A/B" if comparing else "A/A", "cpu": args.cpu, "pairs": args.pairs,
              "aa": args.aa, "rows": {}, "summary": {}, "failures": {}}
    out.mkdir(parents=True)
    target = out / "pairs.json"
    limit = INSTRUCTION_LIMIT if args.threshold is None else args.threshold
    if not comparing:
        limit = None
    failed = False
    with measurement_slot(args.lock):
        os.sched_setaffinity(0, {args.cpu})
        for key in keys:
            pairs = report["rows"][key] = []
            row_dir = out / key
            row_dir.mkdir(parents=True)
            try:
                for index in range(args.pairs):
                    pair = {}
                    pairs.append(pair)
                    sides = (("base", base[key]), ("candidate", after[key]))
                    for side, prepared in (sides if index % 2 == 0 else reversed(sides)):
                        row = pair[side] = prepared.copy()
                        observe(row, row_dir / f"{index + 1}-{side}", env, siblings, cycles=True)
                        cycle_count(key, row)
                    match_rows(key, pair["base"], pair["candidate"])
                summary = summarize_pairs(key, pairs, noise.get(key), limit)
                report["summary"][key] = summary
                failed |= summary["instructions"]["verdict"] == "FAIL"
            except (*INVALID_RECEIPT, subprocess.SubprocessError) as error:
                report["failures"][key] = str(error)
                print(f"{key} REFUSED ({error})", file=sys.stderr)
            write(target, report)
    print(target)
    return REFUSED if report["failures"] else int(failed)


def profile(args):
    before, after = rows_from(args.baseline), rows_from(args.candidate)
    left, right = before[args.row], after[args.row]
    if left["suite"] == "compile":
        raise ValueError("profile-diff requires a runtime row")
    match_rows(args.row, left, right)
    affinity = set(left["affinity"])
    assigned = os.sched_getaffinity(0)
    if not affinity <= assigned:
        raise ValueError(f"run on the recorded affinity {sorted(affinity)}")
    out = args.out.resolve()
    if out.is_relative_to(build.ROOT):
        raise ValueError("use a new evidence directory outside the checkout")
    baseline = left["sample"]["instructions"]
    candidate = right["sample"]["instructions"]
    percent = Decimal(100) * (candidate - baseline) / baseline
    print(f"{args.row} PMU instructions:u {baseline:,} -> {candidate:,} ({percent:+.3f}%)", flush=True)
    for field in CANDIDATE_HASHES:
        if left[field] != right[field]:
            print(f"{field} {left[field]} -> {right[field]}")
    sides = []
    for label, row in (("baseline", left), ("candidate", right)):
        sides.append((out / label, row, verify_runtime_files(row, label)))
    out.mkdir(parents=True)
    try:
        with measurement_slot(args.lock):
            os.sched_setaffinity(0, affinity)
            for directory, row, _ in sides:
                directory.mkdir()
                profile_diff.capture(row, directory)
    finally:
        os.sched_setaffinity(0, assigned)
    mapped = []
    for directory, _, executable in sides:
        addresses = profile_diff.mapped_counts(directory, executable)
        write(directory / "addresses.json", addresses)
        mapped.append(addresses)
    profile_diff.summarize(*mapped)
    print(f"Profiles and source maps: {out}")
    return 0


def threshold(value):
    try:
        result = Decimal(value)
        if result.is_finite() and result >= 0:
            return result
    except InvalidOperation:
        pass
    raise argparse.ArgumentTypeError("threshold must be a finite non-negative percentage")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="action", required=True)
    sampling = commands.add_parser("sample", help="sample checked Hirsel runtime rows")
    sampling.add_argument("out", type=Path)
    sampling.add_argument("--artifacts", type=Path, required=True, help="release measurement_artifacts.py JSON")
    sampling.add_argument("--cpu", type=int, required=True)
    sampling.add_argument("--lock", type=Path, required=True)
    for option in ("upstream", "awfy-build", "pool-build", "arena-build"):
        sampling.add_argument(f"--{option}", type=lambda path: Path(path).resolve())
    selection = sampling.add_mutually_exclusive_group()
    selection.add_argument("--only", nargs="+", metavar="SUITE/ROW")
    selection.add_argument("--rows", metavar="ROW,ROW", help="comma-separated names for paired sampling")
    sampling.add_argument("--pairs", type=positive_int, metavar="N", help="interleaved repetitions per row")
    sampling.add_argument("--aa", type=Path, help="matching A/A pairs.json for an A/B comparison")
    sampling.add_argument("--candidate-artifacts", type=Path, help="release toolchain for A/B sampling")
    for suite in ("awfy", "pool", "arena"):
        sampling.add_argument(f"--candidate-{suite}-build", type=lambda path: Path(path).resolve())
    sampling.add_argument("--threshold", type=threshold, metavar="PERCENT")
    comparison = commands.add_parser("compare", help="compare matched instruction receipts")
    comparison.add_argument("baseline", type=Path)
    comparison.add_argument("candidate", type=Path)
    comparison.add_argument("--threshold", type=threshold, default=INSTRUCTION_LIMIT, metavar="PERCENT")
    comparison.add_argument("--aa", action="store_true", help="require identical compiler, runtime and std/core hashes")
    cycles = commands.add_parser("cycle-pairs", help="qualify named runtime rows with quiet interleaved counter pairs")
    cycles.add_argument("baseline", type=Path, help="base directory containing receipt.json or board.json")
    cycles.add_argument("candidate", type=Path, help="candidate directory containing receipt.json or board.json")
    cycles.add_argument("--rows", required=True, metavar="ROW,ROW", help="comma-separated runtime row names")
    cycles.add_argument("--cpu", type=int, required=True, help="measurement CPU")
    cycles.add_argument("--pairs", type=positive_int, default=9, metavar="N", help="quiet pairs per row (default: 9)")
    cycles.add_argument("--max-attempts", type=positive_int, default=60, metavar="N",
                        help="attempted pairs per row (default: 60)")
    cycles.add_argument("--sibling-busy-threshold", type=threshold, default=Decimal(5), metavar="PERCENT",
                        help="each run must be strictly below this SMT sibling busy percentage (default: 5)")
    cycles.add_argument("--lock", type=Path, required=True)
    cycles.add_argument("--aa", action="store_true", help="require identical builds and images")
    profiling = commands.add_parser("profile-diff", help="attribute one matched runtime row with Callgrind")
    profiling.add_argument("row", metavar="SUITE/ROW")
    profiling.add_argument("baseline", type=Path)
    profiling.add_argument("candidate", type=Path)
    profiling.add_argument("out", type=Path)
    profiling.add_argument("--lock", type=Path, required=True)
    census = commands.add_parser("elf-census", help="compare file bytes and symbol owners in two ELF images")
    census.add_argument("baseline", type=Path)
    census.add_argument("candidate", type=Path)
    args = parser.parse_args()
    try:
        if args.action == "sample":
            if args.pairs or args.rows is not None:
                if args.pairs is None or args.rows is None:
                    sampling.error("--pairs and --rows are required together")
                if args.upstream is None and any((args.awfy_build, args.pool_build,
                                                 args.candidate_awfy_build, args.candidate_pool_build)):
                    sampling.error("--upstream is required for the AWFY workload sizes")
                return pair_samples(args)
            if args.threshold is not None or any((args.aa, args.candidate_artifacts, args.candidate_awfy_build,
                                                  args.candidate_pool_build, args.candidate_arena_build)):
                sampling.error("pair comparison options require --pairs and --rows")
            return sample(args)
        if args.action == "profile-diff":
            return profile(args)
        if args.action == "cycle-pairs":
            return cycle_pairs(args)
        if args.action == "elf-census":
            print(json.dumps(elf_census.census(args.baseline, args.candidate), indent=2))
            return 0
        return compare(args.baseline, args.candidate, args.threshold, aa=args.aa)
    except (KeyError, OSError, ValueError, TypeError, AttributeError, re.error, subprocess.SubprocessError) as error:
        print(f"{args.action}: {error}", file=sys.stderr)
        return REFUSED


if __name__ == "__main__":
    raise SystemExit(main())
