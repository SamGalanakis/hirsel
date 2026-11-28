#!/usr/bin/env python3
"""Build and run native Mac CI tests and stage their cross-built inputs."""

import argparse
from concurrent.futures import ThreadPoolExecutor, as_completed
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile

SUITES = {
    "hirsel-core-rt": ("--lib", (
        "tls::tests::platform::system_trust_laws",
    )),
    "hirsel-llvm": ("--lib", (
        "core::target_tests::linux_c_results_execute_with_mixed_fields_padding_and_hfas",
        "core::target_tests::checked_arithmetic_preserves_status_context_and_cause_copies",
        "core::tests::cancelled_child_inside_isolate_passes_through_and_refunds_cell",
        "core::tests::c_results_and_indirect_calls_match_evaluator",
    )),
    "hirsel-cranelift": ("--test=core_native", (
        "slots_loops_calls_and_c_struct_returns_match_evaluator",
    )),
}

ROOT = Path(__file__).resolve().parents[2]
DRIVER = "hirsel-core-driver"
DRIVER_DIR = ROOT / "crates" / DRIVER
DRIVER_WORKERS = 2
LINUX_ONLY = "launcher::laws::cgroup_lifecycle"
LINK_KIT = "root//tools/buck2/native-toolchain:kit"
DRIVER_RUNTIMES = {
    "HIRSEL_CORE_RT_ARCHIVE": "hirsel-core-rt-archives",
    "HIRSEL_CORE_RT_RECORD_ARCHIVE": "hirsel-core-rt-record-archives",
    "HIRSEL_CORE_RT_OBSERVED_ARCHIVE": "hirsel-core-rt-observed-archives",
    "HIRSEL_CORE_RT_OBSERVED_RECORD_ARCHIVE": "hirsel-core-rt-observed-record-archives",
    "HIRSEL_CORE_RT_PROF_ARCHIVE": "hirsel-core-rt-prof-archives",
    "HIRSEL_CORE_RT_PROF_RECORD_ARCHIVE": "hirsel-core-rt-prof-record-archives",
}


def cargo_executable(command, env, *, test):
    with subprocess.Popen([*command, "--message-format=json"], stdout=subprocess.PIPE,
                          text=True, env=env) as cargo:
        candidates = []
        for line in cargo.stdout:
            event = json.loads(line)
            match event["reason"]:
                case "compiler-message":
                    print(event["message"]["rendered"] or "", end="", flush=True)
                case "compiler-artifact":
                    if event["profile"]["test"] == test and event["executable"]:
                        candidates.append(event["executable"])
        if cargo.wait():
            raise SystemExit(cargo.returncode)
        if len(candidates) != 1:
            raise SystemExit(f"{command}: expected one executable, saw {candidates}")
        return candidates[0]


def build(destination):
    harnesses = {}
    for package, (selection, _) in SUITES.items():
        command = ["cargo", "test", "--locked", "--no-run", "-p", package, selection]
        env = os.environ.copy()
        if package == "hirsel-core-rt":
            command.append("--target=aarch64-apple-darwin")
            env["RUSTC_BOOTSTRAP"] = "1"
            env["RUSTFLAGS"] = f"{env.get('RUSTFLAGS', '')} -Zsanitizer=address"
        harnesses[package] = cargo_executable(command, env, test=True)
    destination.write_text(json.dumps(harnesses) + "\n")


def run(source):
    binaries = json.loads(source.read_text())
    failures = []
    for package, (_, cases) in SUITES.items():
        for case in cases:
            print(f"{package}: {case}", flush=True)
            try:
                result = subprocess.run([binaries[package], "--exact", case, "--nocapture"],
                                        stdout=subprocess.PIPE, text=True, timeout=300)
                print(result.stdout, end="", flush=True)
                if result.returncode == 0 and "test result: ok. 1 passed;" in result.stdout:
                    continue
            except (OSError, subprocess.SubprocessError) as error:
                print(error, flush=True)
            failures.append(case.rsplit("::", 1)[-1])
    with open(os.environ["GITHUB_OUTPUT"], "a") as output:
        print(f"failures={json.dumps(failures)}", file=output)
    return bool(failures)


def export_driver_inputs(destination):
    targets = {name: f"root//:{name}[aarch64-apple-darwin]"
               for name in DRIVER_RUNTIMES.values()}
    built = json.loads(subprocess.check_output(
        [ROOT / ".buck2/bin/buck2", "build", "-j4", "--show-json-output",
         LINK_KIT, *targets.values()], cwd=ROOT, text=True,
    ))
    destination.mkdir(parents=True)
    shutil.copytree(ROOT / built[LINK_KIT], destination / "kit")
    for name, target in targets.items():
        shutil.copyfile(ROOT / built[target], destination / f"{name}.a")


def build_driver(destination):
    options = ["--locked", "--profile", "checked", "-p", DRIVER]
    env = os.environ.copy()
    harness = cargo_executable(["cargo", "test", *options, "--lib", "--no-run"], env, test=True)
    compiler = cargo_executable(["cargo", "build", *options, "--bin", "hirsel"], env, test=False)
    subprocess.run(
        ["cargo", "rustc", "--locked", "--profile", "checked", "-p", "hirsel-core-rt", "--lib",
         "--features", "testing,memcheck,charge-observation,record", "--crate-type", "staticlib"],
        check=True, env=env,
    )
    scripted_record = Path(compiler).with_name("libhirsel_core_rt.a").resolve(strict=True)
    destination.write_text(json.dumps({
        "harness": harness, "compiler": compiler, "scripted_record": str(scripted_record),
    }) + "\n")


def seed_dev_cache(compiler, cache, env):
    testdata = DRIVER_DIR / "src/testdata"
    with tempfile.TemporaryDirectory(prefix="hirsel-ci-dev-") as directory:
        work = Path(directory)
        subprocess.run(["/bin/sh", testdata / "dev_fixture.sh", ROOT / "core", testdata, work],
                       check=True)
        dev = work / "runner/.hirsel-dev"
        (dev / "stores/1").mkdir(parents=True)
        source = (testdata / "dev_place.hirsel").read_text()
        for release, backend in ((0, "cranelift"), (1, "cranelift"), (2, "cranelift"),
                                 (3, "cranelift"), (2, "llvm")):
            (work / "runner/dev_place.hirsel").write_text(source.replace(
                "DEV_FIXTURE_RELEASE: u64 = 0u64", f"DEV_FIXTURE_RELEASE: u64 = {release}u64",
            ))
            subprocess.run(
                [compiler, "build", work / "runner", "--store", dev / "compiler",
                 "--dir", dev / "stores/1", "--backend", backend, "--source-core", "--source-std",
                 "--runtime", env["HIRSEL_CORE_RT_ARCHIVE"], "--libraries", work / "libraries",
                 "--limit", "1610612736", "--grant", "Clock", "--grant", "Random",
                 "--grant", "Tasks", "--grant", "Process", "--grant", "Stderr",
                 "--grant", "Net=Listen 127.0.0.1/32 0-65535", "--output", work / "place"],
                check=True, cwd=work, env=dict(env, XDG_CACHE_HOME=str(cache)),
            )
        shutil.move(dev / "compiler", Path(cache) / "compiler")


def driver_inventory(harness, env, *flags):
    listed = subprocess.check_output([harness, "--list", *flags], cwd=DRIVER_DIR, env=env, text=True)
    rows = [line for line in listed.splitlines() if line]
    names = {line.removesuffix(": test") for line in rows[:-1] if line.endswith(": test")}
    summary = re.fullmatch(r"(\d+) tests?, 0 benchmarks", rows[-1]) if rows else None
    if not summary or int(summary[1]) != len(names) or len(names) != len(rows) - 1:
        raise SystemExit(f"Invalid {DRIVER} inventory: {listed!r}")
    return names


def driver_cases(harness, env):
    all_cases = driver_inventory(harness, env)
    ignored = driver_inventory(harness, env, "--ignored")
    if not ignored <= all_cases or not all_cases - ignored or LINUX_ONLY in all_cases:
        raise SystemExit(f"Unexpected Mac driver inventory. {LINUX_ONLY} must be Linux-only, "
                         "and the default cases must be a nonempty partition.")
    print(f"Platform exclusion: {LINUX_ONLY} (Linux cgroups)")
    print(f"::group::{len(ignored)} explicitly ignored driver tests")
    for case in sorted(ignored):
        print(case)
    print("::endgroup::", flush=True)
    return sorted(all_cases - ignored)


def run_driver_case(harness, env, case):
    command = [harness, "--exact", case, "--test-threads=1"]
    with tempfile.TemporaryFile(mode="w+t", encoding="utf-8", errors="replace") as log:
        try:
            result = subprocess.run(command, cwd=DRIVER_DIR, env=env, stdout=log,
                                    stderr=subprocess.STDOUT, timeout=300)
            status = result.returncode
        except subprocess.TimeoutExpired:
            status = "timeout"
        log.seek(0)
        output = log.read()
    summary = re.findall(r"^test result: (.*)$", output, re.MULTILINE)
    if status == 0 and len(summary) == 1 and re.fullmatch(
            r"ok\. 1 passed; 0 failed; 0 ignored; 0 measured; \d+ filtered out;.*", summary[0]):
        return None
    return f"{case} (exit {status})\n{output}"


def run_driver(source, inputs):
    binaries = json.loads(source.read_text())
    inputs = inputs.resolve()
    kit = inputs / "kit"
    (kit / "manifest.json").resolve(strict=True)
    scripted_record = Path(binaries["scripted_record"]).resolve(strict=True)
    env = dict(os.environ, HIRSEL_JOBS="1", HIRSEL_LINK_KIT=str(kit),
               HIRSEL_DEV_TEST_DRIVER=binaries["compiler"],
               HIRSEL_CORE_RT_SCRIPTED_RECORD_ARCHIVE=str(scripted_record))
    for variable, name in DRIVER_RUNTIMES.items():
        env[variable] = str((inputs / f"{name}.a").resolve(strict=True))
    cases = driver_cases(binaries["harness"], env)
    with tempfile.TemporaryDirectory(prefix="hirsel-ci-dev-cache-") as cache:
        env["HIRSEL_DEV_TEST_CACHE"] = cache
        seed_dev_cache(binaries["compiler"], cache, env)
        failures = 0
        # Some tests launch the driver or mutate process state. Give each
        # case its own harness and keep APFS contention below the host CPU count.
        with ThreadPoolExecutor(max_workers=DRIVER_WORKERS) as pool:
            jobs = {pool.submit(run_driver_case, binaries["harness"], env, case): case for case in cases}
            for job in as_completed(jobs):
                case = jobs[job]
                failure = job.result()
                if failure:
                    failures += 1
                    print(f"FAIL {failure}", flush=True)
                else:
                    print(f"PASS {case}", flush=True)
    print(f"{len(cases) - failures}/{len(cases)} default driver tests passed.")
    return bool(failures)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("build", "run", "export-driver-inputs",
                                            "build-driver", "run-driver"))
    parser.add_argument("path", type=Path)
    parser.add_argument("inputs", type=Path, nargs="?")
    args = parser.parse_args()
    if (args.command == "run-driver") != (args.inputs is not None):
        parser.error("run-driver requires a driver inputs directory")
    match args.command:
        case "build":
            build(args.path)
        case "run":
            return run(args.path)
        case "export-driver-inputs":
            export_driver_inputs(args.path)
        case "build-driver":
            build_driver(args.path)
        case "run-driver":
            return run_driver(args.path, args.inputs)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
