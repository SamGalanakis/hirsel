#!/usr/bin/env python3
"""Measure RUNNER-SPEC v2.70 F6's bind-mint fold.

Builds one release LLVM executable whose two modes run the same bind child. The
minted mode inserts the landed serial mint round trip; the direct mode reproduces
v2.60 by reading the Latest in the requester. Timed runs interleave the modes on one CPU
inside core/tools/timing_shield.py's hold, for example:

    python3 bench/runner/mint_cost.py /tmp/mint-cost-build --build-only
    HIRSEL_MEMORY_LIMIT=2G python3 core/tools/timing_shield.py run -- \\
        taskset -c 16 tools/capped python3 bench/runner/mint_cost.py \\
        /tmp/mint-cost-build --skip-build --pairs 60
"""
import argparse
import json
import os
from pathlib import Path
import statistics
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "core/tools"))
from measurement_artifacts import COMPILER_TARGET, PLATFORM, RUNTIME_TARGET
from omap_bench import counter_command
from store_tests import inputs

BINDS = 2_000
RUNS = 60
EVENTS = "task-clock,cycles:u,instructions:u"
BUILD_CPUS = ("taskset", "-c", "8-11")


def toolchain(out):
    identity_path = out / "toolchain.json"
    if identity_path.is_file():
        identity = json.loads(identity_path.read_text())
        if Path(identity["compiler"]).is_file() and Path(identity["runtime"]).is_file():
            return identity
    outputs = json.loads(subprocess.check_output([
        str(ROOT / ".buck2/bin/buck2"), "build", "--remote-only", "-j4", "--target-platforms",
        "//tools/buck2:release", COMPILER_TARGET, RUNTIME_TARGET, "--show-json-output",
    ], cwd=ROOT, text=True, env=os.environ | {"HIRSEL_JOBS": "4", "HIRSEL_MEMORY_LIMIT": "4G"}))
    compiler = ROOT / outputs["root" + COMPILER_TARGET]
    runtime = ROOT / outputs["root" + RUNTIME_TARGET] / PLATFORM / "libhirsel_core_rt.a"
    result = subprocess.run([
        sys.executable, str(ROOT / "core/tools/measurement_artifacts.py"),
        "--compiler", compiler, "--runtime", runtime, "--compiler-profile", "release",
    ], cwd=ROOT, check=True, stdout=subprocess.PIPE, text=True,
        env=os.environ | {"HIRSEL_JOBS": "4", "HIRSEL_MEMORY_LIMIT": "4G"})
    identity = json.loads(result.stdout)
    identity_path.write_text(json.dumps(identity, indent=2))
    return identity


def manifest(out):
    document = inputs(ROOT, tests=False, package="runner")
    runner = next(p for p in document["packages"] if p["package"]["name"] == "runner")
    runner["sources"].append({
        "file": str(ROOT / "bench/runner/mint-cost.hirsel"),
        "modules": ["runner"],
        "signatures": False,
    })
    path = out / "inputs.json"
    path.write_text(json.dumps(document))
    return path


def build(out):
    identity = toolchain(out)
    inputs_path = manifest(out)
    subprocess.run([
        *BUILD_CPUS, identity["compiler"], "build", "--release", "--backend", "llvm",
        "--source-core", "--source-std", "--inputs", inputs_path,
        "--entry", "runner@1/runner/mint_cost_main", "--limit", str(64 << 20),
        "--runtime", identity["runtime"], "--cc",
        ROOT / "tools/buck2/toolchains/llvm-files/bin/hirsel-clang",
        "--grant", "Clock", "--grant", "Tasks", "--grant", "Stdout", "--grant", "Stdin",
        "--store", out / "compile-store",
        "-o", out / "mint-cost",
    ], cwd=ROOT, check=True, env=os.environ | {"HIRSEL_MEMORY_LIMIT": "2G"})
    return identity


def read_counters(path):
    counts = {}
    for line in path.read_text().splitlines():
        fields = line.split(",")
        if len(fields) >= 5 and fields[2] in EVENTS.split(","):
            if float(fields[4]) < 99.5:
                raise RuntimeError(f"multiplexed counter: {line}")
            scale = 1e6 if fields[1] == "msec" else 1.0
            counts[fields[2]] = float(fields[0]) * scale
    if set(counts) != set(EVENTS.split(",")):
        raise RuntimeError(f"missing counters in {path.read_text()}")
    return counts


def sample(out, binary, mode):
    perf = out / f"perf-{mode}.csv"
    control_read, control_write = os.pipe()
    ack_read, ack_write = os.pipe()
    command = [
        "perf", "stat", "-x,", "-e", EVENTS, "--delay=-1",
        "--control", f"fd:{control_read},{ack_write}", "-o", perf, "--",
        binary, mode,
    ]
    try:
        with subprocess.Popen(
            command, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            pass_fds=(control_read, ack_write), env=os.environ | {"HIRSEL_MEMORY_LIMIT": "2G"},
        ) as process:
            ready = process.stdout.readline()
            if ready != b"READY\n":
                raise RuntimeError(f"{mode}: no READY: {ready!r}")
            counter_command((control_write, ack_read), "enable")
            process.stdin.write(b"x")
            process.stdin.flush()
            done = process.stdout.readline()
            if done != f"DONE {BINDS}\n".encode():
                raise RuntimeError(f"{mode}: bad DONE: {done!r}")
            counter_command((control_write, ack_read), "disable")
            process.stdin.close()
            extra = process.stdout.read()
            errors = process.stderr.read().decode(errors="replace")
            code = process.wait()
            if code or extra != f"SUM {BINDS}\n".encode():
                raise RuntimeError(f"{mode}: {code=} {extra=} errors={errors[-2000:]}")
    finally:
        for fd in (control_read, control_write, ack_read, ack_write):
            os.close(fd)
    return read_counters(perf)


def percentile(values, p):
    ordered = sorted(values)
    return ordered[min(len(ordered) - 1, int(p * len(ordered)))]


def measure(out, pairs):
    binary = out / "mint-cost"
    if not binary.is_file():
        raise RuntimeError(f"missing {binary}; run without --skip-build first")
    samples = {"direct": [], "minted": []}
    order = ("direct", "minted")
    for pair in range(pairs):
        # Interleave the arms and alternate which one goes first, so drift and
        # residual thermal/order effects land on both sides of the pair.
        for mode in order:
            samples[mode].append(sample(out, binary, mode))
        order = tuple(reversed(order))
    rows = {}
    for mode, runs in samples.items():
        ns = [run["task-clock"] / BINDS for run in runs]
        cycles = [run["cycles:u"] / BINDS for run in runs]
        rows[mode] = {
            "ns_per_bind_median": statistics.median(ns),
            "ns_per_bind_p99": percentile(ns, 0.99),
            "cycles_per_bind_median": statistics.median(cycles),
            "instructions_per_bind_median": statistics.median(run["instructions:u"] for run in runs) / BINDS,
            "binds_per_second_median": 1e9 / statistics.median(ns),
            "samples": ns,
        }
    paired = [m["task-clock"] / BINDS - d["task-clock"] / BINDS
              for d, m in zip(samples["direct"], samples["minted"], strict=True)]
    result = {
        "binds_per_sample": BINDS,
        "pairs": len(paired),
        "arms": rows,
        "mint_ns_per_bind_median_paired": statistics.median(paired),
        "mint_ns_per_bind_p99_paired": percentile(paired, 0.99),
        "mint_ns_per_bind_mean_paired": statistics.mean(paired),
        "paired_differences_ns": paired,
    }
    (out / "mint-cost-results.json").write_text(json.dumps(result, indent=2))
    print(f"pairs {len(paired)} of {BINDS} binds per arm")
    for mode in ("direct", "minted"):
        row = rows[mode]
        print(f"{mode}: ns_per_bind median {row['ns_per_bind_median']:.2f} "
              f"p99 {row['ns_per_bind_p99']:.2f}; binds/s {row['binds_per_second_median']:.1f}; "
              f"cycles median {row['cycles_per_bind_median']:.1f}; instructions {row['instructions_per_bind_median']:.1f}")
    print(f"mint cost: paired median {result['mint_ns_per_bind_median_paired']:.2f} ns/bind "
          f"({result['mint_ns_per_bind_median_paired'] / 1000:.3f} us/bind), "
          f"p99 {result['mint_ns_per_bind_p99_paired']:.2f} ns/bind")
    print(f"wrote {out / 'mint-cost-results.json'}")
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("output", type=Path, help="scratch directory for the executable and results")
    parser.add_argument("--pairs", type=int, default=RUNS)
    build_mode = parser.add_mutually_exclusive_group()
    build_mode.add_argument("--build-only", action="store_true")
    build_mode.add_argument("--skip-build", action="store_true")
    args = parser.parse_args()
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=True)
    if not args.skip_build:
        identity = build(out)
        print(f"compiler {identity['compiler_sha256']} runtime {identity['runtime_archive_sha256']}")
    if not args.build_only:
        measure(out, args.pairs)
    return 0


if __name__ == "__main__":
    sys.exit(main())
