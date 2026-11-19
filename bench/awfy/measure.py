#!/usr/bin/env python3
"""Run the AWFY and LangArena samples. Keep raw observations outside the repo."""
import argparse
from contextlib import contextmanager
import fcntl
import importlib.util
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import time

from arena import group, root_mib
from build import ROOT, sha256
from bench_cpus import add_cpu_sets, require_idle, validate_cpu_sets
from elf_census import STRIP_FLAGS, strip_image


SIDES = ("hirsel", "rust", "goose")
SUITES = {"awfy": "AWFY original", "pool": "AWFY pool/index", "arena": "LangArena", "compile": "Compile"}
EXPECTED = {
    "AWFY original": (
        "DeltaBlue", "Richards", "Json", "CD", "Havlak", "Bounce", "List", "Mandelbrot",
        "NBody", "Permute", "Queens", "Sieve", "Storage", "Towers",
    ),
    "AWFY pool/index": ("JsonPool", "StoragePool", "DeltaBlue", "Richards", "CD", "Havlak", "List", "Towers"),
    "LangArena": tuple(
        f"{group}::{row}"
        for group, names in {
            "Binarytrees": "Obj Arena",
            "Brainfuck": "Array Recursion",
            "Matmul": "Single T4 T8 T16",
            "Base64": "Encode Decode",
            "Json": "Generate ParseDom ParseMapping",
            "CSV": "Parse",
            "Etc": "Sieve TextRaytracer NeuralNet CacheSimulation GameOfLife Words LogParser",
            "Template": "Regex Parse",
            "Sort": "Quick Merge Self",
            "Graph": "BFS DFS AStar",
            "Hash": "SHA256 CRC32",
            "Calculator": "Ast Interpreter",
            "Maze": "Generator BFS AStar",
            "CLBG": "Fannkuchredux Mandelbrot Nbody Spectralnorm",
            "Compress": "BWTEncode BWTDecode HuffEncode HuffDecode ArithEncode ArithDecode LZWEncode LZWDecode",
            "Distance": "Jaro NGram",
        }.items()
        for row in names.split()
    ),
    "Compile": ("LangArena cold", "LangArena edit"),
    "Size": ("AWFY original-14 harness",),
    "Checkbox": ("converge, 64 clients", "memory-subscribed, 64 clients", "memory-unsubscribed, 64 clients"),
}
LOCK = Path("/tmp/hirsel-langarena-measure.lock")
HELLO = rb"Hello, world!\n"
CORE_MEASURE = importlib.util.spec_from_file_location("core_measure", ROOT / "core/tools/measure.py")
core_measure = importlib.util.module_from_spec(CORE_MEASURE)
CORE_MEASURE.loader.exec_module(core_measure)


@contextmanager
def measurement_slot():
    with LOCK.open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        yield


def probe(pattern, command):
    # The capped launcher and this measurement process start before the timer.
    # Use spawn directly so Python's child pre-exec setup is outside the sample.
    marker = re.compile(pattern.encode())
    read_fd, write_fd = os.pipe()
    try:
        actions = [(os.POSIX_SPAWN_DUP2, write_fd, 1),
                   (os.POSIX_SPAWN_CLOSE, read_fd),
                   (os.POSIX_SPAWN_CLOSE, write_fd)]
        start = time.monotonic_ns()
        pid = os.posix_spawn(command[0], command, os.environ, file_actions=actions)
    finally:
        os.close(write_fd)
    observed = None
    with os.fdopen(read_fd, "rb") as stream:
        for line in stream:
            received = time.monotonic_ns()
            if observed is None and marker.fullmatch(line):
                observed = received - start
            sys.stderr.buffer.write(line)
    _, status = os.waitpid(pid, 0)
    exit_code = os.waitstatus_to_exitcode(status)
    if observed is None or exit_code:
        print(f"missing validated output, exit {exit_code}", file=sys.stderr)
        return 1
    print(observed)
    return 0


def stripped_image(binary):
    with tempfile.TemporaryDirectory() as temp:
        output = Path(temp) / "stripped"
        strip_image(binary, output)
        return {"bytes": output.stat().st_size, "image_sha256": sha256(output),
                "binary_sha256": sha256(binary)}


def stripped_size(binary):
    return stripped_image(binary)["bytes"]


def startup(command, marker, log):
    with log.open("wb") as error:
        result = subprocess.run([ROOT / "tools/capped", sys.executable, __file__,
                                 "_probe", marker.decode(), *command],
                                stdout=subprocess.PIPE, stderr=error)
    return int(result.stdout) if result.returncode == 0 else None


def runtime(command, marker, directory, exact_output=False):
    result = core_measure.sample(command, directory)
    output = (directory / "stdout").read_bytes()
    lines = [output] if exact_output else output.splitlines(keepends=True)
    if result["status"] == "ok" and not any(re.fullmatch(marker, line) for line in lines):
        result["status"] = "wrong-output"
    result["directory"] = str(directory)
    return result


def rows(root, arena, upstream):
    if arena:
        config = root / "one-iteration.json"
        for row in json.loads(config.read_text()):
            name = row["name"]
            prefix = re.escape(name).encode()
            native = root / f"{group(name)}-{root_mib(name)}"
            marker = prefix + f" checksum={row['checksum']} expected={row['checksum']} status=OK".encode()
            yield name, {
                "hirsel": ([native, config.name, name], marker + rb"\n"),
                **{side: ([root / side, config, name], prefix + rb": OK in [0-9.]+s\n")
                   for side in ("rust", "goose")},
            }
        return
    yield "Hello", {side: ([root / f"{side}-hello"], HELLO) for side in SIDES}
    config = (upstream / "rebench.conf").read_text().split("# VMs have a name")[0]
    for name, inner in re.findall(r"^            - (\w+):\n                extra_args: (\d+)$", config, re.M):
        yield name, {
            side: ([root / f"{side}-harness", name, "1", inner],
                   name.encode() + rb": iterations=1 runtime: \d+us\n" if side == "rust" else rb"verified\n")
            for side in SIDES
        }


def pool_rows(root, upstream):
    checksums = {"Json": 156, "Storage": 5461}
    for name, commands in rows(root, False, upstream):
        if name not in checksums:
            continue
        marker = f"verified checksum={checksums[name]}\n".encode()
        for side in ("hirsel", "goose"):
            _, _, outer, inner = commands[side][0]
            commands[side] = ([root / f"{side}-pools", name + "Pool", outer, inner], marker)
        _, _, outer, inner = commands["rust"][0]
        row = name + "Pool"
        commands["rust"] = ([root / "rust-pool_index", row, outer, inner],
                            row.encode() + rb": iterations=1 runtime: \d+us\n")
        yield name, commands


def matched_pool_rows(root, upstream):
    yield from ((name + "Pool", commands) for name, commands in pool_rows(root, upstream))
    for name, commands in rows(root, False, upstream):
        if name in {"CD", "DeltaBlue", "Havlak", "List", "Richards", "Towers"}:
            yield name, {
                side: ([root / f"{side}-pool_index", *command[1:]], marker)
                for side, (command, marker) in commands.items()
            }


def runtime_cases(suite, directory, upstream):
    if suite == "pool":
        yield from matched_pool_rows(directory, upstream)
    else:
        yield from ((name, commands) for name, commands in rows(directory, suite == "arena", upstream)
                    if name != "Hello")


def size_board(build, upstream):
    """Verify the original AWFY inputs and compare each fresh harness once."""
    originals = [(name, commands) for name, commands in rows(build, False, upstream) if name != "Hello"]
    if len(originals) != 14 or len({name for name, _ in originals}) != 14:
        raise ValueError("the size board requires the 14 original AWFY workloads")
    env = {**os.environ, "HIRSEL_MEMORY_LIMIT": "2G", "HIRSEL_JOBS": "1"}
    for name, commands in originals:
        for side, (command, marker) in commands.items():
            run = subprocess.run([ROOT / "tools/capped", *command], cwd=build,
                                 env=env, stdout=subprocess.PIPE, check=True)
            if not any(re.fullmatch(marker, line) for line in run.stdout.splitlines(keepends=True)):
                raise ValueError(f"{side} did not verify {name}")
    return verified_sizes(build, [name for name, _ in originals])


def verified_sizes(build, names):
    images = {side: stripped_image(build / f"{side}-harness") for side in SIDES}
    sizes = {side: image["bytes"] for side, image in images.items()}
    strip_version = subprocess.check_output(["strip", "--version"], text=True).splitlines()[0]
    native = json.loads((build / "hirsel-harness-build.json").read_text())["native"]
    certified = native.get("step_charging_elided", False)
    return {"verified_workloads": names, "stripped_bytes": sizes, "images": images,
            "strip_version": strip_version, "strip_flags": list(STRIP_FLAGS),
            "hirsel_certified": dict.fromkeys(names, certified),
            "hirsel_over_rust": sizes["hirsel"] / sizes["rust"],
            "hirsel_over_goose": sizes["hirsel"] / sizes["goose"]}


def write_results(out, results):
    target = out / "samples.json"
    temporary = target.with_suffix(".tmp")
    temporary.write_text(json.dumps(results, indent=2) + "\n")
    temporary.replace(target)


def main():
    if len(sys.argv) > 1 and sys.argv[1] == "_probe":
        return probe(sys.argv[2], sys.argv[3:])
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("build", type=Path)
    parser.add_argument("out", type=Path)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--arena", action="store_true")
    mode.add_argument("--pool", action="store_true", help="sample only the matched Json and Storage pools")
    mode.add_argument("--size-board", action="store_true",
                      help="verify fresh original-14 harnesses and compare their stripped sizes once")
    parser.add_argument("--sides", nargs="+", choices=SIDES, default=SIDES,
                        help="reuse unchanged comparison samples when measuring another toolchain")
    parser.add_argument("--upstream", type=Path)
    parser.add_argument("--only", nargs="+")
    add_cpu_sets(parser)
    args = parser.parse_args()
    siblings = validate_cpu_sets(parser, args)
    os.sched_setaffinity(0, args.build_cpus)
    if not args.arena and args.upstream is None:
        parser.error("--upstream is required for the AWFY workload sizes")
    out = args.out.resolve()
    if out.is_relative_to(ROOT):
        parser.error("use an evidence directory outside the repo")
    with measurement_slot():
        try:
            require_idle(siblings)
        except ValueError as error:
            parser.error(str(error))
        os.sched_setaffinity(0, {args.measure_cpus[0]})
        out.mkdir(parents=True, exist_ok=False)
        return collect(parser, args, out)


def collect(parser, args, out):
    if args.size_board:
        if args.only or set(args.sides) != set(SIDES):
            parser.error("the size board requires all original workloads on all three sides")
        result = size_board(args.build.resolve(), args.upstream.resolve())
        write_results(out, result)
        print(json.dumps(result, indent=2))
        return 0
    os.environ.update(HIRSEL_MEMORY_LIMIT="2G", HIRSEL_JOBS="1")
    upstream = args.upstream.resolve() if args.upstream else None
    os.chdir(args.build.resolve())
    sizes = {}
    results = {}
    cases = dict(pool_rows(Path.cwd(), upstream) if args.pool else rows(Path.cwd(), args.arena, upstream))
    if args.only and set(args.only) - cases.keys():
        parser.error("unknown benchmark selection")
    for name, commands in cases.items():
        if args.only and name not in args.only:
            continue
        result = results.setdefault(name, {})
        for side in args.sides:
            command, marker = commands[side]
            binary = command[0]
            if not binary.is_file():
                result[side] = {"status": "missing-binary"}
                continue
            if binary not in sizes:
                sizes[binary] = stripped_size(binary)
            stem = f"{name.replace('::', '-')}-{side}"
            invocation = list(map(str, command))
            observed = result[side] = {"stripped_bytes": sizes[binary], "status": "ok"}
            if not args.arena and name != "Hello":
                observed.update(runtime(invocation, marker, out / stem,
                                        exact_output=args.pool and side != "rust"))
            if not args.pool:
                observed["startup_ns"] = startup(invocation, marker, out / f"{stem}-startup.log")
                if observed["startup_ns"] is None and observed["status"] == "ok":
                    observed["status"] = "wrong-startup-output"
            print(name, side, observed["status"], flush=True)
            write_results(out, results)
    return 0 if results and all(s["status"] == "ok" for row in results.values() for s in row.values()) else 1


if __name__ == "__main__":
    raise SystemExit(main())
