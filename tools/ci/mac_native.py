#!/usr/bin/env python3
"""Build native test harnesses and run named cases with a scrubbed environment."""

import argparse
import json
import os
from pathlib import Path
import subprocess

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
    "hirsel-code": ("--lib", (
        "tests::a_loaded_entry_runs_and_a_refused_module_installs_nothing",
        "tests::a_dead_cohort_faults_and_its_range_is_published_again",
        "tests::a_batch_is_one_cohort_that_lives_until_its_last_image_dies",
        "tests::activate_swaps_at_the_expected_version_within_the_retired_budget",
        "tests::a_code_address_resolves_to_its_image_and_a_fault_quarantines_it",
    )),
    "hirsel-cranelift": ("--test=core_native", (
        "slots_loops_calls_and_c_struct_returns_match_evaluator",
    )),
}


def build(destination):
    harnesses = {}
    for package, (selection, _) in SUITES.items():
        command = ["cargo", "test", "--locked", "--no-run", "-p", package,
                   selection, "--message-format=json"]
        env = os.environ.copy()
        if package == "hirsel-core-rt":
            command.append("--target=aarch64-apple-darwin")
            env["RUSTC_BOOTSTRAP"] = "1"
            env["RUSTFLAGS"] = f"{env.get('RUSTFLAGS', '')} -Zsanitizer=address"
        with subprocess.Popen(command, stdout=subprocess.PIPE, text=True, env=env) as cargo:
            candidates = []
            for line in cargo.stdout:
                event = json.loads(line)
                match event["reason"]:
                    case "compiler-message":
                        print(event["message"]["rendered"] or "", end="", flush=True)
                    case "compiler-artifact":
                        if event["profile"]["test"] and event["executable"]:
                            candidates.append(event["executable"])
            if cargo.wait():
                raise SystemExit(cargo.returncode)
            if len(candidates) != 1:
                raise SystemExit(f"{package}: expected one native test binary, saw {candidates}")
            harnesses[package] = candidates[0]
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


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("build", "run"))
    parser.add_argument("harnesses", type=Path)
    args = parser.parse_args()
    if args.command == "build":
        build(args.harnesses)
        return 0
    return run(args.harnesses)


if __name__ == "__main__":
    raise SystemExit(main())
