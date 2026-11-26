#!/usr/bin/env python3
"""Run Runner's HTTP delivery laws or the keyed record-and-replay laws."""

import argparse
from concurrent.futures import ThreadPoolExecutor
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import tempfile
import tomllib


ROOT = Path(__file__).resolve().parents[2]
RUNNER = ROOT / "core/programs/runner"
NET = "Net=Listen 127.0.0.0/8 0-65535;Connect 127.0.0.0/8 0-65535"


def run(command, *, expected=None, json_output=False):
    env = dict(os.environ)
    if expected is not None:
        env["HIRSEL_L15_BODY_RUNS"] = str(expected)
    result = subprocess.run(
        command, cwd=ROOT, check=True, env=env, text=True, capture_output=True,
    )
    if json_output:
        return json.loads(result.stdout)
    return result.stderr


def fixture(work, compiler, cc):
    (work / "hirsel.toml").write_text('[workspace]\nmembers = ["runner", "store"]\n')
    source = work / "runner"
    manifest = (RUNNER / "hirsel.toml").read_text()
    declared = tomllib.loads(manifest)
    tests = declared["tests"]["sources"]
    existing = declared["sources"]
    # The package is exactly the files its manifest declares.
    for path in ["hirsel.toml", *existing, *tests]:
        (source / path).parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(RUNNER / path, source / path)
    shutil.copytree(RUNNER.parent / "store", work / "store")
    manifest_file = source / "hirsel.toml"
    fixtures = [f'{json.dumps(path)} = {json.dumps(module)}' for path, module in tests.items() if path not in existing]
    manifest_file.write_text(manifest.split("[tests]\n", 1)[0] + "\n".join(fixtures) + "\n")
    directory = work / "disk"
    directory.mkdir()
    binary = work / "restart"
    run([
        compiler, "build", source, "--entry", "runner@1/runner/l15_process",
        "--backend", "cranelift", "--libraries", ROOT / "core", "--source-std",
        "--cc", cc, "--limit", "67108864", "--dir", directory,
        "--grant", "Clock", "--grant", "Random", "--grant", "Tasks", "--grant", NET,
        "-o", binary,
    ])
    for phase, expected in (("cut", 3), ("recover", 0)):
        trace = run([binary, phase], expected=expected)
        if f"L15 body runs: {expected}\n" not in trace:
            raise RuntimeError(f"L15 {phase} trace missing")
        print(f"L15 {phase} | {expected} body runs")


KEYED_REPLAY_CASES = {
    "runner": (
        "keyed_http_webhook_history",
        "keyed_http_horizon_at_deciding_admission",
        "keyed_http_claim_route_and_alias",
        "quiet_reopen_recovers_bound_claims_once",
    ),
    "store": (
        "claim_expires_from_its_reply",
        "dropped_claim_time_survives_reopen_before_sweep",
    ),
}


def keyed_replay(compiler, runtime, engine):
    for package, cases in KEYED_REPLAY_CASES.items():
        command = [compiler, "test", RUNNER.parent / package, "--libraries", ROOT / "core",
                   "--backend", engine, "--replay", "--rerun", "-j", "1", "--json"]
        if runtime is not None:
            command.extend(["--runtime", runtime])
        names = {f"{package}@1/{package}/{name}" for name in cases}
        for name in sorted(names):
            command.extend(["--exact", name])
        verdict = run(command, json_output=True)
        passed = {test["definition"] for test in verdict["tests"] if test["passed"]}
        if not verdict["passed"] or passed != names:
            raise RuntimeError(f"{engine} keyed replay missing laws: {names - passed}")
        print(f"{engine} {package} | {len(passed)} keyed replay laws")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--compiler", required=True, type=Path)
    parser.add_argument("--keyed-replay", action="store_true")
    parser.add_argument("--record-runtime", type=Path)
    args = parser.parse_args()
    compiler = args.compiler.resolve()
    if args.keyed_replay:
        runtime = args.record_runtime.resolve() if args.record_runtime else None
        with ThreadPoolExecutor(max_workers=2) as pool:
            jobs = [pool.submit(keyed_replay, compiler, runtime, engine)
                    for engine in ("cranelift", "llvm")]
            for job in jobs:
                job.result()
        return
    if args.record_runtime:
        parser.error("--record-runtime requires --keyed-replay")
    with tempfile.TemporaryDirectory(prefix="hirsel-http-l15-") as temp:
        work = Path(temp)
        obj = work / "trace.o"
        run([
            "cc", "-c", "-fPIC", RUNNER / "tests/http_delivery_trace.c",
            "-I", ROOT / "crates/hirsel-core-rt/include", "-o", obj,
        ])
        cc = work / "cc"
        cc.write_text('#!/bin/sh\nexec cc "$@" ' + shlex.quote(str(obj)) +
                      " -Wl,--wrap=hirsel_core_os_run_with_sources\n")
        cc.chmod(0o755)
        verdict = run([
            compiler, "test", RUNNER, "--backend", "cranelift", "--source-std",
            "--libraries", ROOT / "core", "--filter", "/test_l15", "--cc", cc, "-j", "4", "--json",
        ], expected=3, json_output=True)
        if not verdict["passed"] or len(verdict["tests"]) != 1 or "L15 body runs: 3\n" not in verdict["tests"][0]["stderr"]:
            raise RuntimeError(f"L15 trace missing from test verdict {verdict}")
        print("L15 | 3 body runs, bounded expiry and retirement")
        # L17: two equal keyed sends while the first run is parked on a cold
        # read complete one run, and the next key's increment one more.
        joined = run([
            compiler, "test", RUNNER, "--backend", "cranelift", "--source-std",
            "--libraries", ROOT / "core", "--filter", "/keyed_http_join_on_cold_read", "--cc", cc, "-j", "4", "--json",
        ], expected=2, json_output=True)
        if not joined["passed"] or len(joined["tests"]) != 1 or "L15 body runs: 2\n" not in joined["tests"][0]["stderr"]:
            raise RuntimeError(f"L17 trace missing from test verdict {joined}")
        print("L17 | 2 body runs, one per claim")
        fixture(work, compiler, cc)


if __name__ == "__main__":
    try:
        main()
    except subprocess.CalledProcessError as error:
        raise SystemExit(f"HTTP law exit {error.returncode}\n{error.stdout}\n{error.stderr}") from error
