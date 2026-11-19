#!/usr/bin/env python3
"""Measure private checkbox session residency before, during and after a checked load."""
import argparse
import gzip
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

import board
import build
import receipt
from bench_cpus import add_cpu_sets, positive_int, require_idle, validate_cpu_sets

MODES = ("memory-subscribed", "memory-unsubscribed")
POINTS = (64, 1000, 10000)


def resident_bytes(directory):
    return {
        phase.name: int(next(line for line in (phase / "smaps_rollup").read_text().splitlines()
                             if line.startswith("Rss:")).split()[1]) * 1024
        for phase in sorted(directory.iterdir())
    }


def retain_snapshots(scratch, out):
    for path in scratch.glob("*/*/*"):
        case, phase, name = path.relative_to(scratch).parts
        target = out / case / "snapshots" / phase / name
        target.parent.mkdir(parents=True, exist_ok=True)
        if name == "smaps":
            with path.open("rb") as source, gzip.open(target.with_suffix(".gz"), "wb", compresslevel=1) as compressed:
                shutil.copyfileobj(source, compressed)
        else:
            shutil.copyfile(path, target)


def sample(work, output, snapshots, prepared, mode, clients, server_cpu, loader_cpu):
    checkbox, executable, address, wants, tools = prepared
    admission = str(max(20_000, clients))
    overrides = ["--per-ip-sessions", admission, "--per-ip-new-principals", admission, "--principal-rate", "1000000000"]
    env = build.capped_env(work)
    command = checkbox.command(executable, address, overrides)
    output.mkdir()
    try:
        with board.private_server(command, address, output, env, server_cpu) as pid:
            load = [tools / "wire-load", mode, "--url", f"ws://{address}/hirsel", "--wants", wants,
                    "--clients", str(clients), "--server-pid", str(pid), "--server-snapshots", snapshots]
            result = subprocess.run(["taskset", "-c", str(loader_cpu), board.CAP, *load], env=env, capture_output=True)
            (output / "client.stdout").write_bytes(result.stdout)
            (output / "client.stderr").write_bytes(result.stderr)
        row = receipt.read(output / "client.stdout")
        complete = (result.returncode == 0 and row["complete"] is True and row["connections"] == clients
                    and row["subscribed"] is (mode == "memory-subscribed") and row["drained"] is True)
        return row | {"complete": complete, "exit": result.returncode, "rss_bytes": resident_bytes(snapshots)}
    finally:
        # The executable's directory grant refers to this one path. Clear its
        # private WAL here so the next run seeds and checks the same payload.
        shutil.rmtree(work / "data")
        (work / "data").mkdir()


def collect(args, work):
    artifacts = receipt.read(args.artifacts)
    hashes = receipt.release_tools(artifacts)
    prepared = board.prepare_checkbox(work, artifacts, build.capped_env(work, "8G", "2") | {
        "LD_LIBRARY_PATH": str(build.llvm_path() / "lib"),
    }, wire_tools=args.wire_tools)
    _, executable, _, wants, tools = prepared
    report = {
        **hashes,
        "server_sha256": build.sha256(executable),
        "loader_sha256": build.sha256(tools / "wire-load"),
        "wants_sha256": build.sha256(wants),
        "server_cpu": args.measure_cpus[0], "loader_cpu": args.measure_cpus[1],
        "points": args.clients, "complete": False, "rows": [], "highest_complete": dict.fromkeys(args.mode, 0),
    }
    target = args.out / "memory-sweep.json"
    snapshots = work.parent / "snapshots"
    with receipt.measurement_slot(args.lock):
        try:
            os.sched_setaffinity(0, args.measure_cpus[:2])
            require_idle(args.siblings)
            for clients in args.clients:
                for mode in args.mode:
                    directory = args.out / f"{mode}-{clients}"
                    try:
                        row = sample(work, directory, snapshots / directory.name, prepared, mode, clients,
                                     *args.measure_cpus[:2])
                    except (OSError, ValueError, RuntimeError, KeyError, StopIteration, subprocess.SubprocessError) as error:
                        row = {"complete": False, "error": str(error)}
                    report["rows"].append({"mode": mode, "clients": clients, **row})
                    if row["complete"]:
                        report["highest_complete"][mode] = clients
                    receipt.write(target, report)
                    print(f"{mode} {clients} {'complete' if row['complete'] else 'incomplete'}", flush=True)
        finally:
            os.sched_setaffinity(0, args.build_cpus)
    retain_snapshots(snapshots, args.out)
    report["complete"] = all(row["complete"] for row in report["rows"])
    receipt.write(target, report)
    print(target)
    return int(not report["complete"])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("out", type=lambda path: Path(path).resolve())
    parser.add_argument("--artifacts", type=Path, required=True)
    parser.add_argument("--wire-tools", type=lambda path: Path(path).resolve(), help="prebuilt wire release directory")
    parser.add_argument("--mode", nargs="+", choices=MODES, default=MODES)
    parser.add_argument("--clients", type=positive_int, nargs="+", default=POINTS)
    add_cpu_sets(parser)
    parser.add_argument("--lock", type=Path, default=board.LOCK)
    args = parser.parse_args()
    args.siblings = validate_cpu_sets(parser, args, width=2)
    if args.out.is_relative_to(build.ROOT):
        parser.error("use a new evidence directory outside the checkout")
    args.clients = sorted(set(args.clients))
    args.mode = list(dict.fromkeys(args.mode))
    os.sched_setaffinity(0, args.build_cpus)
    args.out.mkdir(parents=True)
    with tempfile.TemporaryDirectory(prefix="hirsel-checkbox-memory-") as scratch:
        return collect(args, Path(scratch) / "checkbox")


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError, KeyError, subprocess.SubprocessError) as error:
        print(f"checkbox-memory: {error}", file=sys.stderr)
        raise SystemExit(1)
