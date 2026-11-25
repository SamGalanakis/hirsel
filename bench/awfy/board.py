#!/usr/bin/env python3
"""Build and run the matched Hirsel goal board. Keep outputs outside the checkout."""
import argparse
from contextlib import contextmanager, redirect_stderr, redirect_stdout
from datetime import datetime, time as time_of_day
from decimal import Decimal
from io import StringIO
import importlib.util
import json
import math
import os
from pathlib import Path
import re
import shutil
import shlex
import signal
import socket
import subprocess
import sys
import tempfile
import time

import arena
import awfy_measure
import board_baseline
import board_host
import board_markdown
import board_size
import build
import comparison
import comparison_units
import receipt
from board_baseline import rows_by_label
from checkbox_gate import CHECKBOX_CLIENTS, checkbox_changes, process_rss_bytes, session_rss_sample
from receipt import measurement_slot, write as write_json
from timing_shield import BOARD as LOCK, REGIMES, hold, measurement_affinity
from comparison import SPECTRALNORM, SPECTRALNORM_CHECKSUM, spectralnorm_marker
from bench_cpus import add_cpu_sets, sibling_cpus, smt_activity, validate_cpu_sets
from measurement_artifacts import build_outputs

ROOT = build.ROOT
CAP = ROOT / "tools/capped"
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
INSTRUCTION_LIMIT = Decimal(2)
SIZE_LIMIT = 2048
CHECKBOX_METRICS = {
    "converge": "toggles_per_second",
    "memory-subscribed": "server_rss_bytes",
    "memory-unsubscribed": "server_rss_bytes",
}
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
    "Throughput is reported as a trend. Each memory row reads the server's smaps_rollup RSS "
    "after startup before any session opens and again while 64 sessions are live, and reports "
    "the increase per live session. The board flags that RSS rising by more than 10%. "
    "Goose has no compile or checkbox peer here.", board_host.NOTE,
)


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


place_density = load("place_density", ROOT / "bench/place-density/run.py")
EXPECTED = {**awfy_measure.EXPECTED, "Startup": tuple(label for _, label in STARTUP.values()), place_density.GOAL: (place_density.ROW,)}


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


def inactive_counters(report):
    suites = {title: suite for suite, title in awfy_measure.SUITES.items()}
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


def instruction_keys():
    return [f"{suite}/{name}" for suite, goal in awfy_measure.SUITES.items() for name in EXPECTED[goal]]


class Board:
    def __init__(self, out, commit, *, persist=True):
        self.out = out
        self.persist = persist
        self.report = {"commit": commit, "pins": PINS, "notes": NOTES, "rows": [], "failures": {},
                       "instruction_receipt": receipt.make_receipt(instruction_keys())}

    def save(self):
        if not self.persist:
            return
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
        (self.out / "board.md").write_text(board_markdown.render(self.report, expected, place_density.GOAL))

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
        if self.report.get("place_density", {}).get("regressed"):
            status = 3
        self.report["exit_status"] = status
        self.save()
        print(self.out / "board.md", flush=True)
        return status

    def compare_previous(self, baseline, blocked):
        output = StringIO()
        reference = {}
        receipt_change = {}
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
        except receipt.OlderBoardReceipt as error:
            instructions = receipt.REFUSED
            if not blocked:
                receipt_change = {"previous_receipt_version": error.version}
            print(f"REFUSED ({error})", file=output)
        except receipt.INVALID_RECEIPT as error:
            instructions = receipt.REFUSED
            print(f"REFUSED ({error})", file=output)
        status = ("Within +2% on every row", "Instruction regression", "Refused")[instructions]
        details = output.getvalue().splitlines()
        self.report["instruction_comparison"] = {"status": status, "details": details, **receipt_change}

        size, checkbox = [], []
        refused, failed = instructions == receipt.REFUSED, instructions == 1
        try:
            for side, before, after in board_size.size_changes(reference, self.report):
                delta = after - before
                breach = side == "hirsel" and delta >= SIZE_LIMIT
                failed |= breach
                line = f"{side} {before:,} -> {after:,} B ({delta:+,} B){' FAIL' if breach else ''}"
                size.append(line)
                if delta > 0:
                    print(line)
                if breach:
                    try:
                        self.report["size_census"] = board_size.size_census(
                            baseline, self.out / "board.json", reference, self.report, side)
                    except (*receipt.INVALID_RECEIPT, subprocess.SubprocessError) as error:
                        self.report["size_census"] = {"error": str(error)}
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
        sides = {place_density.GOAL: ("hirsel",), "Compile": ("hirsel", "rust"),
                 "Checkbox": ("hirsel", "rust")}.get(goal, awfy_measure.SIDES)
        complete = True
        for side in sides:
            sample = samples.get(side, {})
            status = sample.get("status", "missing")
            if status == "ok" and not all(
                field in sample and math.isfinite(float(sample[field]))
                and (float(sample[field]) > 0 or
                     (field == "idle_instructions_per_second" and sample[field] == 0))
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


def sample_runtime(row, commands, directory, samples_dir, env, siblings, index, label, *, extended=False):
    samples = {}
    order = ("rust", "hirsel", "goose") if index % 2 == 0 else ("goose", "hirsel", "rust")
    for side in order:
        if extended:
            require_comparison_window()
        command, marker = commands[side]
        target = samples_dir.with_name(f"{samples_dir.name}-{side}")
        if side == "hirsel":
            sample = receipt.observe(row, target, env, siblings, label=f"{label}/{side}")
            output = row["stdout"].encode().splitlines(keepends=True)
        else:
            with smt_activity(siblings, f"{label}/{side}") as busy:
                sample = awfy_measure.core_measure.sample(command, target, env=env, cwd=directory, pmu_times=True)
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


def runtime_rows(board, suite, directory, upstream, tools, *, config=None, startup=False, extended=False):
    goal = awfy_measure.SUITES[suite]
    rows = (awfy_measure.arena_rows(directory, config) if config is not None
            else awfy_measure.runtime_cases(suite, directory, upstream))
    if extended:
        rows = list(rows)
        if sorted(name for name, _ in rows) != sorted(EXPECTED[goal]):
            raise ValueError(f"{goal}: required distinct workloads")
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
        samples = sample_runtime(row, commands, directory, target, env, siblings, index,
                                 f"{title}/{label}", extended=extended)
        if not startup:
            board.report["instruction_receipt"]["rows"].append(row)
        if all(sample["verified"] for sample in samples.values()):
            verified.append(name)
        metrics = ("seconds",) if startup else ("seconds", "instructions") if suite == "arena" else ("instructions",)
        board.row(title, label, samples, metrics, notes=matmul_notes(name))
        if not extended and suite != "arena" and name == startup_name:
            board.row("Startup", startup_label,
                      {side: sample.copy() for side, sample in samples.items()}, ("seconds",))
    return verified


def awfy_rows(board, out, upstream, tools):
    originals = [(name, commands) for name, commands in awfy_measure.rows(out, False, upstream) if name != "Hello"]
    if sorted(dict(originals)) != sorted(EXPECTED["AWFY original"]) or len(originals) != 14:
        raise ValueError("the original board requires 14 distinct AWFY rows")
    verified = runtime_rows(board, "awfy", out, upstream, tools)
    if len(verified) == len(originals):
        result = awfy_measure.verified_sizes(out, verified)
        board.report["size_measurement"] = {
            "verified_workloads": sorted(verified), "strip_version": result["strip_version"],
            "strip_flags": result["strip_flags"],
            "pins": {peer: PINS[peer] for peer in ("awfy", "resid", "goose")},
        }
        board.row("Size", board_size.SIZE_ROW,
                  {side: {"status": "ok", **image} for side, image in result["images"].items()}, ("bytes",))
    else:
        board.report["failures"]["Size"] = "not all original harness results verified"
        board.save()
    pools = list(awfy_measure.matched_pool_rows(out, upstream))
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


def stage_arena_cargo(out):
    rust = out / "rust"
    source = ROOT / "core/langarena/rust"
    rust.mkdir()
    for name in ("Cargo.toml", "Cargo.lock"):
        shutil.copyfile(source / name, rust / name)
    shutil.copytree(source / "src", rust / "src")
    return rust


def prime_cargo(operation, profile, rust, target, package, env, *, release=False, jobs=2):
    run(cargo(operation, f"-j{jobs}", *profile), cwd=rust, env=cargo_scope(env))
    run(cargo("clean", *(("--release",) if release else ()), "--target-dir", target, "-p", package),
        cwd=rust, env=cargo_scope(env))


def prepare_compile(out, artifacts, env):
    out.mkdir()
    rust = stage_arena_cargo(out)
    target = out / "target"
    prime_cargo("build", compile_profile(target), rust, target, "benchmarks", env, release=True)
    prepare_hirsel_compile(out, artifacts, env)
    return compile_commands(out)


def publish_std(out, artifacts, env):
    # The toolchain bundle prebuilds the release std, so the board's std does.
    run([artifacts["compiler"], "std-product", "--store", out / "packages",
         "--target", artifacts["target"]], env=env)


def prepare_hirsel_compile(out, artifacts, env, *, packages=None):
    suite = load("arena_suite", ROOT / "core/langarena/run.py")
    if packages is None:
        publish_std(out, artifacts, env)
    else:
        (out / "packages").symlink_to(packages, target_is_directory=True)
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


def edit_arena_hirsel(path):
    edit_checksum(path, "helper.checksum_f64(float.sqrt(vbv / vv))",
                  "Ok(int.wrapping_add(helper.checksum_f64(float.sqrt(vbv / vv))?, 1u32))")


def edit_arena_rust(rust):
    edit_checksum(rust / "src/benchmarks/spectralnorm.rs", "helper::checksum_f64(result_value)",
                  "helper::checksum_f64(result_value).wrapping_add(1)")


def verify_output(command, cwd, env):
    result = subprocess.run([CAP, *command], env=env, cwd=cwd, capture_output=True)
    return {"returncode": result.returncode, "stdout": result.stdout.decode(),
            "stderr": result.stderr.decode()}


def compile_rows(board, out, prepared, fixture, tools):
    rust, clbg, hirsel, cargo_build, exe, rust_exe = prepared
    env = build.capped_env(out)
    config = out / "fixture.json"
    shutil.copyfile(fixture, config)
    tasks = receipt.read(config)
    name, expected = SPECTRALNORM, SPECTRALNORM_CHECKSUM
    task = next(row for row in tasks if row["name"] == name)
    if task["checksum"] != expected:
        raise ValueError(f"{name}: expected the pinned cold checksum {expected}")
    siblings = sibling_cpus(os.sched_getaffinity(0))
    for mode in ("cold", "edit"):
        if mode == "edit":
            edit_arena_hirsel(clbg)
            edit_arena_rust(rust)
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
                sample = awfy_measure.core_measure.sample(command, target, env=sample_env, cycles=True,
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
            verify = verify_output([executable, config.name, name], out, env)
            (target / "verification").write_text(verify["stdout"] + verify["stderr"])
            marker = spectralnorm_marker(side, expected)
            sample["verified"] = awfy_measure.matches_output(verify, marker)
            if not sample["verified"]:
                sample["status"] = "wrong-result"
            elif sample.get("std_lowered"):
                sample["status"] = "std-lowered"
            elif sample["status"] == "ok" and not sample["seconds"]:
                sample["status"] = "missing-duration"
            elif mode == "edit" and not rebuilt:
                sample["status"] = "no-parts-compiled" if side == "hirsel" else "crate-not-rebuilt"
            if side == "hirsel":
                argv = list(map(str, command))
                argv[argv.index("--cc") + 1] = "clang"
                board.report["instruction_receipt"]["rows"].append({
                    "key": f"compile/{label}", "suite": "compile", "mode": mode,
                    "flags": provenance["flags"],
                    "argv": argv, "build_record": record,
                    "image_sha256": record["image_sha256"], "clang_sha256": record["clang_sha256"],
                    "affinity": sorted(os.sched_getaffinity(0)), "marker": marker + "\n",
                    "stdout": verify["stdout"], "sample": sample, **fixture_id, **tools,
                })
        board.row("Compile", label, samples, ("seconds", *COUNTERS))
        if mode == "cold" and not all(sample.get("verified") for sample in samples.values()):
            raise ValueError("cannot measure a warm edit without two verified cold builds")


def arena_editors(out, rust, goose_source):
    goose_checksum = goose_source.with_name("spectralnorm.goose")
    return {
        "hirsel": lambda: edit_arena_hirsel(out / "clbg.hirsel"),
        "rust": lambda: edit_arena_rust(rust),
        "goose": lambda: edit_checksum(goose_checksum, "return checksum_f64(result_value);",
                                       "return checksum_f64(result_value) + 1;"),
    }


def prepare_comparison_compile(out, recipe, args, artifacts, goose, packages, env):
    out.mkdir()
    check = recipe == "arena-check"
    release = recipe.endswith("-release")
    target = out / "target"
    commands, images = {}, {}
    package = "benchmarks"
    if recipe == "awfy-release":
        rust = out / "rust"
        rust.mkdir()
        build.stage_awfy_rust(args.resid / "bench/v2/t1-awfy/rust", rust / "src")
        package = "awfy-board"
        (rust / "Cargo.toml").write_text('[package]\nname = "awfy-board"\nversion = "0.0.0"\nedition = "2021"\n')
        run(cargo("generate-lockfile", "--offline"), cwd=rust, env=cargo_scope(env))
        shutil.copytree(build.HERE / "hirsel", out / "hirsel-src")
        shutil.copytree(build.HERE / "goose", out / "goose-src")
        manifest = out / "inputs.json"
        write_json(manifest, build.awfy_manifest((out / "hirsel-src").glob("*.hirsel")))
        goose_source = out / "goose-src/harness.goose"
        images["hirsel"] = out / "hirsel-harness"
        commands["hirsel"] = build.awfy_hirsel_command(artifacts, out, "harness", manifest, images["hirsel"])
        # Add the same published std/core dependency state as the other release arm.
        commands["hirsel"] += ["--core-product", packages]
        edit = {
            "hirsel": lambda: edit_checksum(out / "hirsel-src/harness.hirsel",
                                            r'"verified\n"', r'"verified board-edit\n"'),
            "rust": lambda: edit_checksum(rust / "src/main.rs", 'runtime: {}us", name, us',
                                          'runtime: {}us board-edit", name, us'),
            "goose": lambda: edit_checksum(goose_source, r'"verified\n"', r'"verified board-edit\n"'),
        }
    else:
        rust = stage_arena_cargo(out)
        prepare_hirsel_compile(out, artifacts, env, packages=packages)
        shutil.copytree(args.goose / "bench/kostya/goose/src", out / "goose-src")
        goose_source = out / "goose-src/main.goose"
        _, _, command, _, image, _ = compile_commands(out)
        if check:
            command = ["./pinned-compiler", "check", "--inputs", "inputs.json", "--core-product", "packages",
                       "--store", "store", "--entry", "langarena@1/suite/main", "--json"]
        elif not release:
            command[command.index("llvm")] = "cranelift"
            command.remove("--release")
        commands["hirsel"] = command
        images["hirsel"] = None if check else image
        edit = arena_editors(out, rust, goose_source)

    profile = compile_profile(target) if release else [
        "--offline", "--locked", "--target-dir", target, "--config", "profile.dev.incremental=true"]
    operation = "check" if check else "build"
    if package == "benchmarks":
        prime_cargo(operation, profile, rust, target, package, env, release=release, jobs=4)
    else:
        # AWFY has no dependencies to prime.
        if target.exists():
            raise ValueError("AWFY Cargo target must be empty before the cold build")
    commands["rust"] = cargo(operation, "-j1", "--message-format=json", *profile)
    images["rust"] = None if check else target / ("release" if release else "debug") / package
    images["goose"] = None if check else out / ("goose-harness" if package == "awfy-board" else "goose")
    goose_env = env | {"GOOSE_HOME": str(out / "goose-home")}
    if check:
        commands["goose"] = [goose, "check", "--stats", goose_source]
    elif release:
        cflags = build.AWFY_GOOSE_CFLAGS if package == "awfy-board" else ()
        stages = build.goose_release_commands(goose, goose_source, images["goose"], cflags)
        commands["goose"] = ["sh", "-ec", " && ".join(shlex.join(map(str, command)) for command in stages)]
    else:
        commands["goose"] = [goose, "build", "-O0", "--debug", "--standalone",
                             "--cc", build.llvm_path() / "bin/clang", "-o", images["goose"], goose_source]
    plan = {
        side: (commands[side], rust if side == "rust" else out, images[side],
               cargo_scope(env) if side == "rust" else goose_env if side == "goose" else env, edit[side])
        for side in awfy_measure.SIDES
    }
    return plan, package


def hirsel_build_evidence(path, check, manifest, image, cwd):
    report = json.loads(path.read_text())
    ok = (report["accepted"] is True and report["stopped"] is None
          and not any(report[key] for key in ("diagnostics", "incomplete", "resources")))
    if check:
        own_sources = manifest["packages"][-1]["sources"]
        correct_files = (report["files"] == len(own_sources) and report["source_bytes"] ==
                         sum(Path(source["file"]).stat().st_size for source in own_sources))
        return {"rebuilt": ok and correct_files and report["rechecked"] > 0,
                "front_only": report["native"] is None,
                "rechecked_bodies": report["rechecked"], "source_files": report["files"],
                "expected_source_files": len(own_sources), "source_bytes": report["source_bytes"],
                "check_root": report["root"]}
    native = report["native"] or {}
    return {
        "rebuilt": (ok and native.get("linked") is True and native.get("compiled", 0) > 0
                    and (cwd / native["executable"]).resolve() == image.resolve() and image.is_file()),
        "compiled_parts": native.get("compiled"),
        "release_std_lowered": native.get("reach", {}).get("std_lowered"),
    }


def cargo_build_evidence(path, package, check, image):
    messages = [json.loads(line) for line in path.read_text().splitlines()]
    artifacts = [line for line in messages if line["reason"] == "compiler-artifact"]
    own = [line for line in artifacts if line["target"]["name"] == package and "bin" in line["target"]["kind"]]
    dependency_work = [line["target"]["name"] for line in artifacts if line not in own and not line["fresh"]]
    rebuilt = len(own) == 1 and own[0]["fresh"] is False and any(
        line["reason"] == "build-finished" and line["success"] is True for line in messages)
    if check:
        metadata = rebuilt and own[0]["executable"] is None and any(
            filename.endswith(".rmeta") and Path(filename).is_file() for filename in own[0]["filenames"])
        return {"rebuilt": bool(metadata), "front_only": bool(metadata), "dependency_work": dependency_work}
    return {"rebuilt": rebuilt and own[0]["executable"] == str(image) and image.is_file(),
            "dependency_work": dependency_work}


def goose_build_evidence(target, check, image):
    if not check:
        return {"rebuilt": image.is_file() and os.access(image, os.X_OK)}
    output = (target / "stdout").read_text() + "\n" + (target / "stderr").read_text()
    files = re.search(r"^parsed ok: \d+ top-level declarations, (\d+) file\(s\)$", output, re.M)
    verified = files is not None and int(files[1]) > 1 and re.search(r"^typechecked ok: ", output, re.M)
    return {"rebuilt": bool(verified), "front_only": bool(verified),
            "source_files": int(files[1]) if files else None}


def compile_verification(out, recipe, side, image, mode, args, env):
    if recipe == "awfy-release":
        return {name: verify_output([image, *commands[side][0][1:]], out, env)
                for name, commands in awfy_measure.runtime_cases("awfy", out, args.awfy)}
    checksum = SPECTRALNORM_CHECKSUM + (mode == "edit")
    config = out / f"fixture-{mode}.json"
    if not config.exists():
        task, = [row for row in receipt.read(args.langarena / "run.js") if row["name"] == SPECTRALNORM]
        write_json(config, [dict(task, iterations=1, warmup_iterations=0, checksum=checksum)])
    result = verify_output([image, config.name, SPECTRALNORM], out, env)
    return {SPECTRALNORM: result | {"expected_checksum": checksum}}


def native_objects(package_store):
    # Count writes to the published library store only. Recipe assembly also
    # emits library units into the request's native store.
    objects = set()
    for directory in package_store.resolve().rglob("native"):
        for path in directory.iterdir():
            if path.is_file() and re.fullmatch(r"[0-9a-f]{64}", path.name):
                with path.open("rb") as binary:
                    header = binary.read(18)
                if header[:6] == b"\x7fELF\x02\x01" and header[16:18] == b"\x01\0":
                    objects.add(path)
    return objects


def measure_comparison_compile(evidence, out, recipe, prepared, args, modes):
    plan, package = prepared
    check = recipe == "arena-check"
    siblings = sibling_cpus(os.sched_getaffinity(0))
    manifest = receipt.read(out / "inputs.json")
    cold, check_root = {}, None
    for mode in modes:
        mode_id = f"{recipe}/{mode}"
        samples = evidence["samples"][mode_id] = {}
        order = awfy_measure.SIDES if mode == "cold" else awfy_measure.SIDES[::-1]
        for side in order:
            require_comparison_window()
            command, cwd, image, env, edit = plan[side]
            env = env | {"HIRSEL_MEMORY_LIMIT": "2G", "HIRSEL_JOBS": "1"}
            cache = "empty application state" if mode == "cold" else "reuse successful cold application state"
            if side == "goose":
                cache = "full recheck" if check else "full rebuild"
            elif side == "rust":
                cache += (", dependencies primed" if package == "benchmarks" else ", no dependencies")
            elif recipe == "arena-dev":
                cache += ", installed product; Cranelift library and app work included together"
            meta = {"commit": evidence["commit"], "mode": mode_id, "cache": cache,
                    "command": list(map(str, command)), "command_line": shlex.join(map(str, command)),
                    "affinity": sorted(os.sched_getaffinity(0)), "cwd": str(cwd)}
            if mode == "edit" and not cold.get(side):
                samples[side] = meta | {"status": "no qualified cold build"}
                continue
            try:
                if mode == "edit":
                    edit()
                if image is not None:
                    image.unlink(missing_ok=True)
                    if side == "goose":
                        image.with_suffix(".c").unlink(missing_ok=True)
                target = out / f"{mode}-{side}"
                dev_hirsel = side == "hirsel" and recipe == "arena-dev"
                before = native_objects(out / "packages") if dev_hirsel else set()
                with smt_activity(siblings, f"{mode_id}/{side}") as busy:
                    sample = awfy_measure.core_measure.sample(command, target, env=env, cycles=True,
                                                              cwd=cwd, pmu_times=True)
                sample.update(meta, smt_busy_percent=busy)
                if dev_hirsel:
                    sample["cranelift_library_objects_written"] = len(native_objects(out / "packages") - before)
                samples[side] = sample
                if sample["status"] not in ("ok", "missing-counter"):
                    continue
                if side == "hirsel":
                    sample.update(hirsel_build_evidence(target / "stdout", check, manifest, image, cwd))
                    if recipe.endswith("-release") and mode == "edit" and sample["release_std_lowered"] != 0:
                        sample["status"] = "release std was lowered again"
                    if check:
                        if mode == "cold":
                            check_root = sample["check_root"]
                        elif sample["check_root"] == check_root:
                            sample["status"] = "check root did not change after the edit"
                elif side == "rust":
                    sample.update(cargo_build_evidence(target / "stdout", package, check, image))
                    if sample["dependency_work"]:
                        sample["status"] = "Cargo dependencies were not fully primed"
                else:
                    sample.update(goose_build_evidence(target, check, image))
                if not check and sample["rebuilt"]:
                    sample["verification"] = compile_verification(out, recipe, side, image, mode, args, env)
                if mode == "cold":
                    cold[side] = (sample["status"] in ("ok", "missing-counter")
                                  and comparison.compile_oracle(sample, side, recipe, mode))
            except (OSError, ValueError, KeyError, RuntimeError, subprocess.SubprocessError) as error:
                samples[side] = samples.get(side, meta) | {"status": str(error)}


def prepare_checkbox(out, artifacts, env, *, wire_tools=None, jobs=2):
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
    run([artifacts["compiler"], "sdk", "ts", executable, "--output", web,
         "--n-place", checkbox.N_PLACE], env=env)
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
        run(cargo("build", "--release", "--locked", f"-j{jobs}", "--target-dir", target),
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


def checkbox_rows(board, out, prepared, server_cpu, loader_cpu, *, extended=False):
    checkbox, executable, address, wants, tools = prepared
    env = build.capped_env(out)
    overrides = ["--per-ip-sessions", "20000", "--principal-rate", "1000000000"]
    siblings = sibling_cpus((server_cpu, loader_cpu))
    clients, toggles = CHECKBOX_CLIENTS, 200
    modes = ((mode, CHECKBOX_METRICS[mode]) for mode in comparison.CHECKBOX_ROWS) if extended else CHECKBOX_METRICS.items()
    for index, (mode, metric) in enumerate(modes):
        samples = {}
        for side in (("hirsel", "rust") if index % 2 == 0 else ("rust", "hirsel")):
            if extended:
                require_comparison_window()
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
                    idle_rss = process_rss_bytes(pid) if metric == "server_rss_bytes" else None
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
                    sample = session_rss_sample(json.loads(result.stdout), idle_rss)
                    valid = result.returncode == 0 and sample["complete"] and sample["connections"] == clients
                    samples[side] = sample | {"status": "ok" if valid else "incomplete-memory"}
                    if extended:
                        samples[side]["returncode"] = result.returncode
            except (OSError, ValueError, RuntimeError) as error:
                samples[side] = {"status": str(error)}
            finally:
                samples[side]["smt_busy_percent"] = busy
                if side == "hirsel":
                    (out / "data").rename(directory / "data")
                    (out / "data").mkdir()
        accepted(out / "build.json")
        board.row("Checkbox", f"{mode}, {clients} clients", samples, (metric,))


def prepare_arena(out, args, compiler, artifacts, env, *, jobs=2):
    out.mkdir()
    rust = args.langarena / "rust"
    target = out / "rust-target"
    fetch(rust, env)
    run(cargo("build", "--release", "--locked", "--offline", f"-j{jobs}", "--target-dir", target),
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


def required_peers(selected):
    peers = set()
    if selected & {"runtime-memory", "compile", "image-size"}:
        peers.update(("awfy", "resid", "goose"))
    if selected & {"runtime-memory", "compile", "check"}:
        peers.update(("langarena", "goose"))
    return peers


def validate_sources(args, selected):
    if git(ROOT, "status", "--porcelain"):
        raise ValueError("run the board from a clean Hirsel checkout")
    peers = required_peers(selected)
    for name, commit in PINS.items():
        if name not in peers:
            continue
        checkout = getattr(args, name)
        if git(checkout, "rev-parse", "HEAD") != commit or git(checkout, "status", "--porcelain", "--untracked-files=no"):
            raise ValueError(f"{checkout} must have unmodified tracked sources at {commit}")


def prepare_toolchain(artifact_file, env, *, jobs):
    run([sys.executable, ROOT / "tools/buck2/prepare.py"], env=env)
    with artifact_file.open("wb") as stdout:
        run([sys.executable, ROOT / "core/tools/measurement_artifacts.py", "--threads", str(jobs)],
            env=env, stdout=stdout)
    artifacts = json.loads(artifact_file.read_text())
    return artifacts, receipt.release_tools(artifacts)


def compiler_versions():
    versions = {"awfy_rustc": ["rustc"], "cargo_rustc": ["rustc", "+1.99.0"],
                "cargo": cargo(), "hirsel_clang": [build.llvm_path() / "bin/clang"],
                "goose_clang": ["clang"], "perf": ["perf"]}
    return {
        tool: subprocess.check_output([*command, "--version"], text=True).splitlines()[0]
        for tool, command in versions.items()
    }


def prepare_goose(out, args, env, *, jobs):
    goose_dir = out / "goose-build"
    run(["cmake", "-S", args.goose, "-B", goose_dir, "-DCMAKE_BUILD_TYPE=Release"], env=env)
    run(["cmake", "--build", goose_dir, "--parallel", str(jobs), "--target", "goose"], env=env)
    return goose_dir / "goose"


def place_density_row(board, args):
    out = board.out / "place-density.json"
    bundle = "//:hirsel-toolchain"
    compiler = build_outputs([bundle], threads=4)[bundle] / "hirsel"
    cpus = ",".join(map(str, args.measure_cpus))
    subprocess.run([sys.executable, place_density.__file__, "--hirsel", compiler, "--cpus", cpus,
                    "--lock", args.lock, out], cwd=ROOT, check=True)
    result = receipt.read(out)
    comparison = list(place_density.compare(result, receipt.read(place_density.BASELINE)))
    board.report["place_density"] = dict(place_density.board_sample(result),
                                         comparison=[line for _, line in comparison],
                                         regressed=any(failed for failed, _ in comparison))
    for failed, line in comparison:
        if failed:
            print(f"Place density: {line}", file=sys.stderr)
    board.row(place_density.GOAL, place_density.ROW,
              {"hirsel": {"status": "ok", **result["metrics"]}}, tuple(place_density.METRICS))


def prepare_awfy(out, args, goose, artifact_file, env, *, matched):
    out.mkdir()
    entries = ("harness", "pools", "pool_index") if matched else ("harness",)
    settings = argparse.Namespace(out=out, artifacts=artifact_file, upstream=args.awfy, goose=goose,
                                  rust=args.resid / "bench/v2/t1-awfy/rust", env=env, hello=False,
                                  hirsel_entries=entries, goose_entries=entries)
    for builder in (build.rust, build.goose, build.hirsel):
        builder(settings, matched=matched)
    return out


def run_board(board, args):
    out = board.out
    board.report["commit"] = git(ROOT, "rev-parse", "HEAD")
    validate_sources(args, {"runtime-memory"})
    env = build.capped_env(out, "8G", "2")
    artifact_file = out / "toolchain.json"
    artifacts, tools = prepare_toolchain(artifact_file, env, jobs=2)
    board.report.update(toolchain=artifacts, compiler_versions=compiler_versions(), host=board_host.facts(args.measure_cpus))
    goose = prepare_goose(out, args, env, jobs=2)
    awfy = out / "awfy"
    awfy_ready = board.attempt("AWFY build", lambda: prepare_awfy(
        awfy, args, goose, artifact_file, env, matched=True))
    arena_dir = out / "arena"
    arena_rows = board.attempt("LangArena build", prepare_arena, arena_dir, args, goose, artifacts, env)
    compile_dir = out / "compile"
    compile_ready = board.attempt("Compile preparation", prepare_compile, compile_dir, artifacts, env)
    checkbox_dir = out / "checkbox"
    checkbox_ready = board.attempt("Checkbox build", prepare_checkbox, checkbox_dir, artifacts, env)
    board.attempt("Place density", place_density_row, board, args)
    with hold(REGIMES, args.lock):
        require_comparison_window()
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


def require_comparison_window():
    local = datetime.now().time()
    if time_of_day(2, 45) <= local < time_of_day(4):
        raise ValueError("measurement embargo 02:45–04:00 local time")


def attempt_comparison(evidence, label, operation, *args, **kwargs):
    try:
        return operation(*args, **kwargs)
    except (OSError, ValueError, RuntimeError, KeyError, TypeError, subprocess.SubprocessError) as error:
        print(f"{label}: {error}", file=sys.stderr, flush=True)
        evidence["failures"][label] = str(error)


def anchor_present(commit, anchor):
    return subprocess.run(["git", "merge-base", "--is-ancestor", anchor, commit], cwd=ROOT,
                          check=False).returncode == 0


def tag_samples(samples, commit, mode):
    return {side: sample | {"commit": commit, "mode": mode} for side, sample in samples.items()}


def record_comparison_runtime(evidence, rows):
    for row in rows:
        if row["goal"] in comparison.RUNTIME_SUITES:
            evidence["runtime"][f"{row['goal']}/{row['row']}"] = tag_samples(
                row["samples"], row["commit"], "runtime")


def record_comparison_size(evidence, awfy, upstream, verified):
    result = (awfy_measure.size_board(awfy, upstream) if verified is None
              else awfy_measure.verified_sizes(awfy, sorted(verified)))
    evidence["size"] = {key: result[key] for key in ("strip_version", "strip_flags", "verified_workloads")}
    evidence["size"]["verified_workloads"].sort()
    evidence["size"]["anchor_present"] = anchor_present(evidence["commit"], "97ad0db8")
    evidence["samples"]["size"] = tag_samples(
        {side: {"status": "ok", **image} for side, image in result["images"].items()},
        evidence["commit"], "stripped")


def collect_units(evidence, out, args):
    if "units" not in evidence["families"]:
        return
    producer = ROOT / "bench/units/fair.py"
    if args.units_receipt:
        evidence["units"] = {"path": str(args.units_receipt), "source": "imported"}
    elif not producer.is_file():
        evidence["units"] = {"reason": "bench/units/fair.py is absent at this revision"}
        return
    elif os.environ.get("HIRSEL_BOARD_UNITS_AUTHORIZED") != "1":
        evidence["units"] = {"reason": "owner-qualified CPU allocation and 2 GiB scopes were not authorized"}
        return
    else:
        require_comparison_window()
        destination = out / "units"
        command = ["taskset", "-c", "26-27", sys.executable, producer, "--full", "--cells", "a1,regimes",
                   "--out", destination, "--deadline-seconds", str(args.units_deadline_seconds)]
        with (out / "units.log").open("wb") as log:
            result = subprocess.run(command, cwd=ROOT, stdout=log, stderr=subprocess.STDOUT)
        evidence["units"] = {
            "path": str(destination / "fair.json"), "source": "invoked", "exit_status": result.returncode,
            "command": list(map(str, command)), "log": str(out / "units.log"),
        }
    path = Path(evidence["units"]["path"])
    if path.is_file():
        evidence["units"]["sha256"] = build.sha256(path)


def comparison_report(evidence):
    units_rows = []
    if "units" in evidence["families"]:
        units = evidence.get("units", {})
        try:
            if "path" not in units:
                raise ValueError(units.get("reason", "owner export collection did not complete"))
            path = Path(units["path"])
            if build.sha256(path) != units["sha256"]:
                raise ValueError("owner export changed since collection")
            units_rows = comparison_units.import_rows(path, evidence["commit"])
            units = units | {"status": ("qualified" if all(row["status"] == "ok" for row in units_rows)
                                        else "incomplete")}
        except (OSError, ValueError, KeyError, TypeError, ArithmeticError) as error:
            reason = str(error)
            units = units | {"status": "incomplete", "reason": reason}
            units_rows = [comparison_units.rejected_row(key, reason) for key in comparison_units.expected()]
        evidence = evidence | {"units": units}
    return comparison.summary(evidence, units_rows)


def save_comparison(out, evidence):
    report = comparison_report(evidence)
    write_json(out / "comparison.json", report)
    (out / "comparison.md").write_text(comparison.markdown(report))
    return report


def run_comparison(out, args):
    families = args.comparison_family or comparison.FAMILIES
    commit = git(ROOT, "rev-parse", "HEAD")
    evidence = {
        "version": comparison.VERSION, "commit": commit, "families": families, "pins": PINS,
        "build_cpus": sorted(args.build_cpus), "measure_cpus": args.measure_cpus,
        "build_jobs": 4, "build_memory_limit_bytes": 8 << 30, "measurement_memory_limit_bytes": 2 << 30,
        "runtime": {}, "samples": {}, "failures": {}, "command": list(sys.argv),
        "started": datetime.now().astimezone().isoformat(),
    }
    selected = set(families)
    try:
        validate_sources(args, selected)
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        evidence["failures"]["Input verification"] = str(error)
    else:
        if selected - {"units"}:
            attempt_comparison(evidence, "Board measurement", measure_comparison_board, evidence, out, args, selected)
        # The owner has separate CPU admission and takes both locks itself.
        attempt_comparison(evidence, "Units collection", collect_units, evidence, out, args)
    evidence["finished"] = datetime.now().astimezone().isoformat()
    report = save_comparison(out, evidence)
    print(out / "comparison.md", flush=True)
    return 0 if report["selection_complete"] else 1


def measure_comparison_board(evidence, out, args, selected):
    require_comparison_window()
    # Keep the Cargo shim in the board's process-tree scope.
    env = build.capped_env(out, "8G", "4") | {"KILN_CARGO_SCOPE": "hirsel-board"}
    artifact_file = out / "toolchain.json"
    artifacts, tools = prepare_toolchain(artifact_file, env, jobs=4)
    evidence["toolchain"] = artifacts
    needs_peers = bool(selected - {"units", "checkbox"})
    goose = prepare_goose(out, args, env, jobs=4) if needs_peers else None
    if needs_peers:
        evidence["compiler_versions"] = compiler_versions()
    runtime = "runtime-memory" in selected
    size = "image-size" in selected
    awfy_dir, arena_dir, checkbox_dir = (out / name for name in ("awfy", "arena", "checkbox"))
    board = Board(out, evidence["commit"], persist=False)
    awfy = attempt_comparison(evidence, "AWFY build", prepare_awfy, awfy_dir, args, goose,
                              artifact_file, env, matched=runtime) if runtime or size else None
    arena_ready = attempt_comparison(evidence, "LangArena build", prepare_arena,
                                     arena_dir, args, goose, artifacts, env, jobs=4) if runtime else None
    checkbox = attempt_comparison(evidence, "Checkbox build", prepare_checkbox,
                                  checkbox_dir, artifacts, env, jobs=4) if "checkbox" in selected else None
    if "checkbox" in selected:
        evidence["checkbox_anchor_present"] = anchor_present(evidence["commit"], "83faae1e")
    recipes = {}
    for family in comparison.BUILD_ROWS:
        if family in selected:
            for _, _, recipe, mode in comparison.BUILD_ROWS[family]:
                recipes.setdefault(recipe, []).append(mode)
    compiled = {}
    if recipes:
        deps = out / "compile-dependencies"
        deps.mkdir()
        attempt_comparison(evidence, "Publish compile products", publish_std, deps, artifacts, env)
        if "Publish compile products" not in evidence["failures"]:
            for recipe in recipes:
                compiled[recipe] = attempt_comparison(
                    evidence, f"{recipe} preparation", prepare_comparison_compile,
                    out / recipe, recipe, args, artifacts, goose, deps / "packages", env)
    with comparison_measurement_hold(args, evidence):
        verified = None
        if awfy and runtime:
            verified = attempt_comparison(evidence, "AWFY original", runtime_rows, board, "awfy", awfy_dir,
                                          args.awfy, tools, extended=True)
            attempt_comparison(evidence, "AWFY pool/index", runtime_rows, board, "pool", awfy_dir,
                               args.awfy, tools, extended=True)
        if arena_ready:
            attempt_comparison(evidence, "LangArena runtime", runtime_rows, board, "arena", arena_dir,
                               None, tools, config=arena_dir / "run.js", extended=True)
        record_comparison_runtime(evidence, board.report["rows"])
        if awfy and size and (not runtime or sorted(verified or []) == sorted(EXPECTED["AWFY original"])):
            attempt_comparison(evidence, "Image size", record_comparison_size,
                               evidence, awfy_dir, args.awfy, verified)
        for recipe, prepared in compiled.items():
            if prepared:
                attempt_comparison(evidence, f"{recipe} measurement", measure_comparison_compile,
                                   evidence, out / recipe, recipe, prepared, args, recipes[recipe])
        if checkbox:
            os.sched_setaffinity(0, args.measure_cpus[:2])
            attempt_comparison(evidence, "Checkbox memory", checkbox_rows, board, checkbox_dir, checkbox,
                               *args.measure_cpus[:2], extended=True)
            for row in board.report["rows"]:
                if row["goal"] == "Checkbox":
                    mode = row["row"].split(",")[0]
                    evidence["samples"][mode] = tag_samples(row["samples"], row["commit"], mode)
    evidence["failures"].update(board.report["failures"])


@contextmanager
def comparison_measurement_hold(args, evidence):
    with hold(REGIMES, args.lock):
        require_comparison_window()
        evidence["hold"] = {"started": datetime.now().astimezone().isoformat(),
                            "locks": [str(REGIMES), str(args.lock)]}
        os.sched_setaffinity(0, {args.measure_cpus[0]})
        try:
            yield
        finally:
            os.sched_setaffinity(0, args.build_cpus)
            evidence["hold"]["finished"] = datetime.now().astimezone().isoformat()


def validate_baseline(path, report):
    rows = rows_by_label(report)
    if (report["complete"] is not True
            or rows.keys() != {(goal, name) for goal, names in EXPECTED.items() for name in names}
            or any(row["status"] != "ok" for row in rows.values()) or inactive_counters(report)
            or receipt.rows_from(path).keys() != set(instruction_keys())):
        raise ValueError(f"{path}: a baseline requires all current rows and fully active counters")
    if report["size_measurement"]["verified_workloads"] != sorted(EXPECTED["AWFY original"]):
        raise ValueError(f"{path}: size requires all original-14 workloads")
    board_size.size_changes(report, report)
    for mode, metric in CHECKBOX_METRICS.items():
        row = rows["Checkbox", f"{mode}, {CHECKBOX_CLIENTS} clients"]
        list(checkbox_changes(row, row, metric))


def rebaseline(run, base=None, expected_baseline=None):
    path = run / "board.json"
    if not path.is_file():
        path = run / "board/board.json"
    path = path.resolve()
    report = receipt.read(path)
    exit_status = report["exit_status"]
    changed = set()
    if exit_status == receipt.REFUSED and report["complete"]:
        changed = board_baseline.refused_identity_fields(report, set(instruction_keys()), board_size.SIZE_IDENTITY)
    if report["complete"] is not True or report["failures"] or (exit_status != 0 and not changed):
        raise ValueError(f"{path}: rebaseline requires a complete run with clean gates or identity-only refusals")
    validate_baseline(path, report)
    board_baseline.promote(path, base, expected_baseline)
    reason = f"measurement identity changed: {', '.join(sorted(changed))}" if changed else "it is a complete, clean board"
    print(f"Promoted {path} ({report['commit']}) because {reason}.")
    return 0


def main():
    if sys.argv[1:2] == ["render-comparison"]:
        parser = argparse.ArgumentParser(prog=f"{sys.argv[0]} render-comparison",
                                         description="Validate and render a comparison evidence directory")
        parser.add_argument("run", type=Path)
        parser.add_argument("--revision", help="require this exact candidate commit")
        args = parser.parse_args(sys.argv[2:])
        try:
            evidence = receipt.read(args.run / "comparison.json")
            if not re.fullmatch(r"[0-9a-f]{40}", evidence["commit"]) or (
                    args.revision and evidence["commit"] != args.revision):
                raise ValueError("comparison revision mismatch")
            if evidence["pins"] != PINS:
                raise ValueError("comparison peer pins mismatch")
            report = comparison_report(evidence)
            print(comparison.markdown(report), end="")
            return 0 if report["selection_complete"] else 1
        except (*receipt.INVALID_RECEIPT, ArithmeticError) as error:
            print(f"comparison refused: {error}", file=sys.stderr)
            return receipt.REFUSED
    if sys.argv[1:2] == ["rebaseline"]:
        parser = argparse.ArgumentParser(prog=f"{sys.argv[0]} rebaseline",
                                         description="Promote a complete board with clean gates or identity-only refusals")
        parser.add_argument("run", type=Path)
        parser.add_argument("--base", type=Path)
        parser.add_argument("--expected-baseline", type=lambda value: Path(value) if value else "",
                            help="compared board.json path, or an empty string for no previous baseline")
        args = parser.parse_args(sys.argv[2:])
        try:
            return rebaseline(args.run, args.base, args.expected_baseline)
        except (*receipt.INVALID_RECEIPT, ArithmeticError) as error:
            print(f"rebaseline refused: {error}", file=sys.stderr)
            return receipt.REFUSED

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("out", type=Path)
    parser.add_argument("--baseline", type=lambda path: Path(path).resolve(),
                        help="prior complete board.json for instruction, size and checkbox gates")
    for name in PINS:
        parser.add_argument(f"--{name}", type=lambda path: Path(path).resolve(),
                            help=f"clean checkout at {PINS[name]}")
    parser.add_argument("--comparison", action="store_true", help="run the extended comparison board")
    parser.add_argument("--comparison-family", action="append", choices=comparison.FAMILIES,
                        help="measure a selected family; repeat to select several")
    parser.add_argument("--units-receipt", type=lambda path: Path(path).resolve(),
                        help="use a qualified same-revision fair.json")
    parser.add_argument("--units-deadline-seconds", type=int, help="owner producer measurement budget")
    add_cpu_sets(parser)
    parser.add_argument("--lock", type=Path, default=LOCK)
    args = parser.parse_args()
    validate_cpu_sets(parser, args, width=2, measure_affinity=measurement_affinity(REGIMES, args.lock))
    families = args.comparison_family or comparison.FAMILIES
    peers = required_peers(set(families)) if args.comparison else set(PINS)
    for peer in PINS:
        if peer in peers and getattr(args, peer) is None:
            parser.error(f"--{peer} is required for these rows")
    if args.comparison:
        if args.baseline:
            parser.error("--baseline applies to the nightly board")
        if len(set(families)) != len(families):
            parser.error("select each comparison family once")
        if (args.units_receipt or args.units_deadline_seconds is not None) and "units" not in families:
            parser.error("units options require the units family")
        if args.units_deadline_seconds is not None and args.units_deadline_seconds < 1:
            parser.error("--units-deadline-seconds must be positive")
        if (set(args.build_cpus) != set(range(8, 12)) or set(args.measure_cpus) != set(range(16, 20))):
            parser.error("comparison uses build CPUs 8-11 and measurement CPUs 16-19")
        if args.lock.resolve() != LOCK.resolve():
            parser.error("comparison requires the shared hirsel-board.lock")
        if args.units_deadline_seconds is None:
            args.units_deadline_seconds = 14400
    elif args.comparison_family or args.units_receipt or args.units_deadline_seconds is not None:
        parser.error("comparison options require --comparison")
    os.sched_setaffinity(0, args.build_cpus)
    out = args.out.resolve()
    if out.is_relative_to(ROOT):
        parser.error("use a new evidence directory outside the checkout")
    out.mkdir(parents=True)
    if args.comparison:
        return run_comparison(out, args)
    board = Board(out, None)
    board.save()
    board.attempt("Board", run_board, board, args)
    return board.finish(args.baseline)


if __name__ == "__main__":
    raise SystemExit(main())
