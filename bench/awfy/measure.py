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
from build import ROOT


SIDES = ("hirsel", "rust", "goose")
CPU = 16
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


def stripped_size(binary):
    with tempfile.TemporaryDirectory() as temp:
        output = Path(temp) / "stripped"
        subprocess.run(["strip", "--strip-all", "-o", output, binary], check=True)
        return output.stat().st_size


def startup(command, marker, log):
    with measurement_slot(), log.open("wb") as error:
        result = subprocess.run([ROOT / "tools/capped", sys.executable, __file__,
                                 "_probe", marker.decode(), *command],
                                stdout=subprocess.PIPE, stderr=error)
    return int(result.stdout) if result.returncode == 0 else None


def runtime(command, marker, directory):
    with measurement_slot():
        result = core_measure.sample(command, directory)
    output = (directory / "stdout").read_bytes()
    if result["status"] == "ok" and not any(re.fullmatch(marker, line)
                                           for line in output.splitlines(keepends=True)):
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
    parser.add_argument("--arena", action="store_true")
    parser.add_argument("--upstream", type=Path)
    parser.add_argument("--only", nargs="+")
    args = parser.parse_args()
    if not args.arena and args.upstream is None:
        parser.error("--upstream is required for the AWFY workload sizes")
    out = args.out.resolve()
    if out.is_relative_to(ROOT):
        parser.error("use an evidence directory outside the repo")
    out.mkdir(parents=True, exist_ok=False)
    os.sched_setaffinity(0, {CPU})
    os.environ.update(HIRSEL_MEMORY_LIMIT="2G", HIRSEL_JOBS="1")
    upstream = args.upstream.resolve() if args.upstream else None
    os.chdir(args.build.resolve())
    sizes = {}
    results = {}
    cases = dict(rows(Path.cwd(), args.arena, upstream))
    if args.only and set(args.only) - cases.keys():
        parser.error("unknown benchmark selection")
    for name, commands in cases.items():
        if args.only and name not in args.only:
            continue
        result = results.setdefault(name, {})
        for side, (command, marker) in commands.items():
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
                observed.update(runtime(invocation, marker, out / stem))
            observed["startup_ns"] = startup(invocation, marker, out / f"{stem}-startup.log")
            if observed["startup_ns"] is None and observed["status"] == "ok":
                observed["status"] = "wrong-startup-output"
            print(name, side, observed["status"], flush=True)
            write_results(out, results)
    return 0 if results and all(s["status"] == "ok" for row in results.values() for s in row.values()) else 1


if __name__ == "__main__":
    raise SystemExit(main())
