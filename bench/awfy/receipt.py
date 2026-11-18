#!/usr/bin/env python3
"""Sample checked release rows and compare their user-instruction receipts."""
import argparse
from contextlib import contextmanager
from decimal import Decimal, InvalidOperation
import fcntl
import hashlib
import json
import math
import os
from pathlib import Path
import re
import subprocess
import sys

import build
import measure
import profile_diff
from measurement_artifacts import SHIPPING, check

VERSION = 1
MIN_ACTIVE = 99
ARTIFACT_HASHES = ("compiler_sha256", "runtime_archive_sha256")
IDENTITY = ("suite", "mode", "certified", "flags", "argv", "affinity", *ARTIFACT_HASHES, "fixture_sha256")


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


def sha256(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def release_tools(artifacts):
    if (artifacts["compiler_build_profile"] != "release" or artifacts["runtime_archive_profile"] != SHIPPING
            or artifacts["diagnostic"]):
        raise ValueError("expected release compiler and shipping fat-LTO runtime")
    for role in ("compiler", "runtime"):
        check(Path(artifacts[role]), artifacts)
    return {key: artifacts[key] for key in ARTIFACT_HASHES}


def fixture_identity(manifest, runtime_fixture):
    inputs = read(manifest)
    # File names in a manifest can include the checkout and build directory.
    # Compare the complete ordered package graph and contents at those paths.
    for package in inputs["packages"]:
        for source in package["sources"]:
            source["file"] = sha256(Path(source["file"]))
    digest = sha256(runtime_fixture)
    return {
        "fixture_sha256": hashlib.sha256(json.dumps(
            [inputs, digest], sort_keys=True, separators=(",", ":")
        ).encode()).hexdigest(),
        "fixture_files": {str(runtime_fixture): digest},
    }


def runtime_fixture(suite, directory, upstream):
    if suite == "arena":
        return fixture_identity(directory / "inputs.json", directory / "one-iteration.json")
    return fixture_identity(directory / "hirsel-inputs.json", upstream / "rebench.conf")


def native_report(path):
    result = read(path)
    if result.get("accepted") is not True or result.get("stopped") or not result["native"]["linked"]:
        raise ValueError(f"build not accepted and linked: {path}")
    return result["native"]


def runtime_case(suite, name, directory, command, marker, fixture, tools, flags=()):
    binary = command[0]
    native = native_report(directory / f"{binary.name}-build.json")
    if Path(native["executable"]).resolve() != binary.resolve():
        raise ValueError(f"{binary}: native build receipt names a different executable")
    certified = native.get("step_charging_elided", False)
    if suite == "arena":
        module, mib = binary.name.rsplit("-", 1)
        entry = f"langarena@1/{module}/main"
        grants = ["--grant", "Tasks", "--dir", ".", "--limit", str(int(mib) << 20)]
    else:
        entry = f"awfy@1/{binary.name.removeprefix('hirsel-')}/main"
        grants = []
    build_flags = ["--backend", "llvm", "--release", "--entry", entry, "--grant", "Stdout",
                   "--debug-lines", "on" if native.get("debug_lines", False) else "off", *grants]
    return {
        "key": f"{suite}/{name}",
        "suite": suite,
        "mode": "certified" if certified else "bounded",
        "certified": certified,
        "flags": [*build_flags, *flags],
        "argv": [f"./{binary.name}", *map(str, command[1:])],
        "cwd": str(directory),
        "image_sha256": sha256(binary),
        "marker": marker.decode(),
        **fixture,
        **tools,
    }


def counter_ready(sample, event):
    active = sample["pmu_active_percent"].get(event, 0)
    return MIN_ACTIVE <= active <= 100


def observe(row, target, env, *, cycles=False):
    row["affinity"] = sorted(os.sched_getaffinity(0))
    sample = measure.core_measure.sample(row["argv"], target, env=env, cwd=row["cwd"], cycles=cycles)
    sample["verified"] = (
        sample["status"] in ("ok", "missing-counter")
        and re.fullmatch(row["marker"].encode(), (target / "stdout").read_bytes()) is not None
    )
    if sample["status"] == "ok":
        if not sample["verified"]:
            sample["status"] = "wrong-result"
        elif not counter_ready(sample, "instructions:u") or (cycles and not counter_ready(sample, "cycles:u")):
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
            rows[key] = runtime_case(suite, name, directory, command, marker, fixtures[suite], tools, args.flag)
        except (OSError, ValueError, KeyError) as error:
            receipt["failures"][key] = str(error)
    env = build.capped_env(out)
    with measurement_slot(args.lock):
        os.sched_setaffinity(0, {args.cpu})
        for key, row in rows.items():
            try:
                observed = observe(row, out / key.replace("/", "-", 1), env)
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


def rows_from(path):
    receipt = read(path)
    if "instruction_receipt" in receipt:
        if receipt["complete"] is not True:
            raise ValueError(f"{path}: incomplete board")
        receipt = receipt["instruction_receipt"]
    if receipt["version"] != VERSION:
        raise ValueError(f"{path}: unsupported instruction receipt")
    expected = receipt["expected_rows"]
    rows = {row["key"]: row for row in receipt["rows"]}
    if (not expected or len(set(expected)) != len(expected) or len(rows) != len(receipt["rows"])
            or rows.keys() != set(expected) or receipt["failures"]):
        raise ValueError(f"{path}: incomplete or duplicate rows "
                         f"(missing: {sorted(set(expected) - rows.keys())}, "
                         f"unexpected: {sorted(rows.keys() - set(expected))}, "
                         f"failed: {sorted(receipt['failures'])})")
    for key, row in rows.items():
        for field in IDENTITY:
            if field not in row or row[field] is None or (field != "certified" and not row[field]):
                raise ValueError(f"{path}: {key} missing {field}")
        suite, mode, certified = row["suite"], row["mode"], row["certified"]
        if (suite not in measure.SUITES or not key.startswith(suite + "/")
                or type(certified) is not bool
                or (mode not in ("cold", "edit") if suite == "compile"
                    else mode != ("certified" if certified else "bounded"))
                or any(not re.fullmatch("[a-f0-9]{64}", row[field])
                       for field in (*ARTIFACT_HASHES, "fixture_sha256"))
                or not all(isinstance(row[field], list)
                           and all(isinstance(value, str) and value for value in row[field])
                           for field in ("argv", "flags"))
                or not isinstance(row["affinity"], list)
                or any(type(cpu) is not int or cpu < 0 for cpu in row["affinity"])):
            raise ValueError(f"{path}: {key} invalid identity")
        sample = row["sample"]
        if (sample["status"] != "ok" or sample["verified"] is not True
                or type(sample["instructions"]) is not int or sample["instructions"] <= 0
                or not counter_ready(sample, "instructions:u")
                or not row["stdout"] or not re.fullmatch(row["marker"], row["stdout"])):
            raise ValueError(f"{path}: {key} lacks a verified instruction sample")
    return rows


def compare(before_path, after_path, threshold):
    before, after = rows_from(before_path), rows_from(after_path)
    if before.keys() != after.keys():
        raise ValueError(f"row sets differ (missing: {sorted(before.keys() - after.keys())}, "
                         f"unexpected: {sorted(after.keys() - before.keys())})")
    differences = {}
    for key, left in before.items():
        right = after[key]
        fields = [field for field in (*IDENTITY, "marker", "stdout") if left[field] != right[field]]
        if fields:
            differences[key] = fields
    if differences:
        raise ValueError("identity differs: " + "; ".join(
            f"{key}: {', '.join(fields)}" for key, fields in differences.items()))
    failed = False
    ratios = []
    for key, left in sorted(before.items()):
        baseline = left["sample"]["instructions"]
        candidate = after[key]["sample"]["instructions"]
        percent = Decimal(100) * (candidate - baseline) / baseline
        breach = percent > threshold
        failed |= breach
        ratios.append(candidate / baseline)
        print(f"{key} {baseline:,} -> {candidate:,} ({percent:+.3f}%){' FAIL' if breach else ''}")
    geomean = math.expm1(math.fsum(map(math.log, ratios)) / len(ratios)) * 100
    print(f"{len(ratios)} rows, geomean {geomean:+.3f}%, threshold +{threshold}%")
    return int(failed)


def profile(args):
    before, after = rows_from(args.baseline), rows_from(args.candidate)
    left, right = before[args.row], after[args.row]
    if left["suite"] == "compile":
        raise ValueError("profile-diff requires a runtime row")
    fields = [field for field in (*IDENTITY, "marker", "stdout")
              if field not in ARTIFACT_HASHES and left[field] != right[field]]
    if fields:
        raise ValueError(f"{args.row}: identity differs: {', '.join(fields)}")
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
    for field in ARTIFACT_HASHES:
        if left[field] != right[field]:
            print(f"{field} {left[field]} -> {right[field]}")
    sides = []
    for label, row in (("baseline", left), ("candidate", right)):
        if not row["argv"][0].startswith("./"):
            raise ValueError(f"{label}: expected a runtime executable relative to cwd")
        path = (Path(row["cwd"]) / row["argv"][0]).resolve()
        if sha256(path) != row["image_sha256"]:
            raise ValueError(f"{label}: executable no longer matches the measured image")
        for fixture, digest in row["fixture_files"].items():
            if sha256(Path(fixture)) != digest:
                raise ValueError(f"{label}: measured fixture has changed: {fixture}")
        sides.append((out / label, row, path))
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
    sampling.add_argument("--only", nargs="+", metavar="SUITE/ROW")
    sampling.add_argument("--flag", action="append", default=[], help="additional build flag, e.g. --flag=--example")
    comparison = commands.add_parser("compare", help="fail on differing identity, missing rows or a threshold breach")
    comparison.add_argument("baseline", type=Path)
    comparison.add_argument("candidate", type=Path)
    comparison.add_argument("--threshold", type=threshold, default=Decimal("0.5"), metavar="PERCENT")
    profiling = commands.add_parser("profile-diff", help="attribute one matched runtime row with Callgrind")
    profiling.add_argument("row", metavar="SUITE/ROW")
    profiling.add_argument("baseline", type=Path)
    profiling.add_argument("candidate", type=Path)
    profiling.add_argument("out", type=Path)
    profiling.add_argument("--lock", type=Path, required=True)
    args = parser.parse_args()
    try:
        if args.action == "sample":
            return sample(args)
        if args.action == "profile-diff":
            return profile(args)
        return compare(args.baseline, args.candidate, args.threshold)
    except (KeyError, OSError, ValueError, TypeError, AttributeError, re.error, subprocess.SubprocessError) as error:
        print(f"{args.action}: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
