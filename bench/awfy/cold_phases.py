"""Capture cold LangArena build phases and exclusive LLVM pass instruction counts."""
import argparse
from collections import Counter, defaultdict
import gzip
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile

import board
import build
import receipt
from request_profile import frames
from bench_cpus import add_cpu_sets, sibling_cpus, smt_activity, validate_cpu_sets

STAGES = (
    ("link", r"link_objects|direct_link|native_entry::Plan.*::emit"),
    ("machine-code/object", r"hirsel_llvm_emit_parts|emit_part|LLVMTargetMachineEmitTo|llvm::(SelectionDAG|AsmPrinter)"),
    ("ir-optimize", r"LLVMRunPasses|llvm::PassManager"),
    ("ir-compose", r"hirsel_llvm_compose|hirsel::Composer|llvm::Linker"),
    ("ir-emit", r"hirsel_llvm::core::.*(emit_module|emit_body|native_module)|native::emit_unit"),
    ("reach", r"reachable_for|build_reachable|reach::|instances::"),
    ("check/lower", r"check_lower|hirsel_core_front|hirsel_core_check|hirsel_core_resolve"),
)
RULES = [(stage, re.compile(pattern)) for stage, pattern in STAGES]
PERIOD = 20_000_003


def classify(stacks):
    result = Counter(dict.fromkeys([*(stage for stage, _ in STAGES), "other"], 0))
    for command, stack in stacks:
        if command.startswith(("clang", "ld.lld", "rust-lld", "collect2")):
            result["link"] += 1
        else:
            result[next((stage for frame in stack for stage, pattern in RULES if pattern.search(frame)),
                        "other")] += 1
    return result


def prepare(out, artifacts, env):
    out.mkdir()
    board.prepare_hirsel_compile(out, artifacts, env)
    _, _, command, _, executable, _ = board.compile_commands(out)
    return command, executable


def run_profile(directory, evidence, command, env):
    data = directory / "instructions.data"
    launch = ["perf", "record", "-e", "instructions:u", "-c", str(PERIOD), "--call-graph", "dwarf,8192",
              "-o", data, "--", *command, "--metrics", evidence / "driver-metrics.json"]
    with (evidence / "build.json").open("wb") as output, (evidence / "perf.log").open("wb") as error:
        board.run(launch, env=env, cwd=directory, stdout=output, stderr=error)
    return data


def pass_costs(path):
    passes = defaultdict(Counter)
    pipeline = Counter()
    exclusive = Counter()
    with gzip.open(path, "rt") as stream:
        for line in stream:
            row = json.loads(line)
            phase = row["phase"]
            if "pass" in row:
                name = row["pass"]
                count = row["instructions"]
                passes[name].update(count=1, instructions=count)
                exclusive[phase] += count
            else:
                pipeline[phase] += row["pipeline_instructions"]
    if not any(row["instructions"] > 0 for row in passes.values()):
        raise ValueError(f"{path}: no LLVM pass costs")
    if exclusive.keys() - pipeline.keys() or any(exclusive[phase] > total for phase, total in pipeline.items()):
        raise ValueError(f"{path}: exclusive pass costs exceed their pipeline counters")
    phases = {phase: {"instructions": total, "pass_instructions": exclusive[phase],
                      "unattributed_instructions": total - exclusive[phase]}
              for phase, total in sorted(pipeline.items())}
    return passes, phases


def print_pass_costs(trace, top, baseline=None):
    passes, phases = pass_costs(trace)
    total = sum(row["instructions"] for row in passes.values())
    if baseline is None:
        ranked = sorted(passes.items(), key=lambda item: (-item[1]["instructions"], item[0]))
        print(f"{'Pass':<52} {'Count':>8} {'Instructions':>16} {'Share':>8}")
        for name, row in ranked[:top]:
            print(f"{name:<52} {row['count']:>8,} {row['instructions']:>16,} "
                  f"{row['instructions'] / total:>8.2%}")
        shown = sum(row["instructions"] for _, row in ranked[:top])
        print(f"Top {min(top, len(ranked))} instructions: {shown:,} of {total:,} attributed")
        for phase, row in phases.items():
            print(f"{phase:<24} {row['instructions']:>16,} instructions, "
                  f"{row['unattributed_instructions']:,} unattributed")
        return

    before, _ = pass_costs(baseline)
    before_total = sum(row["instructions"] for row in before.values())
    changes = []
    for name in before.keys() | passes.keys():
        old, new = before.get(name, Counter()), passes.get(name, Counter())
        changes.append((new["instructions"] - old["instructions"], name, old, new))
    print(f"{'Pass':<52} {'Before N':>8} {'After N':>8} {'Before':>16} {'After':>16} "
          f"{'Delta':>16} {'Share Δ':>9}")
    for delta, name, old, new in sorted(changes, key=lambda row: (-abs(row[0]), row[1]))[:top]:
        share_delta = 100 * (new["instructions"] / total - old["instructions"] / before_total)
        print(f"{name:<52} {old['count']:>8,} {new['count']:>8,} {old['instructions']:>16,} "
              f"{new['instructions']:>16,} {delta:>+16,} {share_delta:>+6.2f} pp")
    print(f"Total attributed instructions: {before_total:,} -> {total:,} ({total - before_total:+,})")


def counted_build(directory, evidence, command, env):
    sample = board.measure.core_measure.sample(command, evidence, env=env, cwd=directory, pmu_times=True)
    (evidence / "stdout").replace(evidence / "build.json")
    if sample["status"] != "ok" or not receipt.counter_ready(sample, "instructions:u"):
        raise ValueError(f"{evidence}: cold build needs a fully active instructions:u counter")
    return sample["instructions"]


def collect_pass_costs(args, directory, command, exe, ld):
    instructions = {}
    images = {}
    raw = directory / "llvm-pass-costs.jsonl"
    trace = args.out / "llvm-pass-costs.jsonl.gz"
    for name in ("untraced", "traced"):
        evidence = args.out / name
        env = build.capped_env(directory / name) | ld
        if name == "traced":
            env["HIRSEL_LLVM_PASS_COST"] = str(raw)
        cold_command = command.copy()
        cold_command[cold_command.index("--store") + 1] = f"{name}-store"
        instructions[name] = counted_build(directory, evidence, cold_command, env)
        checked(directory, evidence, exe, args.fixture, env)
        images[name] = build.sha256(exe)
    if images["traced"] != images["untraced"]:
        raise ValueError("traced and untraced cold builds emitted different images")
    if not raw.is_file():
        raise ValueError("compiler did not emit LLVM pass costs")
    with raw.open("rb") as source, gzip.open(trace, "wb") as output:
        shutil.copyfileobj(source, output)
    passes, phases = pass_costs(trace)
    if not all(phase in phases for phase in ("release", "object-emission")):
        raise ValueError("trace must cover the release optimizer and object emission")
    overhead = instructions["traced"] - instructions["untraced"]
    return {
        "trace": str(trace), "records": sum(row["count"] for row in passes.values()),
        "image_sha256": images["traced"], "compile_instructions": instructions,
        "overhead_instructions": overhead, "overhead_percent": 100 * overhead / instructions["untraced"],
        "phases": phases,
    }


def checked(directory, evidence, executable, fixture, env):
    report = receipt.read(evidence / "build.json")
    if report.get("accepted") is not True or report.get("stopped") or not report["native"]["linked"]:
        raise ValueError(f"{directory}: cold build did not link")
    shutil.copyfile(fixture, directory / "fixture.json")
    name = "CLBG::Spectralnorm"
    checksum = next(row["checksum"] for row in receipt.read(fixture) if row["name"] == name)
    output = board.run([executable, "fixture.json", name], cwd=directory, env=env, capture_output=True, text=True)
    (evidence / "verification").write_text(output.stdout)
    if output.stdout != f"{name} checksum={checksum} expected={checksum} status=OK\n":
        raise ValueError(f"{directory}: cold image failed the Spectralnorm fixture")
    return output.stdout


def collect(args, scratch):
    artifacts = receipt.read(args.artifacts)
    tools = receipt.release_tools(artifacts)
    ld = {"LD_LIBRARY_PATH": str(build.llvm_path() / "lib")}
    directory = scratch / "profile"
    profile_evidence = args.out / "profile"
    profile_evidence.mkdir()
    prepare_env = build.capped_env(directory, "8G", "2") | ld
    command, exe = prepare(directory, artifacts, prepare_env)

    with receipt.measurement_slot(args.lock):
        try:
            os.sched_setaffinity(0, {args.measure_cpus[0]})
            env = build.capped_env(directory) | ld
            with smt_activity(args.siblings, "cold profile") as profile_busy:
                data = run_profile(directory, profile_evidence, command, env)
            stdout = checked(directory, profile_evidence, exe, args.fixture, env)
            passes = None
            pass_busy = None
            if args.pass_trace:
                with smt_activity(args.siblings, "LLVM pass costs") as pass_busy:
                    passes = collect_pass_costs(args, directory, command, exe, ld)
        finally:
            os.sched_setaffinity(0, args.build_cpus)

    counts = classify(frames(data, env=os.environ | {"DEBUGINFOD_URLS": ""}))
    if not counts.total():
        raise ValueError("no instruction samples in the cold build")
    output = args.out / "cold-phases.json"
    receipt.write(output, {
        "workload": "LangArena cold", "store": "fresh, with prebuilt std product",
        "argv": command, "stdout": stdout, "affinity": [args.measure_cpus[0]], **tools,
        "smt_busy_percent": profile_busy, "llvm_pass_trace_smt_busy_percent": pass_busy,
        "fixture": receipt.fixture_identity(directory / "inputs.json", args.fixture),
        "event": "instructions:u", "period": PERIOD, "samples": counts, "total_samples": counts.total(),
        "driver_metrics": receipt.read(profile_evidence / "driver-metrics.json"), "llvm_pass_trace": passes,
    })
    print(output)
    if passes:
        print_pass_costs(Path(passes["trace"]), args.top)
    return 0


def positive_int(text):
    value = int(text)
    if value < 1:
        raise argparse.ArgumentTypeError("expected a positive integer")
    return value


def main():
    if sys.argv[1:2] == ["report"]:
        parser = argparse.ArgumentParser(prog=f"{Path(sys.argv[0]).name} report")
        parser.add_argument("trace", type=Path)
        parser.add_argument("--baseline", type=Path, help="compare against this trace")
        parser.add_argument("--top", type=positive_int, default=10)
        args = parser.parse_args(sys.argv[2:])
        print_pass_costs(args.trace, args.top, args.baseline)
        return 0

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("out", type=lambda path: Path(path).resolve())
    parser.add_argument("--artifacts", required=True, type=Path)
    parser.add_argument("--fixture", required=True, type=lambda path: Path(path).resolve())
    parser.add_argument("--pass-trace", action="store_true", help="compare fresh-store builds with LLVM pass counters off and on")
    parser.add_argument("--top", type=positive_int, default=10)
    add_cpu_sets(parser)
    parser.add_argument("--lock", type=Path, default=board.LOCK)
    args = parser.parse_args()
    validate_cpu_sets(parser, args)
    args.siblings = sibling_cpus(args.measure_cpus[:1])
    if args.out.is_relative_to(build.ROOT):
        parser.error("use a new evidence directory outside the checkout")
    os.sched_setaffinity(0, args.build_cpus)
    args.out.mkdir(parents=True)
    with tempfile.TemporaryDirectory(prefix="hirsel-cold-phases-") as scratch:
        return collect(args, Path(scratch))


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (ValueError, OSError, KeyError, subprocess.SubprocessError) as error:
        print(f"cold-phases: {error}", file=sys.stderr)
        raise SystemExit(1)
