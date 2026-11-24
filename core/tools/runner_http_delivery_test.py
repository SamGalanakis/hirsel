#!/usr/bin/env python3
"""Run Runner L15 with a native body trace and a fresh process at reopen."""

import argparse
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
    shutil.copytree(RUNNER, source, ignore=shutil.ignore_patterns("models", "tests"))
    shutil.copytree(RUNNER.parent / "store", work / "store")
    manifest_file = source / "hirsel.toml"
    manifest = manifest_file.read_text()
    tests = tomllib.loads(manifest)["tests"]["sources"]
    existing = tomllib.loads(manifest)["sources"]
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


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--compiler", required=True, type=Path)
    args = parser.parse_args()
    compiler = args.compiler.resolve()
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
        fixture(work, compiler, cc)


if __name__ == "__main__":
    try:
        main()
    except subprocess.CalledProcessError as error:
        raise SystemExit(f"L15 exit {error.returncode}\n{error.stdout}\n{error.stderr}") from error
