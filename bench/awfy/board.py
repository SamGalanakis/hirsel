#!/usr/bin/env python3
"""Build and run the matched Hirsel goal board. Keep outputs outside the checkout."""
import argparse
from contextlib import contextmanager, redirect_stdout
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
import time

import arena
import build
import measure
import receipt
from receipt import measurement_slot, write as write_json

ROOT = build.ROOT
CAP = ROOT / "tools/capped"
LOCK = Path("/run/user/1000/hirsel-board.lock")
BUILD_CPUS = {20, 21}
MEASURE_CPUS = {10, 11}
PINS = {
    "awfy": "f35aed0a61171594804a44efe9d75f1f35360700",
    "resid": "0ab89f373829776d3ea285bfb25f3f1505d7be35",
    "goose": "d5ae5bbda990923fa4129f969c7f853c20d4d1e6",
    "langarena": "3c9f84ddaeec834c0631747cb789e8cfd602f993",
}
COUNTERS = ("instructions", "cycles")
EXPECTED = measure.EXPECTED
NOTES = (
    "Instructions and cycles count user-space CPU work; seconds measures elapsed time. "
    "Hirsel checks bounds and overflow and admits bytes. Certified images elide Step charging. "
    "Rust and Goose have no Hirsel replay, Host or budget machinery. The ports use different "
    "libraries and layouts. Each AWFY port checks the reference result internally; LangArena "
    "checks the same one-iteration Rust oracle.",
    "The size row counts each complete original-14 harness once after GNU strip. "
    "The compile rows leave dependencies built, use one Cargo codegen unit with release "
    "incremental enabled, and apply the same unused binding for the edit.",
    "Checkbox runs use the same checked wire driver and durable workload on both servers. "
    "Throughput is toggles/s including connection and convergence time; higher is better. "
    "RSS is sampled with the requested sessions live. Goose has no compile or checkbox peer here.",
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
    """Stay inside tools/capped instead of starting another, wider user scope."""
    return env | {"KILN_CARGO_CGROUP": "off"}


def release_flags(artifacts):
    return ["--backend", "llvm", "--release", "--runtime", artifacts["runtime"],
            "--cc", build.llvm_path() / "bin/clang", "--json"]


def native(path):
    result = json.loads(path.read_text())
    if not result.get("accepted") or result.get("stopped"):
        raise ValueError(f"build not accepted: {path}")
    return result["native"].get("step_charging_elided", False)


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


class Board:
    def __init__(self, out, commit):
        self.out = out
        self.report = {"commit": commit, "pins": PINS, "notes": NOTES, "rows": [], "failures": {},
                       "instruction_receipt": receipt.make_receipt(
                           f"{suite}/{name}" for suite, goal in measure.SUITES.items() for name in EXPECTED[goal]
                       )}

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
                 "| Goal | Row | Metric | H/R | H/G | Certified | Commit |",
                 "| --- | --- | --- | ---: | ---: | --- | --- |"]
        for row in self.report["rows"]:
            for metric in row["metrics"]:
                cells = []
                for side in ("rust", "goose"):
                    value = row["ratios"][side].get(metric)
                    cells.append(f"{value:.4f}×" if value is not None else "n/a")
                certified = {True: "yes", False: "no", None: "unknown"}[row["certified"]]
                lines.append("| " + " | ".join((row["goal"], row["row"], metric, *cells,
                                                  certified, row["commit"][:8])) + " |")
        if self.report["failures"]:
            lines += ["", "## Incomplete samples", ""]
            lines += [f"- {label}: {reason}" for label, reason in self.report["failures"].items()]
        missing = [f"{row['goal']}/{row['row']}" for row in expected if row["status"] == "missing"]
        if missing:
            lines += ["", "## Missing rows", "", *[f"- {label}" for label in missing]]
        comparison = self.report.get("instruction_comparison")
        if comparison:
            lines += ["", "## Nightly instruction comparison", "", comparison["status"], "",
                      *comparison["details"]]
        (self.out / "board.md").write_text("\n".join(lines) + "\n")

    def finish(self, baseline=None):
        self.save()
        print(self.out / "board.md", flush=True)
        if not self.report["complete"]:
            return 1
        status, details = "No prior complete receipt", []
        if baseline is not None:
            output = StringIO()
            try:
                with redirect_stdout(output):
                    breached = receipt.compare(baseline, self.out / "board.json", Decimal(2))
                status = "Instruction regression" if breached else "Within +2% on every row"
                details = output.getvalue().splitlines()
            except (KeyError, OSError, ValueError, TypeError, AttributeError, re.error) as error:
                status, details = "Incomparable", [str(error)]
        self.report["instruction_comparison"] = {"status": status, "details": details}
        self.save()
        return 3 if status == "Instruction regression" else 0

    def row(self, goal, name, certified, samples, fields):
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
                sample["status"] = status
            if status != "ok":
                self.report["failures"][f"{label}/{side}"] = status
                complete = False
        self.report["rows"].append({"goal": goal, "row": name, "certified": certified,
                                    "status": "ok" if complete else "failed",
                                    "commit": self.report["commit"], "samples": samples,
                                    "metrics": fields, "ratios": ratios(samples, fields)})
        self.save()

    def attempt(self, label, operation, *args):
        try:
            return operation(*args)
        except (ValueError, OSError, RuntimeError, KeyError, subprocess.SubprocessError) as error:
            print(f"{label}: {error}", file=sys.stderr, flush=True)
            self.report["failures"][label] = str(error)
            self.save()


def runtime_rows(board, suite, directory, upstream, tools):
    goal = measure.SUITES[suite]
    rows = measure.runtime_cases(suite, directory, upstream)
    fixture = receipt.runtime_fixture(suite, directory, upstream)
    samples_dir = board.out / "runtime" / goal
    samples_dir.mkdir(parents=True)
    verified = []
    for index, (name, commands) in enumerate(rows):
        row = receipt.runtime_case(suite, name, directory, *commands["hirsel"], fixture, tools)
        certified = row["certified"]
        samples = {}
        order = ("rust", "hirsel", "goose") if index % 2 == 0 else ("goose", "hirsel", "rust")
        for side in order:
            command, marker = commands[side]
            target = samples_dir / f"{name.replace('::', '-')}-{side}"
            env = build.capped_env(board.out)
            if side == "hirsel":
                sample = receipt.observe(row, target, env, cycles=True)
                board.report["instruction_receipt"]["rows"].append(row)
                samples[side] = sample
                continue
            sample = measure.core_measure.sample(command, target, env=env, cycles=True, cwd=directory)
            output = (target / "stdout").read_bytes().splitlines(keepends=True)
            sample["verified"] = sample["status"] in ("ok", "missing-counter") and any(
                re.fullmatch(marker, line) for line in output)
            if sample["status"] == "ok" and not sample["verified"]:
                sample["status"] = "wrong-result"
            samples[side] = sample
        if all(sample["verified"] for sample in samples.values()):
            verified.append(name)
        board.row(goal, name, certified, samples, COUNTERS)
    return verified


def awfy_rows(board, out, upstream, tools):
    originals = [(name, commands) for name, commands in measure.rows(out, False, upstream) if name != "Hello"]
    if sorted(dict(originals)) != sorted(EXPECTED["AWFY original"]) or len(originals) != 14:
        raise ValueError("the original board requires 14 distinct AWFY rows")
    verified = runtime_rows(board, "awfy", out, upstream, tools)
    if len(verified) == len(originals):
        result = measure.verified_sizes(out, verified)
        board.row("Size", "AWFY original-14 harness", result["hirsel_certified"][verified[0]],
                  {side: {"status": "ok", "bytes": size}
                   for side, size in result["stripped_bytes"].items()}, ("bytes",))
    else:
        board.report["failures"]["Size"] = "not all original harness results verified"
        board.save()
    pools = list(measure.matched_pool_rows(out, upstream))
    if sorted(dict(pools)) != sorted(EXPECTED["AWFY pool/index"]) or len(pools) != 8:
        raise ValueError("the pool board requires eight distinct AWFY rows")
    runtime_rows(board, "pool", out, upstream, tools)


def cargo(*args):
    return ["cargo", "+1.99.0", *args]


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
    return (out / "rust", out / "helper.hirsel", hirsel, cargo("build", "-j1", *compile_profile(target)),
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
    run(cargo("build", "-j2", *compile_profile(target)), cwd=rust, env=cargo_scope(env))
    run(cargo("clean", "--release", "--target-dir", target, "-p", "benchmarks"), cwd=rust, env=cargo_scope(env))

    prepare_hirsel_compile(out, artifacts, env)
    return compile_commands(out)


def prepare_hirsel_compile(out, artifacts, env):
    suite = load("arena_suite", ROOT / "core/langarena/run.py")
    packages = out / "packages"
    run([artifacts["compiler"], "std-product", "--store", packages], env=env)
    helper = out / "helper.hirsel"
    shutil.copyfile(ROOT / "core/langarena/helper.hirsel", helper)
    manifest = suite.inputs(ROOT, "helper", helper)
    for package in manifest["packages"]:
        package["sources"] = [entry for entry in package["sources"] if len(entry["modules"]) == 1]
    source = out / "suite.hirsel"
    source.write_text(suite.suite_source())
    manifest["packages"][-1]["sources"].append({"file": str(source), "modules": ["suite"], "signatures": False})
    write_json(out / "inputs.json", manifest)
    (out / "pinned-compiler").symlink_to(artifacts["compiler"])
    (out / "pinned-runtime.a").symlink_to(artifacts["runtime"])


def compile_rows(board, out, prepared, fixture, tools):
    rust, helper, hirsel, cargo_build, exe, rust_exe = prepared
    env = build.capped_env(out) | {"LD_LIBRARY_PATH": str(build.llvm_path() / "lib")}
    config = out / "fixture.json"
    shutil.copyfile(fixture, config)
    task = next(row for row in json.loads(config.read_text()) if row["name"] == "CLBG::Spectralnorm")
    name, expected = task["name"], task["checksum"]
    for mode in ("cold", "edit"):
        if mode == "edit":
            for path, signature, edit in ((helper, "pub fn config_i64(", "\n    let _ = 0u8"),
                                           (rust / "src/main.rs", "fn config_i64(", "\n    let _ = 0u8;")):
                body = path.read_text()
                offset = body.index("{", body.index(signature)) + 1
                path.write_text(body[:offset] + edit + body[offset:])
        samples = {}
        certified = None
        label = f"LangArena {mode}"
        fixture_id = receipt.fixture_identity(out / "inputs.json", config)
        provenance = build.native_provenance(hirsel, out / "inputs.json", cwd=out)
        commands = [("hirsel", hirsel, out, exe), ("rust", cargo_build, rust, rust_exe)]
        if mode == "edit":
            commands.reverse()
        for side, command, cwd, executable in commands:
            target = out / f"{mode}-{side}"
            sample = measure.core_measure.sample(command, target, env=cargo_scope(env), cycles=True, cwd=cwd)
            samples[side] = sample
            if sample["status"] not in ("ok", "missing-counter"):
                continue
            if side == "hirsel":
                native_path = target / "stdout"
                certified = native(native_path)
                report = build.write_native_report(executable, native_path.read_bytes(), provenance,
                                                   path=target / "build.json")
                record = receipt.build_record(report)
            verify = subprocess.run([CAP, executable, config.name, name], env=env, cwd=out,
                                    capture_output=True, text=True)
            (target / "verification").write_text(verify.stdout + verify.stderr)
            marker = re.escape(name) + (f" checksum={expected} expected={expected} status=OK" if side == "hirsel"
                                       else r": OK in [0-9.]+s")
            sample["verified"] = verify.returncode == 0 and any(
                re.fullmatch(marker, line) for line in verify.stdout.splitlines())
            if not sample["verified"]:
                sample["status"] = "wrong-result"
            elif sample["status"] == "ok" and not sample["seconds"]:
                sample["status"] = "missing-duration"
            elif sample["status"] == "ok" and not all(
                receipt.counter_ready(sample, f"{counter}:u") for counter in COUNTERS
            ):
                sample["status"] = "inactive-counter"
            if side == "hirsel":
                board.report["instruction_receipt"]["rows"].append({
                    "key": f"compile/{label}", "suite": "compile", "mode": mode, "certified": certified,
                    "flags": provenance["flags"], "argv": list(map(str, command)),
                    "build_record": record, "image_sha256": record["image_sha256"],
                    "affinity": sorted(os.sched_getaffinity(0)), "marker": marker + "\n",
                    "stdout": verify.stdout, "sample": sample, **fixture_id, **tools,
                })
        board.row("Compile", label, certified, samples, ("seconds", *COUNTERS))
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
def private_server(command, address, out, env):
    pidfile = out / "server.pid"
    boot = ["sh", "-c", 'echo "$$" > "$1"; shift; exec "$@"', "checkbox-board", pidfile]
    with (out / "server.log").open("wb") as log:
        with subprocess.Popen(["taskset", "-c", "10", CAP, *boot, *command],
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


def checkbox_rows(board, out, prepared):
    checkbox, executable, address, wants, tools = prepared
    env = build.capped_env(out)
    overrides = ["--per-ip-sessions", "20000", "--per-ip-new-principals", "20000", "--principal-rate", "1000000000"]
    clients, toggles = 64, 200
    for index, (mode, metric) in enumerate((("converge", "toggles_per_second"),
                                            ("memory-subscribed", "server_rss_bytes"),
                                            ("memory-unsubscribed", "server_rss_bytes"))):
        samples = {}
        for side in (("hirsel", "rust") if index % 2 == 0 else ("rust", "hirsel")):
            directory = out / f"{mode}-{side}"
            directory.mkdir()
            if side == "hirsel":
                command = checkbox.command(executable, address, overrides)
            else:
                command = [tools / "wire-reference", "--listen", address, "--wal", directory / "wire.wal",
                           "--n-place", checkbox.N_PLACE, "--wants", wants, *overrides]
            try:
                with private_server(command, address, directory, env) as pid:
                    load_command = [tools / "wire-load", mode, "--url", f"ws://{address}/hirsel",
                                    "--wants", wants, "--clients", str(clients)]
                    load_command += ["--toggles", str(toggles), "--pace", "1000"] if mode == "converge" else ["--server-pid", str(pid)]
                    started = time.monotonic()
                    result = subprocess.run(["taskset", "-c", "11", CAP, *load_command], env=env,
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
                if side == "hirsel":
                    (out / "data").rename(directory / "data")
                    (out / "data").mkdir()
        board.row("Checkbox", f"{mode}, {clients} clients", native(out / "build.json"), samples, (metric,))


def prepare_arena(out, args, compiler, artifacts, env):
    out.mkdir()
    rust = args.langarena / "rust"
    target = out / "rust-target"
    run(cargo("build", "--release", "--locked", "--offline", "-j2", "--target-dir", target),
        cwd=rust, env=cargo_scope(env))
    shutil.copy2(target / "release/benchmarks", out / "rust")
    build.goose_executable(compiler, args.goose / "bench/kostya/goose/src/main.goose", out / "goose", env)
    rows = [dict(row, iterations=1, warmup_iterations=0, checksum=0)
            for row in json.loads((args.langarena / "run.js").read_text())]
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

    env = build.capped_env(out, "8G", "2") | {"LD_LIBRARY_PATH": str(build.llvm_path() / "lib")}
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
    with measurement_slot(LOCK):
        os.sched_setaffinity(0, {min(MEASURE_CPUS)})
        if awfy_ready:
            board.attempt("AWFY measurement", awfy_rows, board, awfy, args.awfy, tools)
        if arena_rows:
            def sample_arena():
                arena.oracle(arena_dir, arena_dir / "rust", arena_rows)
                return runtime_rows(board, "arena", arena_dir, None, tools)
            verified = board.attempt("LangArena runtime", sample_arena)
        else:
            verified = None
        if compile_ready and verified and "CLBG::Spectralnorm" in verified:
            board.attempt("Compile measurement", compile_rows, board, compile_dir, compile_ready,
                          arena_dir / "one-iteration.json", tools)
        elif compile_ready:
            board.report["failures"]["Compile measurement"] = "no verified Spectralnorm fixture"
        if checkbox_ready:
            os.sched_setaffinity(0, MEASURE_CPUS)
            board.attempt("Checkbox measurement", checkbox_rows, board, checkbox_dir, checkbox_ready)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("out", type=Path)
    parser.add_argument("--baseline", type=Path, help="prior complete board.json for +2% instruction triage")
    for name in PINS:
        parser.add_argument(f"--{name}", type=lambda path: Path(path).resolve(), required=True,
                            help=f"clean checkout at {PINS[name]}")
    args = parser.parse_args()
    os.sched_setaffinity(0, BUILD_CPUS)
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
