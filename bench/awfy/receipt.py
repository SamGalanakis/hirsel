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

import awfy_measure
import build
from build import sha256
import elf_census
import profile_diff
from bench_cpus import positive_int, sibling_cpus, smt_activity
from measurement_artifacts import SHIPPING, check

VERSION = 5
REFUSED = 2
INSTRUCTION_LIMIT = Decimal("0.5")
GUARD_B_CYCLE_RATIO = Decimal("1.03")
INVALID_RECEIPT = (KeyError, OSError, ValueError, TypeError, AttributeError, re.error)
ARTIFACT_HASHES = ("compiler_sha256", "runtime_archive_sha256")
CANDIDATE_HASHES = (*ARTIFACT_HASHES, "std_sha256", "workload_sources_sha256", "image_sha256")
WORKLOAD_IDENTITY = ("suite", "mode", "flags", "argv", "affinity", "fixture_sha256")
IDENTITY = (*WORKLOAD_IDENTITY, *CANDIDATE_HASHES)


def read(path):
    return json.loads(path.read_text())


def write(path, data):
    with tempfile.TemporaryDirectory(dir=path.parent) as directory:
        temporary = Path(directory) / path.name
        temporary.write_text(json.dumps(data, indent=2, default=str) + "\n")
        temporary.replace(path)


@contextmanager
def measurement_slot(path, *, blocking=True):
    with path.open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | (0 if blocking else fcntl.LOCK_NB))
        yield


def release_tools(artifacts):
    if (artifacts["compiler_build_profile"] != "release" or artifacts["runtime_archive_profile"] != SHIPPING
            or artifacts["diagnostic"]):
        raise ValueError("expected release compiler and shipping fat-LTO runtime")
    for role in ("compiler", "runtime"):
        check(Path(artifacts[role]), artifacts)
    return {key: artifacts[key] for key in ARTIFACT_HASHES}


def fixture_identity(manifest, runtime_fixture):
    return build.manifest_digests(manifest) | data_fixture({str(runtime_fixture): sha256(runtime_fixture)})


def fixture_digest(files):
    # Callers record inputs in workload order. Paths are local to each build.
    digests = [bytes.fromhex(digest) for digest in files.values()]
    if not digests or any(len(digest) != 32 for digest in digests):
        raise ValueError("invalid fixture file hashes")
    if len(digests) == 1:
        return digests[0].hex()
    return hashlib.sha256(b"".join(digests)).hexdigest()


def data_fixture(files):
    return {"fixture_sha256": fixture_digest(files), "fixture_files": files}


def runtime_fixture(suite, directory, upstream, *, config=None):
    if suite == "arena":
        path = config if config is not None else directory / "one-iteration.json"
        return data_fixture({str(path): sha256(path)})
    return fixture_identity(directory / "hirsel-inputs.json", upstream / "rebench.conf")


def build_record(report):
    native = report["native"]
    return {
        **report["measurement"],
        "accepted": report["accepted"] is True and not report.get("stopped"),
        "linked": native["linked"],
    }


def verified_build(row):
    record = row.get("build_record")
    if not isinstance(record, dict) or record.get("accepted") is not True or record.get("linked") is not True:
        return False
    for field in (*ARTIFACT_HASHES, *build.MANIFEST_HASHES, "image_sha256"):
        value = row.get(field)
        if not isinstance(value, str) or not re.fullmatch("[a-f0-9]{64}", value) or record.get(field) != value:
            return False
    return record.get("flags") == row["flags"]


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
    if suite == "arena":
        manifest = binary.with_name(f"{binary.name}-inputs.json")
        fixture = fixture | build.manifest_digests(manifest)
    if (suite, name) in {("awfy", "Json"), ("pool", "JsonPool")}:
        data = directory / "rap.json"
        digest = sha256(data)
        if digest != provenance.get("rap_sha256"):
            raise ValueError(f"{data}: JSON fixture differs from the build")
        if len(command) != 5 or hashlib.sha256(command[4].encode("utf-8")).hexdigest() != digest:
            raise ValueError(f"{data}: JSON argument differs from the fixture")
        fixture = fixture | data_fixture(fixture["fixture_files"] | {str(data): digest})
    image = sha256(binary)
    checked = tools | {field: fixture[field] for field in build.MANIFEST_HASHES} | {"image_sha256": image}
    for field, value in checked.items():
        if provenance[field] != value:
            raise ValueError(f"{path}: {field} differs from the build")
    return {
        "key": f"{suite}/{name}",
        "suite": suite,
        "mode": "release",
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
    if "stdin" in row:
        options["stdin"] = row["stdin"]
    with activity as busy:
        sample = awfy_measure.core_measure.sample(row["argv"], target, env=env, cwd=row["cwd"], cycles=cycles,
                                                  pmu_times=True, **options)
    if not quiet_window:
        sample["smt_busy_percent"] = busy
    output = (target / "stdout").read_bytes()
    if (verify := row.get("verify")) is not None:
        valid_output = verify(output)
    else:
        lines = output.splitlines(keepends=True) if row.get("line_marker") else [output]
        marker = row["marker"].encode()
        valid_output = any(re.fullmatch(marker, line) for line in lines)
    sample["verified"] = sample["status"] in ("ok", "missing-counter") and valid_output
    if sample["status"] == "ok":
        if not sample["verified"]:
            sample["status"] = "wrong-result"
        elif not all(counter_ready(sample, event + ("" if quiet_window else ":u"))
                     for event in (("instructions", "cycles") if cycles else ("instructions",))):
            sample["status"] = "inactive-counter"
    row["stdout"] = output.decode(errors="replace")
    row["sample"] = sample
    return sample


def make_receipt(expected):
    return {"version": VERSION, "expected_rows": list(expected), "rows": [], "failures": {}}


def runtime_rows(args):
    for suite, directory in (("awfy", args.awfy_build), ("pool", args.pool_build), ("arena", args.arena_build)):
        if directory is not None:
            cases = list(awfy_measure.runtime_cases(suite, directory, args.upstream))
            if sorted(name for name, _ in cases) != sorted(awfy_measure.EXPECTED[awfy_measure.SUITES[suite]]):
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


def instruction_rows(path, expected=None, *, allow_incomplete=False, compare_baseline=False):
    data = read(path)
    previous_board = False
    if "instruction_receipt" in data:
        if data["complete"] is not True and not allow_incomplete:
            raise ValueError(f"{path}: incomplete board")
        data = data["instruction_receipt"]
        previous_board = compare_baseline and data["version"] == 4
    if data["version"] != VERSION and not previous_board:
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
        if previous_board:
            try:
                row = board_v4_row(row)
            except INVALID_RECEIPT as error:
                refusals[key] = f"{path}: {error}"
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


def board_v4_row(row):
    # Version 4 used the runtime mode to record Step certification. Version 5
    # records a release build in either case. Compile modes did not change.
    certified, mode = row["certified"], row["mode"]
    if type(certified) is not bool:
        raise ValueError("invalid Step certification")
    if row["suite"] != "compile":
        if mode != ("certified" if certified else "bounded"):
            raise ValueError("runtime mode disagrees with Step certification")
        mode = "release"
    record = row.get("build_record")
    if record is not None and record.get("certified") is not certified:
        raise ValueError("build record disagrees with Step certification")
    return {**row, "mode": mode}


def validate_row(key, row):
    for field in IDENTITY:
        if field not in row or row[field] is None or not row[field]:
            raise ValueError(f"missing {field}")
    suite, mode = row["suite"], row["mode"]
    if (suite not in awfy_measure.SUITES or not key.startswith(suite + "/")
            or (mode not in ("cold", "edit") if suite == "compile"
                else mode != "release")
            or any(not re.fullmatch("[a-f0-9]{64}", row[field])
                   for field in (*CANDIDATE_HASHES, "fixture_sha256"))
            or not all(isinstance(row[field], list)
                       and all(isinstance(value, str) and value for value in row[field])
                       for field in ("argv", "flags"))
            or not isinstance(row["affinity"], list)
            or any(type(cpu) is not int or cpu < 0 for cpu in row["affinity"])):
        raise ValueError("invalid identity")
    if row["fixture_sha256"] != fixture_digest(row["fixture_files"]):
        raise ValueError("fixture_sha256 differs from the recorded fixture files")
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


def candidate_identity(row):
    return tuple(row[field] for field in CANDIDATE_HASHES)


def match_rows(key, left, right):
    fields = [field for field in (*WORKLOAD_IDENTITY, "marker", "stdout") if left[field] != right[field]]
    if fields:
        raise ValueError(f"identity differs: {key}: {', '.join(fields)}")
    base_hashes = candidate_identity(left)
    candidate_hashes = candidate_identity(right)
    if base_hashes != candidate_hashes and not all(verified_build(row) for row in (left, right)):
        raise ValueError(f"{key} requires a verified build record for a candidate pair")
    if (left["suite"] != "compile" and left["std_sha256"] != right["std_sha256"]
            and left["compiler_sha256"] == right["compiler_sha256"]
            and {"--image", "--source-std"}.isdisjoint(left["flags"])):
        raise ValueError(f"{key}: changed std with one compiler requires --whole-std on both builds "
                         "or --source-std on both native commands")
    return base_hashes, candidate_hashes


def compare(before_path, after_path, threshold, *, aa=False, expected=None, blocked=None):
    before, before_refusals = instruction_rows(before_path, expected, compare_baseline=True)
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
    for index, field in enumerate(CANDIDATE_HASHES):
        values = sorted({(left[index], right[index]) for left, right in changed_pairs})
        for left, right in values:
            if field == "runtime_archive_sha256":
                print(f"Paired {field} " + ("unchanged" if left == right else f"{left} -> {right}"))
            elif left != right:
                print(f"Variable {field} {left} -> {right}")
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


def receipt_path(directory):
    path = directory / "receipt.json"
    return path if path.is_file() else directory / "board.json"


def cycle_samples(base, candidate, args, siblings, env, pairs=None):
    kept = []
    limit = args.sibling_busy_threshold
    for attempt in range(1, args.max_attempts + 1):
        sides = [("base", base), ("candidate", candidate)]
        random.shuffle(sides)
        pair = {"order": [], "kept": False}
        if pairs is not None:
            pairs.append(pair)
        samples = {}
        with tempfile.TemporaryDirectory() as temporary:
            for side, row in sides:
                pair["order"].append(side)
                run = pair[side] = row.copy()
                run.pop("sample", None)
                run.pop("stdout", None)
                sample = observe(run, Path(temporary) / side, env, siblings, cycles=True, quiet_window=True)
                sample.pop("directory", None)
                if sample["status"] != "ok":
                    detail = (Path(temporary) / side / "stderr").read_text().strip()
                    raise ValueError(f"attempt {attempt}, {side}: {sample['status']}\n{detail}")
                samples[side] = sample
        if limit is None or all((busy := sample["smt_busy_percent"][cpu]) is not None and busy < limit
                                for sample in samples.values() for cpu in siblings):
            pair["kept"] = True
            kept.append((samples["base"], samples["candidate"]))
            if len(kept) == args.pairs:
                break
    return kept, attempt


def report_cycle_pairs(key, kept, attempted, wanted, *, filtered=True, peer=False, summary=None):
    description = "quiet pairs" if filtered else "unfiltered pairs"
    if kept:
        cycles, instructions = (
            [Decimal(right[event]) / left[event] for left, right in kept] for event in ("cycles", "instructions")
        )
        center = median(cycles)
        statistics = {}
        for event, ratios in (("cycles", cycles), ("instructions", instructions)):
            left, right = ([sample[event] for sample in side] for side in zip(*kept))
            statistics[event] = pair_statistics(left, right) | {
                "median_ratio": median(ratios), "min_ratio": min(ratios), "max_ratio": max(ratios),
                "verdict": "",
            }
        absolutes = statistics["cycles"]
        if summary is not None:
            summary.update(statistics)
            summary["smt_busy_range_percent"] = {}
        print(f"{key}: {len(kept)}/{attempted} {description}, cycles {center:.6f}x "
              f"[{min(cycles):.6f}x, {max(cycles):.6f}x], instructions {median(instructions):.6f}x, "
              f"base median {absolutes['base_median']:,} cycles, candidate median {absolutes['candidate_median']:,} cycles")
        for cpu in kept[0][0]["smt_busy_percent"]:
            rates = [rate for pair in kept for sample in pair
                     if (rate := sample["smt_busy_percent"][cpu]) is not None]
            if rates:
                low, high = min(rates), max(rates)
                if summary is not None:
                    summary["smt_busy_range_percent"][cpu] = (low, high)
                print(f"  CPU {cpu} busy {low:.2f}% to {high:.2f}% in kept runs")
    else:
        print(f"{key}: 0/{attempted} {description}")
    if len(kept) < wanted:
        verdict, status = "TOO FEW QUIET PAIRS", REFUSED
    elif peer:
        return 0
    elif center > GUARD_B_CYCLE_RATIO:
        verdict, status = "FAILS", 1
    else:
        verdict, status = "PASSES", 0
    if summary is not None:
        summary.setdefault("cycles", {})["verdict"] = verdict
    print(f"{key} {verdict}", flush=True)
    return status


def cycle_pairs(args):
    limit = args.sibling_busy_threshold
    affinity = os.sched_getaffinity(0)
    if args.cpu not in affinity:
        raise ValueError(f"CPU {args.cpu} is outside the assigned affinity")
    if args.peer and (args.candidate is not None or args.aa):
        raise ValueError("--peer takes one Hirsel receipt and cannot be combined with --aa")
    if not args.peer and (args.candidate is None or args.upstream is not None):
        raise ValueError("provide two receipts for a Hirsel comparison or use --peer")
    if args.multicore and (not args.peer or args.upstream is not None):
        raise ValueError("--multicore requires --peer and a multicore build directory")
    target = args.out or (args.candidate or args.baseline) / "pairs.json"
    inputs = [receipt_path(directory) for directory in (args.baseline, args.candidate) if directory is not None]
    if target.resolve() in {path.resolve() for path in [args.lock, *inputs]}:
        raise ValueError("--out must differ from the input receipts and lock")
    report = pair_report(args, "peer" if args.peer else "A/A" if args.aa else "A/B")
    with measurement_slot(args.lock), tempfile.TemporaryDirectory() as temporary:
        siblings = sibling_cpus((args.cpu,))
        env = build.capped_env(Path(temporary))
        activity = "no busy filter" if limit is None else f"quiet means below {limit}% busy per run"
        print(f"CPU {args.cpu}, SMT siblings {', '.join(map(str, siblings)) or 'none'}, {activity}", flush=True)
        try:
            if args.multicore:
                os.sched_setaffinity(0, {args.cpu})
                return multicore_peer_pairs(args, siblings, env, report)
            base, base_refusals = instruction_rows(inputs[0])
            after, after_refusals = instruction_rows(inputs[1]) if args.candidate else ({}, {})
            keys = select_pair_rows(args.rows, base)
            status = 0
            os.sched_setaffinity(0, {args.cpu})
            for key in keys:
                try:
                    if key in base_refusals or key in after_refusals:
                        raise ValueError((base_refusals | after_refusals)[key])
                    left = cycle_row(key, base[key], "base", args.cpu)
                    if args.peer:
                        status = max(status, peer_cycle_pairs(key, left, args, siblings, env, report))
                        continue
                    right = cycle_row(key, after[key], "candidate", args.cpu)
                    base_identity, candidate = match_rows(key, left, right)
                    same = base_identity == candidate
                    if same != args.aa:
                        raise ValueError("--aa requires identical builds and images" if args.aa
                                         else "identical builds and images; pass --aa for a deliberate A/A run")
                    status = max(status, run_cycle_pairs(key, left, right, args, siblings, env, report))
                except (*INVALID_RECEIPT, subprocess.SubprocessError) as error:
                    report["failures"][key] = str(error)
                    print(f"{key} REFUSED ({error})", file=sys.stderr)
                    status = REFUSED
            return status
        finally:
            os.sched_setaffinity(0, affinity)
            if report["rows"] or report["failures"]:
                write(target, report)
                print(target)


def cycle_row(key, source, label, cpu):
    validate_row(key, source)
    if source["suite"] == "compile" or not verified_build(source):
        raise ValueError(f"{label}: expected a verified runtime build")
    verify_runtime_files(source, label)
    return source | {"affinity": [cpu]}


def peer_cycle_pairs(key, hirsel, args, siblings, env, report=None):
    suite = hirsel["suite"]
    if suite != "arena" and args.upstream is None:
        raise ValueError("--upstream is required for AWFY and pool peer commands")
    name = key.split("/", 1)[1]
    cwd = Path(hirsel["cwd"])
    if suite == "arena":
        config, = map(Path, hirsel["fixture_files"])
        cases = dict(awfy_measure.arena_rows(cwd, config))
    else:
        cases = dict(awfy_measure.runtime_cases(suite, cwd, args.upstream))
    commands = cases[name]
    command, marker = commands["hirsel"]
    argv = [f"./{command[0].name}", *map(str, command[1:])]
    if argv != hirsel["argv"] or marker.decode() != hirsel["marker"]:
        raise ValueError("the peer commands do not match the Hirsel receipt's workload")
    status = 0
    for peer in args.peer:
        command, marker = commands[peer]
        row = {"argv": list(map(str, command)), "cwd": str(cwd), "marker": marker.decode(), "line_marker": True}
        status = max(status, run_cycle_pairs(f"{key} Hirsel/{peer}", row, hirsel, args, siblings, env, report, peer=True))
    return status


def run_cycle_pairs(label, base, candidate, args, siblings, env, report, *, peer=False):
    pairs = summary = None
    if report is not None:
        key = label
        repeat = 1
        while key in report["rows"]:
            repeat += 1
            key = f"{label} ({repeat})"
        pairs = report["rows"][key] = []
        summary = report["summary"][key] = {}
        base, candidate = (
            row if "image_sha256" in row else row | {"image_sha256": sha256(Path(row["cwd"]) / row["argv"][0])}
            for row in (base, candidate)
        )
    kept, attempted = cycle_samples(base, candidate, args, siblings, env, pairs)
    return report_cycle_pairs(label, kept, attempted, args.pairs,
                              filtered=args.sibling_busy_threshold is not None, peer=peer, summary=summary)


def multicore_peer_pairs(args, siblings, env, report):
    sys.path.insert(0, str(build.ROOT))
    from bench.multicore import run as game

    bins = args.baseline.resolve()
    keys = select_pair_rows(args.rows, {f"multicore/{name}": name for name in game.GAME_ROWS})
    env = env | {"LC_ALL": "C", "RAYON_NUM_THREADS": "1"}
    status = 0
    for key in keys:
        name = key.split("/", 1)[1]
        fasta = bins / "input.fasta"
        wanted = game.expected(name, fasta)

        def command(language):
            argv = game.sample_command(bins, name, language, 1, game.SIZES[name])
            return {"argv": list(map(str, argv)), "cwd": str(bins),
                    "stdin": game.sample_stdin(bins, name, language),
                    "verify": lambda output: game.matches(output, wanted)}

        hirsel = command("Hirsel")
        log = bins / f"hirsel-{name}.log"
        report = next((json.loads(line) for line in log.read_text().splitlines() if line.startswith("{")), {})
        native = report.get("native") or {}
        if not (report.get("accepted") and native.get("linked")
                and Path(native["executable"]).resolve() == Path(hirsel["argv"][0]).resolve()):
            raise ValueError(f"{key}: expected a linked Hirsel build")
        for peer in args.peer:
            status = max(status, run_cycle_pairs(f"{key} Hirsel/{peer}", command(peer.title()),
                                                hirsel, args, siblings, env, report, peer=True))
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


def pair_report(args, comparison, aa=None):
    return {"comparison": comparison, "cpu": args.cpu, "pairs": args.pairs,
            "aa": aa, "lock": str(args.lock.resolve()), "command": sys.orig_argv, "cwd": str(Path.cwd()),
            "rows": {}, "summary": {}, "failures": {}}


def pair_statistics(left, right):
    deltas = [change(a, b) for a, b in zip(left, right)]
    return {"base_median": median(left), "base_min": min(left), "base_max": max(left),
            "candidate_median": median(right), "candidate_min": min(right), "candidate_max": max(right),
            "median_percent": median(deltas), "min_percent": min(deltas), "max_percent": max(deltas)}


def aa_spread(key, pairs, baseline, cpu, count):
    if len(pairs) != count:
        raise ValueError(f"{key}: A/A control must have {count} pairs")
    changes = []
    for pair in pairs:
        sides = pair["base"], pair["candidate"]
        values = []
        for row in sides:
            values.append(cycle_count(key, row))
            if (row["affinity"] != [cpu] or candidate_identity(row) != candidate_identity(baseline)
                    or any(row[field] != baseline[field] for field in (*WORKLOAD_IDENTITY, "marker")
                           if field != "affinity")):
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
        counts = pair_statistics(left, right)
        center, low, high = (counts[field] for field in ("median_percent", "min_percent", "max_percent"))
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
        print(f"{key} {event}: {counts['base_median']:,} -> {counts['candidate_median']:,}, paired median {center:+.3f}% "
              f"[{low:+.3f}%, {high:+.3f}%] {verdict}{detail}", flush=True)
        summary[event] = counts | {"verdict": verdict}
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
    report = pair_report(args, "A/B" if comparing else "A/A", args.aa)
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
                    sides = (("base", base[key]), ("candidate", after[key]))
                    if index % 2 != 0:
                        sides = sides[::-1]
                    pair = {"order": []}
                    pairs.append(pair)
                    for side, prepared in sides:
                        pair["order"].append(side)
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
    base_identity, candidate = match_rows(args.row, left, right)
    affinity = set(left["affinity"])
    assigned = os.sched_getaffinity(0)
    if not affinity <= assigned:
        raise ValueError(f"run on the recorded affinity {sorted(affinity)}")
    out = args.out.resolve()
    if out.is_relative_to(build.ROOT):
        raise ValueError("use a new evidence directory outside the checkout")
    base_count = left["sample"]["instructions"]
    candidate_count = right["sample"]["instructions"]
    percent = Decimal(100) * (candidate_count - base_count) / base_count
    print(f"{args.row} PMU instructions:u {base_count:,} -> {candidate_count:,} ({percent:+.3f}%)", flush=True)
    for field, before_value, after_value in zip(CANDIDATE_HASHES, base_identity, candidate):
        if before_value != after_value:
            print(f"{field} {before_value} -> {after_value}")
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


def sibling_busy_threshold(value):
    if value == "none":
        return None
    limit = float(threshold(value))
    if not 0 < limit <= 100:
        raise argparse.ArgumentTypeError("sibling busy threshold must be above 0 and at most 100 percent")
    return limit


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
    comparison.add_argument("--aa", action="store_true", help="require identical candidate identities")
    cycles = commands.add_parser("cycle-pairs", help="qualify named runtime rows with interleaved counter pairs")
    cycles.add_argument("baseline", type=Path, help="base receipt directory or --multicore build directory")
    cycles.add_argument("candidate", nargs="?", type=Path,
                        help="candidate directory containing receipt.json or board.json")
    cycles.add_argument("--peer", nargs="+", choices=("rust", "goose"),
                        help="report Hirsel/peer ratios from one Hirsel build")
    cycles.add_argument("--upstream", type=lambda path: Path(path).resolve(),
                        help="pinned AWFY checkout for peer commands")
    cycles.add_argument("--multicore", action="store_true",
                        help="sample Benchmarks Game peers from a multicore build directory")
    cycles.add_argument("--rows", required=True, metavar="ROW,ROW", help="comma-separated runtime row names")
    cycles.add_argument("--cpu", type=int, required=True, help="measurement CPU")
    cycles.add_argument("--pairs", type=positive_int, default=60, metavar="N", help="kept pairs per row (default: 60)")
    cycles.add_argument("--max-attempts", type=positive_int, default=60, metavar="N",
                        help="attempted pairs per row (default: 60)")
    cycles.add_argument("--sibling-busy-threshold", type=sibling_busy_threshold, metavar="PERCENT",
                        help="each run must be strictly below this SMT sibling busy percentage (default: none)")
    cycles.add_argument("--lock", type=Path, required=True)
    cycles.add_argument("--out", type=Path, metavar="PATH",
                        help="paired evidence file (default: pairs.json beside the candidate or only receipt)")
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
