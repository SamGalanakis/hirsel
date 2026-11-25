#!/usr/bin/env python3
"""Compare RX3 admissions and RX5 exhaustion across the evaluator and replay."""

import argparse
from collections import Counter
import json
from pathlib import Path
import re
import shutil
import subprocess
import tempfile

from std_native import Contract, Evaluator, ROOT, manifest

WRAPPER = """
pub fn rx3_events() -> bool {
    regex_cache_refusal()
}

pub fn rx5_threshold() -> u64 {
    regex_step_thresholds()
}

pub fn rx5_fixed_capture_threshold() -> u64 {
    regex_fixed_capture_threshold()
}

pub fn rx5_main(out: Stdout) -> Result[(), Exit] waits {
    let threshold = regex_step_thresholds()
    let fixed_capture = regex_fixed_capture_threshold()
    if threshold == 0 || fixed_capture == 0 || !regex_cache_refusal() {
        return Error(Exit(status: 1))
    }
    let mut answer = "RX5 threshold "
    string.push_u64(mut answer, threshold)
    string.push(mut answer, " ")
    string.push_u64(mut answer, fixed_capture)
    string.push(mut answer, "\\n")
    result.unwrap(stdout.write(out, answer))
    Ok(())
}

pub fn rx5_failure(steps: u64, history: u64) {
    regex_uncaught_steps(steps, history, 0)
}
"""


def position(source, section, text):
    start = source.index(text, source.index(section))
    return source.count("\n", 0, start) + 1, start - source.rfind("\n", 0, start)


def run(compiler, runtime, recording_runtime, evaluator):
    law = ROOT / "core/std/tests/regex_charges.hirsel"
    case = Contract(law, 0, "rx3_events()", "true", False, test_of="regex")
    with tempfile.TemporaryDirectory(prefix="hirsel-regex-replay-") as tmp:
        work = Path(tmp)
        libraries = work / "libraries"
        (libraries / "std").mkdir(parents=True)
        (libraries / "core").symlink_to(ROOT / "core/core", target_is_directory=True)
        shutil.copytree(ROOT / "core/std", libraries / "std", dirs_exist_ok=True,
                        ignore=shutil.ignore_patterns("tests"))
        body = libraries / "std/regex.hirsel"
        finalizers = law.with_name("regex_finalizer.hirsel").read_text()
        bounded_search = finalizers[finalizers.index("resource type BoundedSearch"):].replace("regex.", "")
        source = body.read_text() + law.read_text() + bounded_search + WRAPPER
        body.write_text(source)

        def invoke(command, app, backend, *extra, recording=False):
            archive = recording_runtime if recording else runtime
            return subprocess.run([
                compiler, command, app, "--libraries", libraries, "--source-core",
                "--runtime", archive, "--store", work / "store", "--limit", "207000000000",
                "--debug-lines", "on", "--backend", backend, *extra,
                "-o", app / (command + "-" + backend),
            ], cwd=work, capture_output=True, text=True)

        def runs_and_replays(app, *grants):
            for backend, other in (("cranelift", "llvm"), ("llvm", "cranelift")):
                tape = work / f"{app.name}-{backend}.tape"
                live = invoke("run", app, backend, *grants)
                recorded = invoke("run", app, backend, *grants, "--record", tape, recording=True)
                replay = invoke("replay", app, other, "--tape", tape, recording=True)
                assert replay.returncode == 0 and not replay.stdout, replay
                yield live, recorded

        def application(name, source):
            app = work / name
            app.mkdir()
            main = app / "main.hirsel"
            main.write_text(source)
            return app, main

        division = position(source, "fn regex_uncaught_steps(", "1u64 / divisor")
        stops = [
            position(source, "resource type BoundedSearch", "next(s.re"),
            position(source, "pub fn next(", "check_cache(re, c)"),
        ]
        inputs = manifest(ROOT, case, body)
        cfg = work / "inputs.json"
        cfg.write_text(json.dumps(inputs))
        entry = "std@1/regex/"
        with Evaluator(evaluator) as engine:
            admission = evaluate(engine, cfg, entry + "rx3_events", events=True)
            assert admission["outcome"] == {"kind": "returned", "values": [1]}, admission
            # CAPACITY = 17. Its event fields carry requested and available.
            refused = Counter((requested, available) for kind, _, requested, available in admission["events"]
                              if kind == 17)
            # For a, W(S) = 48S + 182. At B=16384 S=81 and the charge is 16,280.
            # The largest cache uses S=floor((2^31-1)/4) states.
            expected = Counter((size, size - 1) for size in (2264, 16280, 103079215640))
            assert refused == expected, refused
            threshold = evaluate(engine, cfg, entry + "rx5_threshold")["outcome"]
            assert threshold["kind"] == "returned" and len(threshold["values"]) == 1, threshold
            (steps,) = threshold["values"]
            assert steps > 0
            fixed = evaluate(engine, cfg, entry + "rx5_fixed_capture_threshold")["outcome"]
            assert fixed["kind"] == "returned" and len(fixed["values"]) == 1, fixed
            (capture_steps,) = fixed["values"]
            assert capture_steps > 0

            app, _ = application("caught", """pub fn main(out: Stdout) -> Result[(), Exit] waits {
    std.regex.rx5_main(out)
}
""")
            for live, recorded in runs_and_replays(app, "--grant", "Stdout"):
                for result in (live, recorded):
                    assert result.returncode == 0 and result.stdout == f"RX5 threshold {steps} {capture_steps}\n", result
            thresholds = [(steps >> (16 * i)) & 65535 for i in range(4)]
            print(f"RX3 three exact refusals. RX5 caught thresholds {thresholds} and fixed capture "
                  f"{capture_steps} on all engines and replay.", flush=True)

            app_package = {"name": "app", "version": "1"}
            inputs["packages"].append({
                "package": app_package, "dependencies": [inputs["core"], {"name": "std", "version": "1"}],
                "sources": [],
            })
            for steps, stop in enumerate(stops, 1):
                for history in range(4):
                    app, main = application(f"uncaught-{steps}-{history}", f"""pub fn main() -> Result[(), Exit] {{
    std.regex.rx5_failure({steps}, {history})
    Ok(())
}}
""")
                    inputs["packages"][-1]["sources"] = [{"file": str(main), "modules": ["main"], "signatures": False}]
                    cfg.write_text(json.dumps(inputs))
                    expected = [("Division", *division), ("OutOfTime", *stop)]
                    report = evaluate(engine, cfg, "app@1/main/main", program_entry=True)
                    assert report["outcome"] == {"kind": "failed", "causes": [kind for kind, *_ in expected]}, report
                    sites = [tuple(site[1:]) if Path(site[0]).name == body.name else None for site in report["cause_sites"]]
                    assert sites == [cause[1:] for cause in expected], (expected, report)
                    for live, recorded in runs_and_replays(app):
                        check_failure(live, body.name, expected)
                        check_failure(recorded, body.name, expected)
            print("RX5 uncaught: eight cases with exact evaluator and native sites and sealed replays.")


def evaluate(engine, config, entry, **options):
    result = engine.evaluate(config, entry, **options)
    result.check_returncode()
    return json.loads(result.stdout)


def check_failure(result, module, expected):
    sites = [
        (kind, int(line), int(column))
        for kind, path, line, column in re.findall(
            r"^(\w+) at (.+):(\d+):(\d+)$", result.stderr, flags=re.MULTILINE
        )
        if Path(path).name == module
    ]
    assert (result.returncode == 101 and not result.stdout and sites == expected
            and len(result.stderr.splitlines()) == len(expected)), (expected, result)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("compiler", "runtime", "recording-runtime", "evaluator"):
        parser.add_argument("--" + name, type=Path, required=True)
    args = parser.parse_args()
    run(args.compiler.resolve(), args.runtime.resolve(), args.recording_runtime.resolve(),
        args.evaluator.resolve())


if __name__ == "__main__":
    main()
