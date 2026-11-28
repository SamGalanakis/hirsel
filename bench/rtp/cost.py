#!/usr/bin/env python3
"""Check RTP-12's parse_rewrite_cost: at most 80 ns and 0 allocations per packet.

Allocations and instructions run on build CPUs without a lock. The timed mode
runs inside a shielded hold, for example

    python3 bench/rtp/cost.py <dir> --build-only
    HIRSEL_MEMORY_LIMIT=2G HIRSEL_JOBS=1 python3 core/tools/timing_shield.py run -- \\
        taskset -c 16 tools/capped python3 bench/rtp/cost.py <dir> --mode time --skip-build
"""

import argparse
import json
import os
from pathlib import Path
import shlex
import statistics
import subprocess
import sys
import tempfile

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / 'tools/toolchain-dist'))
import hcfg

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
sys.path.insert(0, str(ROOT / "core/tools"))
from measurement_artifacts import (
    ALLOCATION_RUNTIME_TARGET,
    COMPILER_TARGET,
    PLATFORM,
    RUNTIME_TARGET,
    phase_allocations,
)
from omap_bench import counter_command
from std_native import library_sources

PACKETS = 1_000_000
TARGET_NS = 80
RUNS = 60
ROOT_LIMIT = 64 << 20
FNV = 0x100000001b3
MASK = (1 << 64) - 1


def expected_sum():
    # The subscriber header of packet i, from RTP-11's offsets: outgoing index
    # 1000 + i, timestamp 5000 + 960 i, transport sequence i, level 42 with voice.
    total = 0xcbf29ce484222325
    for i in range(1, PACKETS + 1):
        head = bytes([0x90, 0x6f]) + ((1000 + i) & 0xffff).to_bytes(2, "big") \
            + ((5000 + 960 * i) & 0xffffffff).to_bytes(4, "big") + bytes.fromhex("11223344bede0002") \
            + bytes([0x10, 0xaa, 0x31]) + (i & 0xffff).to_bytes(2, "big") + bytes(3)
        for at in (0, 8, 16):
            total = ((total ^ int.from_bytes(head[at:at + 8], "big")) * FNV) & MASK
    return total


def phase_linker(cc, out, flavor):
    support = out / f"{flavor}-phase.o"
    define = ["-DCOUNT_ALLOCATIONS"] if flavor == "counted" else []
    subprocess.run([
        cc, "-c", "-O2", "-Wall", "-Wextra", "-Werror", *define,
        "-I", ROOT / "crates/hirsel-core-rt/include",
        ROOT / "bench/srtp/phase.c", "-o", support,
    ], check=True)
    linker = out / f"{flavor}-cc"
    linker.write_text(f'#!/bin/sh\nexec {shlex.quote(str(cc))} "$@" '
                      f'{shlex.quote(str(support))} -Wl,--wrap=hirsel_core_host_io\n')
    linker.chmod(0o755)
    return linker


def build(out):
    targets = [COMPILER_TARGET, "//:core-driver-runtime-pic", RUNTIME_TARGET, ALLOCATION_RUNTIME_TARGET]
    paths = json.loads(subprocess.check_output([
        ROOT / ".buck2/bin/buck2", "build", "-j4", "--target-platforms",
        "//tools/buck2:release", "--show-json-output", *targets,
    ], cwd=ROOT))
    compiler = ROOT / paths["root" + COMPILER_TARGET]
    cc = ROOT / "tools/buck2/toolchains/llvm-files/bin/hirsel-clang"

    source_package = ROOT / "core/programs/rtp"
    manifest = hcfg.package(source_package)
    core = dict(name="core", version="1")
    std = dict(name="std", version="1")
    rtp = dict(name="rtp", version=str(manifest["version"].split(".")[0]))
    app = dict(name="rtp_cost", version="1")
    document = dict(core=core, packages=[
        dict(package=core, dependencies=[], sources=library_sources(ROOT / "core/core", True)),
        dict(package=std, dependencies=[core], sources=library_sources(ROOT / "core/std", False)),
        dict(package=rtp, dependencies=[core, std], sources=[
            dict(file=str(source_package / file), modules=module.split("/"), signatures=False)
            for file, module in manifest["sources"].items()
        ]),
        dict(package=app, dependencies=[core, std, rtp], sources=[
            dict(file=str(HERE / "forward.hirsel"), modules=["main"], signatures=False),
        ]),
    ])
    manifest_path = out / "inputs.json"
    manifest_path.write_text(json.dumps(document))

    for flavor, target in (("shipping", RUNTIME_TARGET), ("counted", ALLOCATION_RUNTIME_TARGET)):
        runtime = ROOT / paths["root" + target] / PLATFORM / "libhirsel_core_rt.a"
        linker = phase_linker(cc, out, flavor)
        subprocess.run([
            compiler, "build", "--release", "--backend", "llvm", "--source-core",
            "--source-std", "--inputs", manifest_path, "--entry", "rtp_cost@1/main/main",
            "--limit", str(ROOT_LIMIT), "--runtime", runtime, "--cc", linker, "--grant", "Stdout",
            "--store", out / "build-store", "-o", out / flavor,
        ], cwd=ROOT, check=True, env=os.environ | {"HIRSEL_MEMORY_LIMIT": "2G"})


def sample(out, binary, events, checksum, *, shielded=False):
    """Run one process; answer the allocation count, or the counter values."""
    command = [] if shielded else ["systemd-run", "--user", "--scope", "--quiet", "-p", "MemoryMax=2G",
                                   "-p", "MemorySwapMax=0"]
    command += ["timeout", "300"]
    perf = out / "perf.csv"
    control_read, control_write = os.pipe()
    ack_read, ack_write = os.pipe()
    try:
        with tempfile.TemporaryFile() as stderr:
            if events:
                command += ["perf", "stat", "-x,", "-e", events, "--delay=-1",
                            "--control", f"fd:{control_read},{ack_write}", "-o", perf, "--"]
            command.append(binary)
            with subprocess.Popen(
                command, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=stderr,
                pass_fds=(control_read, ack_write) if events else (),
                env=os.environ | {"HIRSEL_MEMORY_LIMIT": "2G"},
            ) as process:
                ready = process.stdout.readline()
                done = b""
                if ready == b"READY\n":
                    if events:
                        counter_command((control_write, ack_read), "enable")
                    process.stdin.write(b"x")
                    process.stdin.flush()
                    done = process.stdout.readline()
                    if done == f"DONE {PACKETS}\n".encode():
                        if events:
                            counter_command((control_write, ack_read), "disable")
                        process.stdin.write(b"x")
                        process.stdin.flush()
                process.stdin.close()
                extra = process.stdout.read()
                code = process.wait()
            stderr.seek(0)
            errors = stderr.read().decode(errors="replace")
            if code or ready != b"READY\n" or done != f"DONE {PACKETS}\n".encode() \
                    or extra != f"SUM {checksum}\n".encode():
                raise RuntimeError(f"{code=} {ready=} {done=} {extra=} want SUM {checksum}\n{errors}")
            if not events:
                return phase_allocations(errors)
    finally:
        for fd in (control_read, control_write, ack_read, ack_write):
            os.close(fd)
    counts = {}
    for line in perf.read_text().splitlines():
        fields = line.split(",")
        if len(fields) >= 5 and fields[2] in events.split(","):
            if float(fields[4]) != 100:
                raise RuntimeError(f"multiplexed counter: {line}")
            # perf reports task-clock in msec; keep every value in its base unit.
            scale = 1e6 if fields[1] == "msec" else 1.0
            counts[fields[2]] = float(fields[0]) * scale
    if set(counts) != set(events.split(",")):
        raise RuntimeError(f"missing counters in {perf.read_text()}")
    return counts


def percentile(values, p):
    ordered = sorted(values)
    return ordered[min(len(ordered) - 1, int(p * len(ordered)))]


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("output", type=Path, help="scratch directory for the two native executables")
    parser.add_argument("--mode", choices=("allocations", "instructions", "time"), default="allocations")
    parser.add_argument("--before", type=Path, help="time mode: a parent build to interleave as the before arm")
    build_mode = parser.add_mutually_exclusive_group()
    build_mode.add_argument("--build-only", action="store_true")
    build_mode.add_argument("--skip-build", action="store_true")
    args = parser.parse_args()
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=True)
    if not args.skip_build:
        build(out)
    if args.build_only:
        return 0
    checksum = expected_sum()
    if args.mode == "allocations":
        count = sample(out, out / "counted", None, checksum)
        print(f"allocations_per_packet {count / PACKETS:g}")
        return 0 if count == 0 else 1
    if args.mode == "instructions":
        counts = sample(out, out / "shipping", "instructions:u", checksum)
        print(f"instructions_per_packet {counts['instructions:u'] / PACKETS:.3f}")
        return 0
    arms = {"candidate": out / "shipping"}
    if args.before:
        arms["before"] = args.before.resolve()
    events = "task-clock,cycles:u,instructions:u"
    runs = {arm: [] for arm in arms}
    for _ in range(RUNS):
        # Interleave the arms so drift lands on both.
        for arm, binary in arms.items():
            runs[arm].append(sample(out, binary, events, checksum, shielded=True))
    print(f"runs {RUNS} of {PACKETS} packets per arm")
    for arm, samples in runs.items():
        ns = [run["task-clock"] / PACKETS for run in samples]
        cycles = [run["cycles:u"] / PACKETS for run in samples]
        instructions = statistics.median(run["instructions:u"] for run in samples) / PACKETS
        ghz = statistics.median(c / n for c, n in zip(cycles, ns))
        print(f"{arm}: ns_per_packet median {statistics.median(ns):.2f} p99 {percentile(ns, 0.99):.2f}"
              f" (target {TARGET_NS}); cycles median {statistics.median(cycles):.1f}"
              f" p99 {percentile(cycles, 0.99):.1f}; instructions {instructions:.3f}; GHz {ghz:.3f}")
    candidate = [run["task-clock"] / PACKETS for run in runs["candidate"]]
    return 0 if statistics.median(candidate) <= TARGET_NS else 1

if __name__ == "__main__":
    sys.exit(main())
