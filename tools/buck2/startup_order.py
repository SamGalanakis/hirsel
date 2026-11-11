#!/usr/bin/env python3
"""Generate lld startup orderfiles from the runtime package's serial/pool traces.

Valgrind Lackey's trace-mem records instruction fetches and data accesses
(lackey/lk_main.c:651-801). First use supplies the rank. lld
ELF/Writer.cpp:1085-1155 consumes both function and data symbols through
--symbol-ordering-file. No recorded profile is checked in.
"""
import argparse
import bisect
import json
import os
from pathlib import Path
import re
import runpy
import signal
import subprocess
import sys
import tempfile
import time


TRACE_RECORD = re.compile(r" ?[ILSM] +[0-9a-fA-F]+,[0-9]+$")
# The fixtures' own functions (package `standalone`): their names hash the
# compiler's output and occur in no other program, so they never rank.
FIXTURE_SYMBOL = re.compile(r"_H1[0-9]+standalone")


# A training takes under a minute; the whole action stops at this bound so a
# stalled child cannot hold its pool slot (an RE stall of over 30 minutes
# made it local-only in 0c09cb6d, and its cause was never found).
DEADLINE = time.monotonic() + 600


def run(command, environment, directory, stderr=None):
    process = subprocess.Popen(
        list(map(str, command)), env=environment, cwd=directory,
        stdout=subprocess.PIPE, stderr=stderr or subprocess.PIPE, start_new_session=True,
    )
    try:
        stdout, errors = process.communicate(timeout=max(0, DEADLINE - time.monotonic()))
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGKILL)
        process.communicate()
        raise RuntimeError(f"{command[0]} passed the training's 600 s deadline")
    result = subprocess.CompletedProcess(process.args, process.returncode, stdout, errors)
    if result.returncode:
        if stderr:
            stderr.flush()
            # The log interleaves Lackey's records with the diagnostics.
            detail = "".join(line for line in Path(stderr.name).open(errors="replace")
                             if not TRACE_RECORD.match(line))[-8000:]
        else:
            detail = result.stderr.decode(errors="replace")
        raise RuntimeError(f"{command[0]} exited {result.returncode}: {detail}")
    return result.stdout


def symbols(llvm, binary, environment, directory, *options):
    text = run(
        [llvm / "bin/llvm-nm", *options, "--format=posix", binary],
        environment, directory,
    ).decode()
    return [fields for line in text.splitlines() if len(fields := line.split()) == 4]


def demangle(llvm, names, environment):
    return subprocess.check_output(
        [llvm / "bin/llvm-cxxfilt"], input=("\n".join(names) + "\n").encode(),
        env=environment, timeout=max(0, DEADLINE - time.monotonic()),
    ).decode().splitlines()


def trace_symbols(trace, rows):
    ranges = {}
    for name, kind, address, size in rows:
        if kind not in "tTwWrRdD" or not int(size, 16):
            continue
        address, size = int(address, 16), int(size, 16)
        entry = ranges.setdefault(address, [address + size, []])
        entry[0] = max(entry[0], address + size)
        entry[1].append(name)
    addresses = sorted(ranges)
    for line in trace.open():
        event = re.match(r" ?[ILSM] +([0-9a-fA-F]+),", line)
        if not event:
            continue
        address = int(event[1], 16)
        index = bisect.bisect_right(addresses, address) - 1
        if index >= 0:
            end, names = ranges[addresses[index]]
            if address < end:
                yield from sorted(names)


def train(args):
    """Trace the startup fixtures linked on the x86-64 default archive; write
    each first-used symbol, in rank order, as `<raw>\t<demangled>`."""
    capped = runpy.run_path(str(args.capped))
    jobs = int(capped["granted_cpus"](int(os.environ.get("HIRSEL_JOBS") or capped["DEFAULT_JOBS"])))
    if jobs < 1:
        raise RuntimeError("startup_order requires at least one granted CPU")
    # Keep the original training bound while fitting narrower action grants.
    pool_width = min(4, jobs)
    print(f"startup_order: jobs={jobs}, pool width={pool_width}", file=sys.stderr, flush=True)
    environment = dict(
        os.environ, LD_LIBRARY_PATH=str(args.llvm / "lib"),
        LLVM_SYS_231_PREFIX=str(args.llvm), HIRSEL_LINK_KIT=str(args.kit),
        HIRSEL_GLIBC_FLOOR=str(args.glibc_floor), HIRSEL_CAPTURE_LINKER=str(args.linker),
        HIRSEL_AUDITOR_PACKAGE=str(args.runtime_auditor),
        HIRSEL_RUNTIME_ARCHIVES=str(args.archives), HIRSEL_PRODUCTS=str(args.products),
        HIRSEL_JOBS=str(jobs), HIRSEL_MEMORY_LIMIT="2G", HIRSEL_DAEMON="off",
        HIRSEL_RESULTS="none@1", VALGRIND_LIB=str(args.valgrind / "libexec/valgrind"),
        HIRSEL_RT_WORKERS="1",
    )
    selected = {}
    with tempfile.TemporaryDirectory(prefix="hirsel-startup-order-") as directory:
        directory = Path(directory)
        # Match native tests: LLVM's bin/lib files get private inodes before
        # strict selected-input hashing, independent of shared CAS hard links.
        args.llvm = Path(run(
            ["python3", args.private_native_inputs, args.llvm, directory / "llvm"],
            environment, directory,
        ).decode().strip())
        environment["LLVM_SYS_231_PREFIX"] = str(args.llvm)
        environment["LD_LIBRARY_PATH"] = str(args.llvm / "lib")
        environment["XDG_CACHE_HOME"] = str(directory / "cache")
        environment["XDG_DATA_HOME"] = str(directory / "data")
        for shape, width in [("serial", 1), ("pool", pool_width)]:
            binary = directory / shape
            started = time.monotonic()
            print(f"startup_order: building {shape}", file=sys.stderr, flush=True)
            run([args.capped, args.hirsel,
                 "build", "--release", args.source / (shape + ".hir"),
                 "-o", binary], environment, directory)
            print(f"startup_order: built {shape} in {time.monotonic() - started:.1f} s",
                  file=sys.stderr, flush=True)
            rows = symbols(args.llvm, binary, environment, directory,
                           "--numeric-sort", "--print-size")
            grants = {
                "io": {"stdout": "stdout.bytes@1", "tasks": "tasks.cooperative@1", "dirs": []},
                "placement": {"cores": 1, "commit": "shared", "reorder": "auto"},
            }
            if shape == "pool":
                grants["io"]["cpu"] = {
                    "provider": "cpu.scoped@1", "max_workers": width, "max_domains": 1,
                    "max_physical_bytes": 268435456, "max_batch_bytes": 8388608,
                }
            trace = directory / (shape + ".trace")
            with trace.open("wb") as log:
                output = run(
                    [args.capped, args.valgrind / "bin/valgrind",
                     "--tool=lackey", "--trace-mem=yes", "--basic-counts=no",
                     "--detailed-counts=no", binary, "--hirsel-grants", json.dumps(grants),
                     shape, str(width), "21"], environment, directory, stderr=log,
                )
            # One job per worker, doubling 21, 22, ... in input order.
            expected = b"42\n" if shape == "serial" else f"{width * (41 + width)}\n".encode()
            if output != expected:
                raise RuntimeError(f"{shape} startup fixture returned {output!r}")
            for name in trace_symbols(trace, rows):
                if not FIXTURE_SYMBOL.match(name):
                    selected.setdefault(name, len(selected))
        human = demangle(args.llvm, selected, environment)
    args.output.write_text("".join(f"{name}\t{readable}\n" for name, readable in zip(selected, human)))


def translate(args):
    """Order one archive's symbols by the trained ranks. Host Rust names
    resolve through their demangled form, so every target/feature archive
    gets them; CRT and anonymous constant names keep their exact matches. A
    link can omit any traced function; ordering never creates a GC root."""
    environment = dict(os.environ, LD_LIBRARY_PATH=str(args.llvm / "lib"))
    rows = [line.split("\t") for line in args.ranks.read_text().splitlines()]
    selected = {name: rank for rank, (name, _) in enumerate(rows)}
    ranks = {readable: rank for rank, (_, readable) in enumerate(rows)}
    with tempfile.TemporaryDirectory(prefix="hirsel-startup-order-") as directory:
        names = [row[0] for row in symbols(args.llvm, args.archive, environment, directory,
                                          "--defined-only")]
    translated = {}
    for name, readable in zip(names, demangle(args.llvm, names, environment)):
        if readable in ranks:
            translated[name] = ranks[readable]
    for name, rank in selected.items():
        translated.setdefault(name, rank)
    args.output.write_text("\n".join(
        name for name, rank in sorted(translated.items(), key=lambda row: (row[1], row[0]))
    ) + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    path = lambda p: Path(p).resolve()
    training = commands.add_parser("train")
    for name in ["hirsel", "products", "llvm", "kit", "linker", "glibc-floor",
                 "runtime-auditor", "valgrind", "capped", "private-native-inputs",
                 "archives", "source", "output"]:
        training.add_argument("--" + name, required=True, type=path)
    translating = commands.add_parser("translate")
    for name in ["llvm", "ranks", "archive", "output"]:
        translating.add_argument("--" + name, required=True, type=path)
    args = parser.parse_args()
    (train if args.command == "train" else translate)(args)


if __name__ == "__main__":
    main()
