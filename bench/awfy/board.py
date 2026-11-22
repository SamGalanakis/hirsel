#!/usr/bin/env python3
"""Build and run the matched Hirsel goal board. Keep outputs outside the checkout."""
import argparse
from contextlib import contextmanager, redirect_stderr, redirect_stdout
from decimal import Decimal
from io import StringIO
import importlib.util
import json
import math
import os
from pathlib import Path
import re
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time

import arena
import build
import measure
import receipt
from receipt import measurement_slot, write as write_json
from bench_cpus import add_cpu_sets, sibling_cpus, smt_activity, validate_cpu_sets

ROOT = build.ROOT
CAP = ROOT / "tools/capped"
LOCK = Path("/run/user") / str(os.getuid()) / "hirsel-board.lock"
PINS = {
    "awfy": "f35aed0a61171594804a44efe9d75f1f35360700",
    "resid": "0ab89f373829776d3ea285bfb25f3f1505d7be35",
    "goose": "d5ae5bbda990923fa4129f969c7f853c20d4d1e6",
    "langarena": "3c9f84ddaeec834c0631747cb789e8cfd602f993",
}
COUNTERS = ("instructions", "cycles")
STARTUP = {
    "awfy": ("Bounce", "AWFY original (Bounce)"),
    "pool": ("List", "AWFY pool/index (List)"),
    "arena": ("CLBG::Nbody", "LangArena (CLBG::Nbody)"),
}
EXPECTED = {**measure.EXPECTED, "Startup": tuple(label for _, label in STARTUP.values())}
INSTRUCTION_LIMIT = Decimal(2)
SIZE_LIMIT = 2048
SIZE_ROW = "AWFY original-14 harness"
CHECKBOX_METRICS = {
    "converge": "toggles_per_second",
    "memory-subscribed": "server_rss_bytes",
    "memory-unsubscribed": "server_rss_bytes",
}
CHECKBOX_CLIENTS = 64
NOTES = (
    "Hirsel checks bounds and overflow, admits bytes and charges Steps only where a step bound can be current; "
    "Rust and Goose check bounds only.",
    "Runtime counters count user-space CPU work. Seconds use perf duration_time around the same "
    "whole-process command on each side, including startup, setup, warmups, measured iterations, "
    "verification and exit. The LangArena runtime fixture is the pinned upstream run.js, copied byte "
    "for byte, with upstream checksums. Each AWFY row verifies the reference result internally. "
    "The startup rows report a representative whole process with one iteration and no warmup. "
    "LangArena startup uses the one-iteration Rust oracle. The ports use different libraries and layouts.",
    "The nightly instruction gate flags Hirsel increases above 2%. All CPU counters must be fully active. "
    "The size row measures each original-14 harness after GNU strip, reports growth, and flags Hirsel "
    "increases of 2,048 bytes or more. The compile rows keep dependencies built and use one Cargo "
    "codegen unit with release incremental enabled. The warm edit rebuilds code and changes "
    "the Spectralnorm checksum from 1051192944 to 1051192945.",
    "Checkbox runs use the same checked wire driver and durable workload on both servers. "
    "Throughput is toggles/s including connection and convergence time; higher is better. "
    "Throughput is reported as a trend. The board flags RSS per live session rising by more "
    "than 10%. Goose has no compile or checkbox peer here.",
)


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def git(directory, *args):
    return subprocess.check_output(["git", "-C", directory, *args], text=True).strip()


def run(command, *, env, cwd=ROOT, **kwargs):
    return subprocess.run([CAP, *command], cwd=cwd, env=env, check=True, **kwargs)


def cargo_scope(env):
    """Pass an optional NAME=VALUE setting to Cargo inside tools/capped."""
    setting = env.get("HIRSEL_BOARD_CARGO_ENV")
    if not setting:
        return env
    name, separator, value = setting.partition("=")
    if not name or not separator:
        raise ValueError("HIRSEL_BOARD_CARGO_ENV must be NAME=VALUE")
    return env | {name: value}


def release_flags(artifacts):
    return ["--backend", "llvm", "--release", "--runtime", artifacts["runtime"],
            "--cc", build.llvm_path() / "bin/clang", "--json"]


def accepted(path):
    result = json.loads(path.read_text())
    if not result.get("accepted") or result.get("stopped"):
        raise ValueError(f"build not accepted: {path}")


def ratios(samples, fields):
    numerator = samples.get("hirsel", {})
    result = {}
    for side in ("rust", "goose"):
        denominator = samples.get(side, {})
        result[side] = {}
        if numerator.get("status") != "ok" or denominator.get("status") != "ok":
            continue
        for field in fields:
            left, right = float(numerator[field]), float(denominator[field])
            if math.isfinite(left) and math.isfinite(right) and left > 0 and right > 0:
                result[side][field] = left / right
    return result


def rows_by_label(report):
    rows = {}
    for row in report["rows"]:
        label = (row["goal"], row["row"])
        if label in rows:
            raise ValueError(f"duplicate board row {'/'.join(label)}")
        rows[label] = row
    return rows


def inactive_counters(report):
    suites = {title: suite for suite, title in measure.SUITES.items()}
    blocked = {}
    for row in report["rows"]:
        goal, name = row["goal"], row["row"]
        if goal not in suites:
            continue
        for side, sample in row["samples"].items():
            if sample["status"] == "inactive-counter" or (sample["status"] == "ok" and not all(
                receipt.counter_ready(sample, f"{counter}:u") for counter in COUNTERS if counter in row["metrics"]
            )):
                key = f"{suites[goal]}/{name}"
                blocked.setdefault(key, []).append(f"{goal}/{name}/{side}: inactive-counter (must be fully active)")
    return {key: ", ".join(reasons) for key, reasons in blocked.items()}


def size_changes(reference, candidate):
    previous_measurement = reference.get("size_measurement", {})
    current_measurement = candidate.get("size_measurement", {})
    for field in ("strip_version", "strip_flags", "verified_workloads", "pins"):
        previous = previous_measurement.get(field)
        if not previous or previous != current_measurement.get(field):
            raise ValueError(f"size identity differs: {field}")
    left, right = (rows_by_label(report)["Size", SIZE_ROW] for report in (reference, candidate))
    changes = []
    for side in measure.SIDES:
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


def checkbox_value(sample, metric):
    value = Decimal(str(sample[metric]))
    if sample["status"] != "ok" or not value.is_finite() or value <= 0:
        raise ValueError(f"invalid {metric} sample")
    if metric == "server_rss_bytes":
        if sample["connections"] != CHECKBOX_CLIENTS:
            raise ValueError(f"RSS must cover {CHECKBOX_CLIENTS} live sessions")
        value /= CHECKBOX_CLIENTS
    return value


def checkbox_changes(reference, candidate, metric):
    for row in (reference, candidate):
        if row["status"] != "ok" or tuple(row["metrics"]) != (metric,):
            raise ValueError("checkbox row has a different metric or an incomplete sample")
    for side in ("hirsel", "rust"):
        before, after = (checkbox_value(row["samples"][side], metric) for row in (reference, candidate))
        percent = Decimal(100) * (after - before) / before
        yield side, before, after, percent


def instruction_keys():
    return [f"{suite}/{name}" for suite, goal in measure.SUITES.items() for name in EXPECTED[goal]]


class Board:
    def __init__(self, out, commit):
        self.out = out
        self.report = {"commit": commit, "pins": PINS, "notes": NOTES, "rows": [], "failures": {},
                       "instruction_receipt": receipt.make_receipt(instruction_keys())}

    def save(self):
        observed = {(row["goal"], row["row"]): row for row in self.report["rows"]}
        expected = [
            {"goal": goal, "row": name, "status": (
                observed[goal, name]["status"] if (goal, name) in observed else "missing")}
            for goal, names in EXPECTED.items() for name in names
        ]
        self.report["expected_rows"] = expected
        self.report["complete"] = (
            all(row["status"] == "ok" for row in expected) and not self.report["failures"]
        )
        write_json(self.out / "board.json", self.report)
        lines = ["# Hirsel goal board", "",
                 "Complete" if self.report["complete"] else "Incomplete", "", *NOTES, "",
                 "| Goal | Row | Metric | H/R | H/G | Commit |",
                 "| --- | --- | --- | ---: | ---: | --- |"]
        for row in self.report["rows"]:
            for metric in row["metrics"]:
                cells = []
                for side in ("rust", "goose"):
                    value = row["ratios"][side].get(metric)
                    cells.append(f"{value:.4f}×" if value is not None else "n/a")
                lines.append("| " + " | ".join((row["goal"], row["row"], metric, *cells,
                                                  row["commit"][:8])) + " |")
        notes = [f"- {row['goal']}/{row['row']}. {row['notes']}"
                 for row in self.report["rows"] if row.get("notes")]
        if notes:
            lines += ["", "## Runtime notes", "", *notes]
        if self.report["failures"]:
            lines += ["", "## Incomplete samples", ""]
            lines += [f"- {label}: {reason}" for label, reason in self.report["failures"].items()]
        missing = [f"{row['goal']}/{row['row']}" for row in expected if row["status"] == "missing"]
        if missing:
            lines += ["", "## Missing rows", "", *[f"- {label}" for label in missing]]
        if "exit_status" in self.report:
            lines += ["", f"Board exit status {self.report['exit_status']}"]
        comparison = self.report.get("instruction_comparison")
        if comparison:
            lines += ["", "## Nightly instruction comparison", "", comparison["status"], "",
                      *comparison["details"]]
        for gate in ("size", "checkbox"):
            if comparison := self.report.get(f"{gate}_comparison"):
                lines += ["", f"## Nightly {gate} comparison", "", *comparison]
        (self.out / "board.md").write_text("\n".join(lines) + "\n")

    def finish(self, baseline=None):
        self.save()
        blocked = inactive_counters(self.report)
        if not self.report["complete"] and not blocked:
            status = 1
        elif baseline is not None:
            status = self.compare_previous(baseline, blocked)
        elif blocked:
            status = receipt.REFUSED
            for reason in blocked.values():
                print(reason, file=sys.stderr)
        else:
            try:
                validate_baseline(self.out / "board.json", self.report)
                self.report["instruction_comparison"] = {"status": "No baseline supplied", "details": []}
                status = 0
            except (*receipt.INVALID_RECEIPT, ArithmeticError) as error:
                status = receipt.REFUSED
                details = [f"REFUSED ({error})"]
                self.report["instruction_comparison"] = {"status": "Refused", "details": details}
                print(details[0], file=sys.stderr)
        self.report["exit_status"] = status
        self.save()
        print(self.out / "board.md", flush=True)
        return status

    def compare_previous(self, baseline, blocked):
        output = StringIO()
        reference = {}
        try:
            reference = receipt.read(baseline)
            if reference["complete"] is not True:
                reference = {}
                raise ValueError("incomplete board baseline")
            blocked = inactive_counters(reference) | blocked
            with redirect_stdout(output), redirect_stderr(output):
                instructions = receipt.compare(
                    baseline, self.out / "board.json", INSTRUCTION_LIMIT,
                    expected=self.report["instruction_receipt"]["expected_rows"], blocked=blocked)
        except receipt.INVALID_RECEIPT as error:
            instructions = receipt.REFUSED
            print(f"REFUSED ({error})", file=output)
        status = ("Within +2% on every row", "Instruction regression", "Refused")[instructions]
        details = output.getvalue().splitlines()
        self.report["instruction_comparison"] = {"status": status, "details": details}

        size, checkbox = [], []
        refused, failed = instructions == receipt.REFUSED, instructions == 1
        try:
            for side, before, after in size_changes(reference, self.report):
                delta = after - before
                breach = side == "hirsel" and delta >= SIZE_LIMIT
                failed |= breach
                line = f"{side} {before:,} -> {after:,} B ({delta:+,} B){' FAIL' if breach else ''}"
                size.append(line)
                if delta > 0:
                    print(line)
        except receipt.INVALID_RECEIPT as error:
            refused = True
            size.append(f"REFUSED ({error})")

        try:
            previous = rows_by_label(reference)
            current = rows_by_label(self.report)
            for mode, metric in CHECKBOX_METRICS.items():
                label = ("Checkbox", f"{mode}, {CHECKBOX_CLIENTS} clients")
                try:
                    rss = metric == "server_rss_bytes"
                    unit = "B/session" if rss else "toggles/s"
                    for side, before, after, percent in checkbox_changes(previous[label], current[label], metric):
                        breach = rss and percent > 10
                        failed |= breach
                        checkbox.append(f"{label[1]}/{side} {before:,.2f} -> {after:,.2f} {unit} "
                                        f"({percent:+.3f}%){' FAIL' if breach else ''}")
                except (*receipt.INVALID_RECEIPT, ArithmeticError) as error:
                    refused = True
                    checkbox.append(f"{label[1]} REFUSED ({error})")
        except receipt.INVALID_RECEIPT as error:
            refused = True
            checkbox.append(f"REFUSED ({error})")

        self.report["size_comparison"], self.report["checkbox_comparison"] = size, checkbox
        for line in (*details, *size, *checkbox):
            if "REFUSED" in line:
                print(line, file=sys.stderr)
        if refused:
            return receipt.REFUSED
        return 3 if failed else 0

    def row(self, goal, name, samples, fields, *, notes=None):
        label = f"{goal}/{name}"
        if name not in EXPECTED.get(goal, ()) or any(
            (row["goal"], row["row"]) == (goal, name) for row in self.report["rows"]
        ):
            raise ValueError(f"unexpected or repeated row {label}")
        sides = ("hirsel", "rust") if goal in ("Compile", "Checkbox") else measure.SIDES
        complete = True
        for side in sides:
            sample = samples.get(side, {})
            status = sample.get("status", "missing")
            if status == "ok" and not all(
                field in sample and math.isfinite(float(sample[field])) and float(sample[field]) > 0
                for field in fields
            ):
                status = "missing-or-invalid-metric"
            if status == "ok" and not all(
                receipt.counter_ready(sample, f"{counter}:u") for counter in COUNTERS if counter in fields
            ):
                status = "inactive-counter"
            if status != "ok":
                if side in samples:
                    sample["status"] = status
                self.report["failures"][f"{label}/{side}"] = status
                complete = False
        self.report["rows"].append({"goal": goal, "row": name,
                                    "status": "ok" if complete else "failed",
                                    "commit": self.report["commit"], "samples": samples,
                                    "metrics": fields, "ratios": ratios(samples, fields), "notes": notes})
        self.save()

    def attempt(self, label, operation, *args):
        try:
            return operation(*args)
        except (ValueError, OSError, RuntimeError, KeyError, subprocess.SubprocessError) as error:
            print(f"{label}: {error}", file=sys.stderr, flush=True)
            self.report["failures"][label] = str(error)
            self.save()


def sample_runtime(row, commands, directory, samples_dir, env, siblings, index, label):
    samples = {}
    order = ("rust", "hirsel", "goose") if index % 2 == 0 else ("goose", "hirsel", "rust")
    for side in order:
        command, marker = commands[side]
        target = samples_dir.with_name(f"{samples_dir.name}-{side}")
        if side == "hirsel":
            sample = receipt.observe(row, target, env, siblings, label=f"{label}/{side}")
            output = row["stdout"].encode().splitlines(keepends=True)
        else:
            with smt_activity(siblings, f"{label}/{side}") as busy:
                sample = measure.core_measure.sample(command, target, env=env, cwd=directory, pmu_times=True)
            sample["smt_busy_percent"] = busy
            output = (target / "stdout").read_bytes().splitlines(keepends=True)
            sample["verified"] = sample["status"] in ("ok", "missing-counter") and any(
                re.fullmatch(marker, line) for line in output)
            if sample["status"] == "ok" and not sample["verified"]:
                sample["status"] = "wrong-result"
        if row["suite"] == "arena" and not sample["verified"]:
            name = row["argv"][-1].encode()
            if any(line.startswith(name + b": ERR[") or
                   (line.startswith(name + b" checksum=") and line.endswith(b" status=ERR\n"))
                   for line in output):
                sample["status"] = "wrong-checksum"
        samples[side] = sample
    return samples


def matmul_notes(name):
    widths = {"Matmul::T4": 4, "Matmul::T8": 8, "Matmul::T16": 16}
    if name in widths:
        width = widths[name]
        return (f"Hirsel submits {width} row blocks to one worker for the process run. "
                f"Rust creates and drops a {width}-worker Rayon pool each iteration. "
                f"Goose starts {width} workers at construction and joins them at checksum time. "
                "All workers share the measurement CPU.")


def runtime_rows(board, suite, directory, upstream, tools, *, config=None, startup=False):
    goal = measure.SUITES[suite]
    rows = (measure.arena_rows(directory, config) if config is not None
            else measure.runtime_cases(suite, directory, upstream))
    fixture = receipt.runtime_fixture(suite, directory, upstream, config=config)
    startup_name, startup_label = STARTUP[suite]
    samples_dir = board.out / "runtime" / ("Startup" if startup else goal)
    samples_dir.mkdir(parents=True)
    siblings = sibling_cpus(os.sched_getaffinity(0))
    env = build.capped_env(board.out)
    verified = []
    for index, (name, commands) in enumerate(rows):
        if startup and name != startup_name:
            continue
        row = receipt.runtime_case(suite, name, directory, *commands["hirsel"], fixture, tools)
        title, label = ("Startup", startup_label) if startup else (goal, name)
        target = samples_dir / name.replace("::", "-")
        samples = sample_runtime(row, commands, directory, target, env, siblings, index, f"{title}/{label}")
        if not startup:
            board.report["instruction_receipt"]["rows"].append(row)
        if all(sample["verified"] for sample in samples.values()):
            verified.append(name)
        metrics = ("seconds",) if startup else ("seconds", "instructions") if suite == "arena" else ("instructions",)
        board.row(title, label, samples, metrics, notes=matmul_notes(name))
        if suite != "arena" and name == startup_name:
            board.row("Startup", startup_label,
                      {side: sample.copy() for side, sample in samples.items()}, ("seconds",))
    return verified


def awfy_rows(board, out, upstream, tools):
    originals = [(name, commands) for name, commands in measure.rows(out, False, upstream) if name != "Hello"]
    if sorted(dict(originals)) != sorted(EXPECTED["AWFY original"]) or len(originals) != 14:
        raise ValueError("the original board requires 14 distinct AWFY rows")
    verified = runtime_rows(board, "awfy", out, upstream, tools)
    if len(verified) == len(originals):
        result = measure.verified_sizes(out, verified)
        board.report["size_measurement"] = {
            "verified_workloads": sorted(verified), "strip_version": result["strip_version"],
            "strip_flags": result["strip_flags"],
            "pins": {peer: PINS[peer] for peer in ("awfy", "resid", "goose")},
        }
        board.row("Size", SIZE_ROW,
                  {side: {"status": "ok", **image} for side, image in result["images"].items()}, ("bytes",))
    else:
        board.report["failures"]["Size"] = "not all original harness results verified"
        board.save()
    pools = list(measure.matched_pool_rows(out, upstream))
    if sorted(dict(pools)) != sorted(EXPECTED["AWFY pool/index"]) or len(pools) != 8:
        raise ValueError("the pool board requires eight distinct AWFY rows")
    runtime_rows(board, "pool", out, upstream, tools)


def cargo(*args):
    return ["cargo", "+1.99.0", *args]


def fetch(rust, env):
    """Download the locked crates the Cargo cache lacks; the measured builds stay offline."""
    run(cargo("fetch", "--locked"), cwd=rust, env=cargo_scope(env))


def compile_profile(target):
    return ["--offline", "--locked", "--release", "--target-dir", target,
            "--config", "profile.release.incremental=true",
            "--config", "profile.release.codegen-units=1",
            "--config", "profile.release.overflow-checks=true"]


def compile_commands(out):
    target = out / "target"
    executable = out / "hirsel"
    hirsel = ["./pinned-compiler", "build", *release_flags({"runtime": "./pinned-runtime.a"}),
              "--inputs", "inputs.json",
              "--core-product", "packages", "--store", "store", "--entry", "langarena@1/suite/main",
              "--dir", ".", "--grant", "Stdout", "--grant", "Tasks", "--limit", str(256 << 20), "-o", "hirsel"]
    return (out / "rust", out / "clbg.hirsel", hirsel,
            cargo("build", "-j1", "--message-format=json", *compile_profile(target)),
            executable, target / "release/benchmarks")


def prepare_compile(out, artifacts, env):
    out.mkdir()
    rust = out / "rust"
    source = ROOT / "core/langarena/rust"
    rust.mkdir()
    for name in ("Cargo.toml", "Cargo.lock"):
        shutil.copyfile(source / name, rust / name)
    shutil.copytree(source / "src", rust / "src")
    target = out / "target"
    fetch(rust, env)
    run(cargo("build", "-j2", *compile_profile(target)), cwd=rust, env=cargo_scope(env))
    run(cargo("clean", "--release", "--target-dir", target, "-p", "benchmarks"), cwd=rust, env=cargo_scope(env))

    prepare_hirsel_compile(out, artifacts, env)
    return compile_commands(out)


def prepare_hirsel_compile(out, artifacts, env):
    suite = load("arena_suite", ROOT / "core/langarena/run.py")
    packages = out / "packages"
    run([artifacts["compiler"], "std-product", "--store", packages, "--runtime", artifacts["runtime"]], env=env)
    helper = out / "helper.hirsel"
    shutil.copyfile(ROOT / "core/langarena/helper.hirsel", helper)
    manifest = suite.inputs(ROOT, "helper", helper)
    clbg = out / "clbg.hirsel"
    shutil.copyfile(ROOT / "core/langarena/clbg.hirsel", clbg)
    for source in manifest["packages"][-1]["sources"]:
        if source["modules"] == ["clbg"]:
            source["file"] = str(clbg)
    suite_package = manifest["packages"][-1]
    suite_package["sources"] = [entry for entry in suite_package["sources"] if len(entry["modules"]) == 1]
    source = out / "suite.hirsel"
    source.write_text(suite.suite_source())
    manifest["packages"][-1]["sources"].append({"file": str(source), "modules": ["suite"], "signatures": False})
    write_json(out / "inputs.json", manifest)
    (out / "pinned-compiler").symlink_to(artifacts["compiler"])
    (out / "pinned-runtime.a").symlink_to(artifacts["runtime"])


def edit_checksum(path, old, new):
    source = path.read_text()
    if source.count(old) != 1:
        raise ValueError(f"{path}: expected one Spectralnorm checksum expression")
    path.write_text(source.replace(old, new))


def compile_rows(board, out, prepared, fixture, tools):
    rust, clbg, hirsel, cargo_build, exe, rust_exe = prepared
    env = build.capped_env(out)
    config = out / "fixture.json"
    shutil.copyfile(fixture, config)
    tasks = receipt.read(config)
    name, expected = "CLBG::Spectralnorm", 1051192944
    task = next(row for row in tasks if row["name"] == name)
    if task["checksum"] != expected:
        raise ValueError(f"{name}: expected the pinned cold checksum {expected}")
    siblings = sibling_cpus(os.sched_getaffinity(0))
    for mode in ("cold", "edit"):
        if mode == "edit":
            edit_checksum(clbg, "helper.checksum_f64(float.sqrt(vbv / vv))",
                          "Ok(int.wrapping_add(helper.checksum_f64(float.sqrt(vbv / vv))?, 1u32))")
            edit_checksum(rust / "src/benchmarks/spectralnorm.rs", "helper::checksum_f64(result_value)",
                          "helper::checksum_f64(result_value).wrapping_add(1)")
            expected += 1
            task["checksum"] = expected
            config = out / "fixture-edit.json"
            write_json(config, tasks)
        samples = {}
        label = f"LangArena {mode}"
        fixture_id = receipt.fixture_identity(out / "inputs.json", config)
        sample_env = cargo_scope(env)
        provenance = build.native_provenance(hirsel, out / "inputs.json", cwd=out)
        commands = [("hirsel", hirsel, out, exe), ("rust", cargo_build, rust, rust_exe)]
        if mode == "edit":
            commands.reverse()
        for side, command, cwd, executable in commands:
            target = out / f"{mode}-{side}"
            with smt_activity(siblings, f"{label}/{side}") as busy:
                sample = measure.core_measure.sample(command, target, env=sample_env, cycles=True,
                                                     cwd=cwd, pmu_times=True)
            sample["smt_busy_percent"] = busy
            samples[side] = sample
            if sample["status"] not in ("ok", "missing-counter"):
                continue
            if side == "hirsel":
                native_path = target / "stdout"
                accepted(native_path)
                report = build.write_native_report(executable, native_path.read_bytes(), provenance,
                                                   path=target / "build.json")
                record = receipt.build_record(report)
                sample["compiled_parts"] = report["native"]["compiled"]
                # prepare_hirsel_compile prebuilt the release std into the
                # std product, so a build that lowers std itself missed it.
                sample["std_lowered"] = report["native"]["reach"]["std_lowered"]
                rebuilt = sample["compiled_parts"] > 0
            else:
                messages = (json.loads(line) for line in (target / "stdout").read_text().splitlines())
                rebuilt = any(message["reason"] == "compiler-artifact" and message["target"]["name"] == "benchmarks"
                              and message["executable"] and message["fresh"] is False for message in messages)
                sample["rebuilt_crate"] = bool(rebuilt)
            verify = subprocess.run([CAP, executable, config.name, name], env=env, cwd=out,
                                    capture_output=True, text=True)
            (target / "verification").write_text(verify.stdout + verify.stderr)
            marker = re.escape(name) + (f" checksum={expected} expected={expected} status=OK" if side == "hirsel"
                                       else r": OK in [0-9.]+s")
            sample["verified"] = verify.returncode == 0 and any(
                re.fullmatch(marker, line) for line in verify.stdout.splitlines())
            if not sample["verified"]:
                sample["status"] = "wrong-result"
            elif sample.get("std_lowered"):
                sample["status"] = "std-lowered"
            elif sample["status"] == "ok" and not sample["seconds"]:
                sample["status"] = "missing-duration"
            elif mode == "edit" and not rebuilt:
                sample["status"] = "no-parts-compiled" if side == "hirsel" else "crate-not-rebuilt"
            if side == "hirsel":
                board.report["instruction_receipt"]["rows"].append({
                    "key": f"compile/{label}", "suite": "compile", "mode": mode,
                    "flags": provenance["flags"],
                    "argv": list(map(str, command)),
                    "build_record": record, "image_sha256": record["image_sha256"],
                    "affinity": sorted(os.sched_getaffinity(0)), "marker": marker + "\n",
                    "stdout": verify.stdout, "sample": sample, **fixture_id, **tools,
                })
        board.row("Compile", label, samples, ("seconds", *COUNTERS))
        if mode == "cold" and not all(sample.get("verified") for sample in samples.values()):
            raise ValueError("cannot measure a warm edit without two verified cold builds")


def prepare_checkbox(out, artifacts, env, *, wire_tools=None):
    checkbox = load("checkbox_run", ROOT / "core/programs/checkbox/run.py")
    out.mkdir()
    web = out / "web"
    web.mkdir()
    with socket.socket() as spare:
        spare.bind(("127.0.0.1", 0))
        port = spare.getsockname()[1]
    if port == 7700:
        raise ValueError("refusing the public checkbox port")
    address = f"127.0.0.1:{port}"
    with (out / "build.json").open("wb") as log:
        executable = checkbox.build(artifacts["compiler"], out, web, address, prefix=(CAP,),
                                    flags=release_flags(artifacts), env=env, stdout=log)
    run([artifacts["compiler"], "sdk", "ts", executable, "--out", web,
         "--n-place", checkbox.N_PLACE, "--runtime", artifacts["runtime"],
         "--cc", build.llvm_path() / "bin/clang"], env=env)
    wants = out / "wants.bin"
    run(["node", "--input-type=module", "-e", '''
import {writeFile} from "node:fs/promises";
import {pathToFileURL} from "node:url";
const base = pathToFileURL(process.argv[1] + "/");
const {Frame} = await import(new URL("frame.js", base));
const {want} = await import(new URL("entries.js", base));
await writeFile(process.argv[2], Frame.encode({tag:"Hello", values:[{tag:"None"}, want]}));
''', web, wants], env=env)
    if wire_tools is None:
        target = out / "wire-target"
        run(cargo("build", "--release", "--locked", "-j2", "--target-dir", target),
            cwd=ROOT / "bench/wire", env=cargo_scope(env))
        wire_tools = target / "release"
    return checkbox, executable, address, wants, wire_tools


def owns_listener(pid, address):
    process = Path(f"/proc/{pid}")
    port = f"{int(address.rsplit(':', 1)[1]):04X}"
    with (process / "net/tcp").open() as sockets:
        next(sockets)
        listening = {f"socket:[{fields[9]}]" for fields in map(str.split, sockets)
                     if fields[1] == f"0100007F:{port}" and fields[3] == "0A"}
    for descriptor in (process / "fd").iterdir():
        try:
            if str(descriptor.readlink()) in listening:
                return True
        except FileNotFoundError:
            continue
    return False


@contextmanager
def private_server(command, address, out, env, cpu):
    pidfile = out / "server.pid"
    boot = ["sh", "-c", 'echo "$$" > "$1"; shift; exec "$@"', "checkbox-board", pidfile]
    with (out / "server.log").open("wb") as log:
        with subprocess.Popen(["taskset", "-c", str(cpu), CAP, *boot, *command],
                              cwd=ROOT, env=env, stdout=log, stderr=log) as process:
            while not pidfile.exists() and process.poll() is None:
                time.sleep(0.05)
            pid = int(pidfile.read_text()) if pidfile.exists() else None
            try:
                expected = Path(command[0]).resolve()
                while process.poll() is None:
                    try:
                        if pid is not None and Path(f"/proc/{pid}/exe").resolve() == expected and owns_listener(pid, address):
                            break
                    except FileNotFoundError:
                        pass
                    time.sleep(0.05)
                else:
                    raise RuntimeError(f"private checkbox server exited before listen: {process.returncode}")
                yield pid
            finally:
                if process.poll() is None and pid is not None:
                    try:
                        comm = subprocess.run(["ps", "-o", "comm=", "-p", str(pid)],
                                              capture_output=True, text=True)
                        if (comm.stdout.strip() == Path(command[0]).name[:15]
                                and Path(f"/proc/{pid}/exe").resolve() == Path(command[0]).resolve()):
                            os.kill(pid, signal.SIGTERM)
                    except ProcessLookupError:
                        pass


def checkbox_rows(board, out, prepared, server_cpu, loader_cpu):
    checkbox, executable, address, wants, tools = prepared
    env = build.capped_env(out)
    overrides = ["--per-ip-sessions", "20000", "--principal-rate", "1000000000"]
    siblings = sibling_cpus((server_cpu, loader_cpu))
    clients, toggles = CHECKBOX_CLIENTS, 200
    for index, (mode, metric) in enumerate(CHECKBOX_METRICS.items()):
        samples = {}
        for side in (("hirsel", "rust") if index % 2 == 0 else ("rust", "hirsel")):
            directory = out / f"{mode}-{side}"
            directory.mkdir()
            if side == "hirsel":
                command = checkbox.command(executable, address, [])
            else:
                command = [tools / "wire-reference", "--listen", address, "--wal", directory / "wire.wal",
                           "--n-place", checkbox.N_PLACE, "--wants", wants, *overrides]
            busy = {}
            try:
                with smt_activity(siblings, f"{mode}/{side}") as busy, private_server(
                    command, address, directory, env, server_cpu
                ) as pid:
                    load_command = [tools / "wire-load", mode, "--url", f"ws://{address}/hirsel",
                                    "--wants", wants, "--clients", str(clients)]
                    load_command += ["--toggles", str(toggles), "--pace", "1000"] if mode == "converge" else ["--server-pid", str(pid)]
                    started = time.monotonic()
                    result = subprocess.run(["taskset", "-c", str(loader_cpu), CAP, *load_command], env=env,
                                            capture_output=True, text=True)
                    (directory / "client.log").write_text(result.stdout + result.stderr)
                    seconds = time.monotonic() - started
                if mode == "converge":
                    valid = result.returncode == 0 and re.search(
                        rf"^Rust converge smoke PASS clients={clients} Returned={clients * toggles} ",
                        result.stdout, re.M)
                    samples[side] = {"status": "ok" if valid else "incomplete-convergence",
                                     "seconds": seconds, metric: clients * toggles / seconds if valid else None}
                else:
                    sample = json.loads(result.stdout)
                    valid = result.returncode == 0 and sample["complete"] and sample["connections"] == clients
                    samples[side] = sample | {"status": "ok" if valid else "incomplete-memory"}
            except (OSError, ValueError, RuntimeError) as error:
                samples[side] = {"status": str(error)}
            finally:
                samples[side]["smt_busy_percent"] = busy
                if side == "hirsel":
                    (out / "data").rename(directory / "data")
                    (out / "data").mkdir()
        accepted(out / "build.json")
        board.row("Checkbox", f"{mode}, {clients} clients", samples, (metric,))


def prepare_arena(out, args, compiler, artifacts, env):
    out.mkdir()
    rust = args.langarena / "rust"
    target = out / "rust-target"
    fetch(rust, env)
    run(cargo("build", "--release", "--locked", "--offline", "-j2", "--target-dir", target),
        cwd=rust, env=cargo_scope(env))
    shutil.copy2(target / "release/benchmarks", out / "rust")
    build.goose_executable(compiler, args.goose / "bench/kostya/goose/src/main.goose", out / "goose", env)
    config = out / "run.js"
    shutil.copyfile(args.langarena / config.name, config)
    rows = json.loads(config.read_text())
    if sorted(row["name"] for row in rows) != sorted(EXPECTED["LangArena"]):
        raise ValueError("LangArena requires 50 distinct workloads")
    arena.build_hirsel(out, artifacts, rows, env)
    return rows


def run_board(board, args):
    out = board.out
    board.report["commit"] = git(ROOT, "rev-parse", "HEAD")
    if git(ROOT, "status", "--porcelain"):
        raise ValueError("run the board from a clean Hirsel checkout")
    for name, commit in PINS.items():
        checkout = getattr(args, name)
        if git(checkout, "rev-parse", "HEAD") != commit or git(checkout, "status", "--porcelain", "--untracked-files=no"):
            raise ValueError(f"{checkout} must have unmodified tracked sources at {commit}")

    env = build.capped_env(out, "8G", "2")
    artifact_file = out / "toolchain.json"
    run([sys.executable, ROOT / "tools/buck2/prepare.py"], env=env)
    with artifact_file.open("wb") as stdout:
        run([sys.executable, ROOT / "core/tools/measurement_artifacts.py", "--threads", "2"],
            env=env, stdout=stdout)
    artifacts = json.loads(artifact_file.read_text())
    tools = receipt.release_tools(artifacts)
    board.report["toolchain"] = artifacts
    versions = {"awfy_rustc": ["rustc"], "cargo_rustc": ["rustc", "+1.99.0"],
                "cargo": cargo(), "hirsel_clang": [build.llvm_path() / "bin/clang"],
                "goose_clang": ["clang"], "perf": ["perf"]}
    board.report["compiler_versions"] = {
        tool: subprocess.check_output([*command, "--version"], text=True).splitlines()[0]
        for tool, command in versions.items()
    }
    goose_dir = out / "goose-build"
    run(["cmake", "-S", args.goose, "-B", goose_dir, "-DCMAKE_BUILD_TYPE=Release"], env=env)
    run(["cmake", "--build", goose_dir, "--parallel", "2", "--target", "goose"], env=env)
    goose = goose_dir / "goose"

    awfy = out / "awfy"
    awfy.mkdir()
    settings = argparse.Namespace(out=awfy, artifacts=artifact_file, upstream=args.awfy, goose=goose,
                                  rust=args.resid / "bench/v2/t1-awfy/rust", env=env, hello=False,
                                  hirsel_entries=("harness", "pools", "pool_index"),
                                  goose_entries=("harness", "pools", "pool_index"))

    def build_awfy():
        for builder in (build.rust, build.goose, build.hirsel):
            builder(settings, matched=True)
        return awfy

    awfy_ready = board.attempt("AWFY build", build_awfy)
    arena_dir = out / "arena"
    arena_rows = board.attempt("LangArena build", prepare_arena, arena_dir, args, goose, artifacts, env)
    compile_dir = out / "compile"
    compile_ready = board.attempt("Compile preparation", prepare_compile, compile_dir, artifacts, env)
    checkbox_dir = out / "checkbox"
    checkbox_ready = board.attempt("Checkbox build", prepare_checkbox, checkbox_dir, artifacts, env)
    with measurement_slot(args.lock):
        os.sched_setaffinity(0, {args.measure_cpus[0]})
        if awfy_ready:
            board.attempt("AWFY measurement", awfy_rows, board, awfy, args.awfy, tools)
        if arena_rows:
            fixture = arena_dir / "one-iteration.json"
            startup_rows = [dict(row, iterations=1, warmup_iterations=0, checksum=0)
                            for row in arena_rows]
            board.attempt("LangArena one-iteration fixture",
                          arena.oracle, arena_dir, arena_dir / "rust", startup_rows)
            if "LangArena one-iteration fixture" not in board.report["failures"]:
                board.attempt("LangArena startup", lambda: runtime_rows(
                    board, "arena", arena_dir, None, tools, config=fixture, startup=True))
                if compile_ready:
                    board.attempt("Compile measurement", compile_rows, board, compile_dir, compile_ready,
                                  fixture, tools)
            board.attempt("LangArena runtime", lambda: runtime_rows(
                board, "arena", arena_dir, None, tools, config=arena_dir / "run.js"))
        elif compile_ready:
            board.report["failures"]["Compile measurement"] = "no one-iteration Spectralnorm fixture"
        if checkbox_ready:
            os.sched_setaffinity(0, args.measure_cpus[:2])
            board.attempt("Checkbox measurement", checkbox_rows, board, checkbox_dir, checkbox_ready,
                          *args.measure_cpus[:2])


def validate_baseline(path, report):
    rows = rows_by_label(report)
    if (report["complete"] is not True
            or rows.keys() != {(goal, name) for goal, names in EXPECTED.items() for name in names}
            or any(row["status"] != "ok" for row in rows.values()) or inactive_counters(report)
            or receipt.rows_from(path).keys() != set(instruction_keys())):
        raise ValueError(f"{path}: a baseline requires all current rows and fully active counters")
    if report["size_measurement"]["verified_workloads"] != sorted(EXPECTED["AWFY original"]):
        raise ValueError(f"{path}: size requires all original-14 workloads")
    size_changes(report, report)
    for mode, metric in CHECKBOX_METRICS.items():
        row = rows["Checkbox", f"{mode}, {CHECKBOX_CLIENTS} clients"]
        list(checkbox_changes(row, row, metric))


def rebaseline(run):
    path = run / "board.json"
    if not path.is_file():
        path = run / "board/board.json"
    path = path.resolve()
    report = receipt.read(path)
    if report["complete"] is not True or report["exit_status"] != 0:
        raise ValueError(f"{path}: rebaseline requires a complete, clean run (exit 0)")
    validate_baseline(path, report)
    base = Path.home() / ".local/share/hirsel-board"
    base.mkdir(parents=True, exist_ok=True)
    refused = base / "baseline-refused"
    with measurement_slot(base / "baseline.lock"):
        previous = refused.read_text().strip() if refused.exists() else None
        with tempfile.TemporaryDirectory(dir=base) as scratch:
            link = Path(scratch) / "baseline"
            link.symlink_to(path)
            link.replace(base / "last-complete.json")
        refused.unlink(missing_ok=True)
    print(f"Promoted {path} ({report['commit']}) because it is a complete, clean board.")
    if previous:
        print(f"Cleared the promotion hold from {previous}.")
    return 0


def main():
    if sys.argv[1:2] == ["rebaseline"]:
        parser = argparse.ArgumentParser(prog=f"{sys.argv[0]} rebaseline",
                                         description="Promote a complete, clean board and clear a promotion hold")
        parser.add_argument("run", type=Path)
        args = parser.parse_args(sys.argv[2:])
        try:
            return rebaseline(args.run)
        except (*receipt.INVALID_RECEIPT, ArithmeticError) as error:
            print(f"rebaseline refused: {error}", file=sys.stderr)
            return receipt.REFUSED

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("out", type=Path)
    parser.add_argument("--baseline", type=lambda path: Path(path).resolve(),
                        help="prior complete board.json for instruction, size and checkbox gates")
    for name in PINS:
        parser.add_argument(f"--{name}", type=lambda path: Path(path).resolve(), required=True,
                            help=f"clean checkout at {PINS[name]}")
    add_cpu_sets(parser)
    parser.add_argument("--lock", type=Path, default=LOCK)
    args = parser.parse_args()
    validate_cpu_sets(parser, args, width=2)
    os.sched_setaffinity(0, args.build_cpus)
    out = args.out.resolve()
    if out.is_relative_to(ROOT):
        parser.error("use a new evidence directory outside the checkout")
    out.mkdir(parents=True)
    board = Board(out, None)
    board.save()
    board.attempt("Board", run_board, board, args)
    return board.finish(args.baseline)


if __name__ == "__main__":
    raise SystemExit(main())
