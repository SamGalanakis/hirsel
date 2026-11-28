#!/usr/bin/env python3
"""Check the TURN spec's cost targets for the relay's data path (docs/spec/turn.md, Cost targets).

- allocations: a counted allocator over 10^6 relayed packets of 1,200 bytes each way
  (from_client on ChannelData, from_peer on a bound channel), after setup. Target: 0.
- recording: the same run recorded at 10^6 and at 2 * 10^6 packets. Target: the two tapes
  have the same size, so no frame is written per packet (no sink call per packet).
- instructions: instructions per packet, reported and gating nothing, with 1 allocation and
  with ALLOCATIONS_WIDE allocations live (flat: the relay finds allocations by key).
"""

import argparse
import json
import os
from pathlib import Path
import shlex
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

RECORD_RUNTIME_TARGET = "//:hirsel-core-rt-record-archives"
OPERATIONS = ("from_client", "from_peer")
PACKETS = 1_000_000
ALLOCATIONS_WIDE = 1000
LIMIT = 512 << 20


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


def program(directory, dependencies):
    manifest = hcfg.package(directory)
    return dict(package=dict(name=manifest["name"], version=str(manifest["version"].split(".")[0])),
                dependencies=dependencies,
                sources=[dict(file=str(directory / f), modules=m.split("/"), signatures=False)
                         for f, m in manifest["sources"].items()])


def build(out, counters_only=False):
    targets = [COMPILER_TARGET, "//:core-driver-runtime-pic", RUNTIME_TARGET, ALLOCATION_RUNTIME_TARGET]
    if not counters_only:
        targets.append(RECORD_RUNTIME_TARGET)
    paths = json.loads(subprocess.check_output([
        ROOT / ".buck2/bin/buck2", "build", "-j4", "--target-platforms",
        "//tools/buck2:release", "--show-json-output", *targets,
    ], cwd=ROOT))
    compiler = ROOT / paths["root" + COMPILER_TARGET]
    cc = ROOT / "tools/buck2/toolchains/llvm-files/bin/hirsel-clang"
    core, std = dict(name="core", version="1"), dict(name="std", version="1")
    stun, turn = dict(name="stun_ice", version="1"), dict(name="turn", version="1")
    app = dict(name="turn_cost", version="1")
    document = dict(core=core, packages=[
        dict(package=core, dependencies=[], sources=library_sources(ROOT / "core/core", True)),
        dict(package=std, dependencies=[core], sources=library_sources(ROOT / "core/std", False)),
        program(ROOT / "core/programs/stun_ice", [core, std]),
        program(ROOT / "core/programs/turn", [core, std, stun]),
        dict(package=app, dependencies=[core, std, stun, turn], sources=[
            dict(file=str(HERE / "relay_packets.hirsel"), modules=["main"], signatures=False),
        ]),
    ])
    manifest_path = out / "inputs.json"
    manifest_path.write_text(json.dumps(document))
    flavors = [("shipping", RUNTIME_TARGET, True), ("counted", ALLOCATION_RUNTIME_TARGET, True)]
    if not counters_only:
        flavors.append(("recorded", RECORD_RUNTIME_TARGET, False))
    for flavor, target, phased in flavors:
        runtime = ROOT / paths["root" + target] / PLATFORM / "libhirsel_core_rt.a"
        linker = phase_linker(cc, out, flavor) if phased else cc
        recording = ["--recording"] if flavor == "recorded" else []
        subprocess.run([
            compiler, "build", "--release", "--backend", "llvm", "--source-core",
            "--source-std", "--inputs", manifest_path, "--entry", "turn_cost@1/main/main", *recording,
            "--limit", str(LIMIT), "--runtime", runtime, "--cc", linker, "--grant", "Stdout",
            "--grant", "Random", "--store", out / "build-store", "-o", out / flavor,
        ], cwd=ROOT, check=True, env=os.environ | {"HIRSEL_MEMORY_LIMIT": "2G"})


def scoped():
    return ["systemd-run", "--user", "--scope", "--quiet", "-p", "MemoryMax=2G",
            "-p", "MemorySwapMax=0", "timeout", "300"]


def sample(out, binary, operation, allocations, instructions):
    command = scoped()
    perf = out / f"{operation}-{allocations}.stat.csv"
    control_read, control_write = os.pipe()
    ack_read, ack_write = os.pipe()
    argv = [binary, str(OPERATIONS.index(operation)), str(allocations), str(PACKETS)]
    try:
        with tempfile.TemporaryFile() as stderr:
            if instructions:
                command += ["perf", "stat", "-x,", "-e", "instructions:u", "--delay=-1",
                            "--control", f"fd:{control_read},{ack_write}", "-o", perf, "--"]
            with subprocess.Popen(
                [*command, *argv], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=stderr,
                pass_fds=(control_read, ack_write) if instructions else (),
                env=os.environ | {"HIRSEL_MEMORY_LIMIT": "2G"},
            ) as process:
                ready = process.stdout.readline()
                done = b""
                if ready == b"READY\n":
                    if instructions:
                        counter_command((control_write, ack_read), "enable")
                    process.stdin.write(b"x")
                    process.stdin.flush()
                    done = process.stdout.readline()
                    if done == f"DONE {PACKETS}\n".encode():
                        if instructions:
                            counter_command((control_write, ack_read), "disable")
                        process.stdin.write(b"x")
                        process.stdin.flush()
                process.stdin.close()
                extra = process.stdout.read()
                code = process.wait()
            stderr.seek(0)
            errors = stderr.read().decode(errors="replace")
            if code or ready != b"READY\n" or done != f"DONE {PACKETS}\n".encode() or extra:
                raise RuntimeError(f"{operation} {allocations}: {code=} {ready=} {done=} {extra=}\n{errors}")
            if not instructions:
                return phase_allocations(errors)
    finally:
        for fd in (control_read, control_write, ack_read, ack_write):
            os.close(fd)
    for line in perf.read_text().splitlines():
        fields = line.split(",")
        if len(fields) >= 5 and fields[2] == "instructions:u" and float(fields[4]) == 100:
            return int(fields[0])
    raise RuntimeError(f"{operation}: no complete instruction count in {perf.read_text()}")


def tape_size(out, operation, packets):
    tape = out / f"{operation}-{packets}.tape"
    tape.unlink(missing_ok=True)
    result = subprocess.run(
        [*scoped(), out / "recorded", str(OPERATIONS.index(operation)), "1", str(packets)],
        capture_output=True, env=os.environ | {"HIRSEL_MEMORY_LIMIT": "2G", "HIRSEL_RECORD": str(tape)})
    if result.returncode or result.stdout != f"READY\nDONE {packets}\n".encode():
        raise RuntimeError(f"recorded {operation} {packets}: {result.returncode} {result.stdout!r} {result.stderr[-2000:]!r}")
    return tape.stat().st_size


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("output", type=Path, help="scratch directory for the native executables")
    parser.add_argument("--counters-only", action="store_true", help="skip recording and tape-size checks")
    build_mode = parser.add_mutually_exclusive_group()
    build_mode.add_argument("--build-only", action="store_true")
    build_mode.add_argument("--skip-build", action="store_true")
    args = parser.parse_args()
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=True)
    if not args.skip_build:
        build(out, args.counters_only)
    if args.build_only:
        return 0
    passed = True
    print("operation,allocations_live,allocations_per_packet,tape_bytes_1e6,tape_bytes_2e6,instructions_per_packet",
          flush=True)
    for operation in OPERATIONS:
        count = sample(out, out / "counted", operation, 1, False)
        small, large = "", ""
        if not args.counters_only:
            small, large = tape_size(out, operation, PACKETS), tape_size(out, operation, 2 * PACKETS)
        passed &= count == 0 and small == large
        narrow = sample(out, out / "shipping", operation, 1, True)
        print(f"{operation},1,{count / PACKETS:g},{small},{large},{narrow / PACKETS:.1f}", flush=True)
        wide = sample(out, out / "shipping", operation, ALLOCATIONS_WIDE, True)
        print(f"{operation},{ALLOCATIONS_WIDE},,,,{wide / PACKETS:.1f}", flush=True)
    return 0 if passed else 1


if __name__ == "__main__":
    sys.exit(main())
