#!/usr/bin/env python3
"""Check the DTLS spec's SRTP allocations and report instructions per packet."""

import argparse
import json
import os
import shlex
from pathlib import Path
import subprocess
import sys
import tempfile
import tomllib

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

PROFILES = ("cm", "gcm128", "gcm256")
OPERATIONS = (
    "protect_rtp", "protect_rtp_into", "unprotect_rtp", "protect_rtcp", "unprotect_rtcp",
    "unprotect_rtcp_clear",
)
ALL_OPERATIONS = OPERATIONS + ("seal", "open")
PACKETS = 1_000_000
ROOT_LIMIT = 1536 << 20  # The opening rows prepare up to 1.22 GB of distinct wire packets.


def phase_linker(cc, out, flavor):
    support = out / f"{flavor}-phase.o"
    define = ["-DCOUNT_ALLOCATIONS"] if flavor == "counted" else []
    subprocess.run([
        cc, "-c", "-O2", "-Wall", "-Wextra", "-Werror", *define,
        "-I", ROOT / "crates/hirsel-core-rt/include",
        HERE / "phase.c", "-o", support,
    ], check=True)
    linker = out / f"{flavor}-cc"
    linker.write_text(f'#!/bin/sh\nexec {shlex.quote(str(cc))} "$@" '
                      f'{shlex.quote(str(support))} -Wl,--wrap=hirsel_core_host_io\n')
    linker.chmod(0o755)
    return linker


def build(out, mode):
    targets = [COMPILER_TARGET, "//:core-driver-runtime-pic"]
    if mode != "allocations":
        targets.append(RUNTIME_TARGET)
    if mode != "instructions":
        targets.append(ALLOCATION_RUNTIME_TARGET)
    paths = json.loads(subprocess.check_output([
        ROOT / ".buck2/bin/buck2", "build", "-j4", "--target-platforms",
        "//tools/buck2:release", "--show-json-output", *targets,
    ], cwd=ROOT))
    compiler = ROOT / paths["root" + COMPILER_TARGET]
    cc = ROOT / "tools/buck2/toolchains/llvm-files/bin/hirsel-clang"

    source_package = ROOT / "core/programs/srtp"
    manifest = tomllib.loads((source_package / "hirsel.toml").read_text())
    core = dict(name="core", version="1")
    std = dict(name="std", version="1")
    srtp = dict(name="srtp", version=str(manifest["package"]["version"].split(".")[0]))
    app = dict(name="srtp_cost", version="1")
    document = dict(core=core, packages=[
        dict(package=core, dependencies=[], sources=library_sources(ROOT / "core/core", True)),
        dict(package=std, dependencies=[core], sources=library_sources(ROOT / "core/std", False)),
        dict(package=srtp, dependencies=[core, std], sources=[
            dict(file=str(source_package / file), modules=module.split("/"), signatures=False)
            for file, module in manifest["sources"].items()
        ]),
        dict(package=app, dependencies=[core, std, srtp], sources=[
            dict(file=str(HERE / "packets.hirsel"), modules=["main"], signatures=False),
        ]),
    ])
    manifest_path = out / "inputs.json"
    manifest_path.write_text(json.dumps(document))

    for flavor, target in (("shipping", RUNTIME_TARGET), ("counted", ALLOCATION_RUNTIME_TARGET)):
        if "root" + target not in paths:
            continue
        runtime = ROOT / paths["root" + target] / PLATFORM / "libhirsel_core_rt.a"
        linker = phase_linker(cc, out, flavor)
        subprocess.run([
            compiler, "build", "--release", "--backend", "llvm", "--source-core",
            "--source-std", "--inputs", manifest_path, "--entry", "srtp_cost@1/main/main",
            "--limit", str(ROOT_LIMIT), "--runtime", runtime, "--cc", linker, "--grant", "Stdout",
            "--store", out / "build-store", "-o", out / flavor,
        ], cwd=ROOT, check=True, env=os.environ | {"HIRSEL_MEMORY_LIMIT": "2G"})


def sample(out, binary, profile, operation, instructions):
    command = ["systemd-run", "--user", "--scope", "--quiet", "-p", "MemoryMax=2G",
               "-p", "MemorySwapMax=0", "timeout", "300"]
    perf = out / f"{profile}-{operation}.stat.csv"
    control_read, control_write = os.pipe()
    ack_read, ack_write = os.pipe()
    try:
        with tempfile.TemporaryFile() as stderr:
            if instructions:
                command += ["perf", "stat", "-x,", "-e", "instructions:u", "--delay=-1",
                            "--control", f"fd:{control_read},{ack_write}", "-o", perf, "--"]
            command += [binary, str(PROFILES.index(profile)), str(ALL_OPERATIONS.index(operation))]
            with subprocess.Popen(
                command, stdin=subprocess.PIPE,
                stdout=subprocess.PIPE, stderr=stderr,
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
                raise RuntimeError(f"{profile} {operation}: {code=} {ready=} {done=} {extra=}\n{errors}")
            if not instructions:
                return phase_allocations(errors)
    finally:
        for fd in (control_read, control_write, ack_read, ack_write):
            os.close(fd)
    for line in perf.read_text().splitlines():
        fields = line.split(",")
        if len(fields) >= 5 and fields[2] == "instructions:u" and float(fields[4]) == 100:
            return int(fields[0])
    raise RuntimeError(f"{profile} {operation}: no complete instruction count in {perf.read_text()}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path, help="scratch directory for the two native executables")
    parser.add_argument("--mode", choices=("allocations", "instructions", "both"), default="both")
    build_mode = parser.add_mutually_exclusive_group()
    build_mode.add_argument("--build-only", action="store_true")
    build_mode.add_argument("--skip-build", action="store_true")
    args = parser.parse_args()
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=True)
    if not args.skip_build:
        build(out, args.mode)
    if args.build_only:
        return 0
    print("profile,operation,allocations_per_packet,instructions_per_packet", flush=True)
    passed = True
    for profile in PROFILES:
        for operation in ALL_OPERATIONS if profile == "gcm128" else OPERATIONS:
            allocations = instructions = ""
            if args.mode != "instructions" and operation in OPERATIONS:
                count = sample(out, out / "counted", profile, operation, False)
                allocations = f"{count / PACKETS:g}"
                passed &= count == 0
            if args.mode != "allocations":
                count = sample(out, out / "shipping", profile, operation, True)
                instructions = f"{count / PACKETS:.3f}"
            if allocations or instructions:
                print(f"{profile},{operation},{allocations},{instructions}", flush=True)
    return 0 if passed else 1


if __name__ == "__main__":
    sys.exit(main())
