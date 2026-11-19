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

pub fn rx5_main(out: Stdout) -> Result[(), Exit] waits {
    let threshold = regex_step_thresholds()
    if threshold == 0 || !regex_cache_refusal() {
        return Error(Exit(status: 1))
    }
    let mut answer = "RX5 threshold "
    result.unwrap(string.push_u64(mut answer, threshold))
    result.unwrap(string.push(mut answer, "\\n"))
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


def run(compiler, runtime, evaluator):
    law = ROOT / "core/std/tests/regex_charges.hirsel"
    case = Contract(law, 0, "rx3_events()", "true", False, test_of="regex")
    with tempfile.TemporaryDirectory(prefix="hirsel-regex-replay-") as tmp:
        work = Path(tmp)
        libraries = work / "libraries"
        (libraries / "std").mkdir(parents=True)
        (libraries / "core").symlink_to(ROOT / "core/core", target_is_directory=True)
        for module in (ROOT / "core/std").glob("*.hirsel"):
            shutil.copyfile(module, libraries / "std" / module.name)
        body = libraries / "std/regex.hirsel"
        finalizers = law.with_name("regex_finalizer.hirsel").read_text()
        bounded_search = finalizers[finalizers.index("resource type BoundedSearch"):].replace("regex.", "")
        source = body.read_text() + law.read_text() + bounded_search + WRAPPER
        body.write_text(source)

        def invoke(command, app, backend, *extra):
            return subprocess.run([
                compiler, command, app, "--libraries", libraries, "--source-core",
                "--runtime", runtime, "--store", work / "store", "--limit", "207000000000",
                "--debug-lines", "on", "--backend", backend, *extra,
                "-o", app / (command + "-" + backend),
            ], cwd=work, capture_output=True, text=True)

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
            expected = Counter((size, size - 1) for size in (2264, 16280, 206158430744))
            assert refused == expected, refused
            threshold = evaluate(engine, cfg, entry + "rx5_threshold")["outcome"]
            assert threshold["kind"] == "returned" and len(threshold["values"]) == 1, threshold
            (steps,) = threshold["values"]
            assert steps > 0

            app, _ = application("caught", """pub fn main(out: Stdout) -> Result[(), Exit] waits {
    std.regex.rx5_main(out)
}
""")
            tape = work / "caught.tape"
            for backend in ("cranelift", "llvm"):
                extra = ["--record", tape] if backend == "cranelift" else []
                result = invoke("run", app, backend, "--grant", "Stdout", *extra)
                assert result.returncode == 0 and result.stdout == f"RX5 threshold {steps}\n", result
                result = invoke("replay", app, backend, "--tape", tape)
                assert result.returncode == 0, result
            thresholds = [(steps >> (16 * i)) & 65535 for i in range(4)]
            print(f"RX3 three exact refusals. RX5 caught thresholds {thresholds} on all engines and replay.", flush=True)

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
                    tape = work / (app.name + ".tape")
                    for backend in ("cranelift", "llvm"):
                        extra = ["--record", tape] if backend == "cranelift" else []
                        check_failure(invoke("run", app, backend, *extra), body.name, expected)
                        check_failure(invoke("replay", app, backend, "--tape", tape), body.name, expected)
            print("RX5 uncaught: eight cases with exact evaluator, Cranelift, LLVM and replay sites.")


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
    for name in ("compiler", "runtime", "evaluator"):
        parser.add_argument("--" + name, type=Path, required=True)
    args = parser.parse_args()
    run(args.compiler.resolve(), args.runtime.resolve(), args.evaluator.resolve())


if __name__ == "__main__":
    main()
