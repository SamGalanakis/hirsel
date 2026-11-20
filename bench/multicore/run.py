#!/usr/bin/env python3
"""Measure one process-tree sample per workload, language, and worker count."""
import argparse
from contextlib import contextmanager, nullcontext
from datetime import datetime
from collections import Counter
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import signal
import socket
import time
import statistics
import struct
import subprocess
import sys

if __package__:
    from . import regimes
    from .build import GAME_ROWS, ROOT, STANDARD_RUST_SIZES
else:
    import regimes
    from build import GAME_ROWS, ROOT, STANDARD_RUST_SIZES

sys.path.insert(0, str(ROOT / "core/tools"))
from bench_cpus import add_cpu_sets, cpu_list, sibling_cpus, smt_activity, validate_cpu_sets
from std_native import wait_load

LOCK = Path("/tmp/hirsel-langarena-measure.lock")
WIDTHS = (1, 2, 4, 8, 12, 16)
LANGUAGES = ("Hirsel", "Rust", "Tokio", "Go", "Goose")
SIZES = {
    "mandelbrot": STANDARD_RUST_SIZES["mandelbrot"],
    "spectralnorm": 1600,
    "fannkuchredux": 11,
    "knucleotide": 120000,
    "binarytrees": 16,
    "spawn_join": 10000,
    "spawn_chain": 1000,
    "ping_pong": 100000,
    "yield_many": 12500,
    "fan_in": 12500,
}
S8_SIZES = {"fan_in_cap1": 12500, "ping_pong_burst": 100000}
S0_SIZES = {
    "matmul_t16": 4,
    "spawn_join": 10000,
    "ping_pong": 100000,
    "arc_pair": 1000000,
}
# Fields are completed iterations and checksum. Matmul uses the center
# dot product -814846225/33554432, rounded to "-24.2843099"; the other
# three checksums are the sum of the integers from 1 through the count.
S0_WORK = {
    "matmul_t16": b"4 1286528612\n",
    "spawn_join": b"10000 50005000\n",
    "ping_pong": b"100000 5000050000\n",
    "arc_pair": b"1000000 500000500000\n",
}
FASTA_SHA = "eb604786ef13cc1012c3fc3cc760a4a159bfbae30cf6e12bf37792799b1df24e"
STANDARD_FASTA_SHA = "3fcf4f78104c8a65ef210fe1d469f4e473456c791225f2f1f9114f4986aa09fa"
# Exact outputs for the pinned Rust programs at the published inputs.
STANDARD_OUTPUT = {
    "mandelbrot": "609262469ee6a0262ccd03932e557f745c9e7b997ad17835a02a0232a64807be",
    "spectralnorm": "f9d5b5e3eb7657cf1bbba4cc856651864df9cd9fd9a6be9b9bc5fcbb67150deb",
    "fannkuchredux": "4265a65135c506a68d90d6474003fb9030b7ee244a06c046bd89b3932a28ce20",
    "knucleotide": "f8c042fe770dda8f870b967605ea40ca2e353f2bdf92e89a327209dd31d445c7",
    "binarytrees": "341de11a51feab3d8122b4b5d6a68b038a2d14434aa9bc2372f39300bf5f48e1",
}
QUERIES = (b"GGT", b"GGTA", b"GGTATT", b"GGTATTTTAATT", b"GGTATTTTAATTTATAGT")


def digest(data):
    return hashlib.sha256(data).hexdigest()


def expected(case, fasta, standard=False):
    if standard:
        if case == "knucleotide":
            with fasta.open("rb") as file:
                if hashlib.file_digest(file, "sha256").hexdigest() != STANDARD_FASTA_SHA:
                    raise ValueError(f"{fasta} does not match the standard FASTA input")
        return STANDARD_OUTPUT[case]
    n = (SIZES | S8_SIZES)[case]
    if case == "mandelbrot":
        return STANDARD_OUTPUT[case]
    if case == "spectralnorm":
        return digest(b"1.274224152\n")
    if case == "fannkuchredux":
        return digest(b"556355\nPfannkuchen(11) = 51\n")
    if case == "binarytrees":
        def nodes(depth):
            return (1 << (depth + 1)) - 1

        lines = [f"stretch tree of depth {n + 1}\t check: {nodes(n + 1)}"]
        for depth in range(4, n + 1, 2):
            trees = 1 << (n - depth + 4)
            lines.append(f"{trees}\t trees of depth {depth}\t check: {trees * nodes(depth)}")
        lines.append(f"long lived tree of depth {n}\t check: {nodes(n)}")
        return digest(("\n".join(lines) + "\n").encode())
    if case == "knucleotide":
        data = fasta.read_bytes()
        if digest(data) != FASTA_SHA:
            raise ValueError(f"{fasta} does not match FASTA Go #1 at n={n}")
        _, three = data.split(b"\n>THREE", 1)
        bases = b"".join(three.splitlines()[1:]).upper()
        counts = tuple(Counter(bases[i:i + k] for i in range(len(bases) - k + 1))
                       for k in (1, 2))
        queries = [str(len(re.findall(b"(?=" + query + b")", bases))).encode() + b"\t" + query
                   for query in QUERIES]
        return counts, b"\n".join(queries) + b"\n"
    if case in {"spawn_chain", "yield_many", "fan_in", "fan_in_cap1"}:
        n *= 16
    return digest(f"{n * (n + 1) // 2}\n".encode())


def matches(output, wanted):
    if isinstance(wanted, str):
        return digest(output) == wanted
    counts, queries = wanted
    sections = output.split(b"\n\n")
    if len(sections) != 3 or sections[2] != queries:
        return False

    def f32(value):
        return struct.unpack("f", struct.pack("f", value))[0]

    for section, frequencies in zip(sections, counts):
        total = frequencies.total()
        remaining = dict(frequencies)
        last = total
        for line in section.splitlines():
            key, space, percent = line.partition(b" ")
            count = remaining.pop(key, None)
            if not space or count is None or count > last:
                return False
            # The pinned peers use both float32 and float64.
            precise = 100 * count / total
            rounded32 = f32(f32(100 * count) / f32(total))
            if percent not in {f"{number:.3f}".encode() for number in (precise, rounded32)}:
                return False
            last = count
        if remaining:
            return False
    return True


def sample_command(bins, case, language, width, size, s0=False):
    if s0:
        if case == "matmul_t16" and language == "Hirsel":
            return [bins / "hirsel/matmul_t16", size]
        if language == "Rust":
            return [bins / "rust/s0", case, width, size]
        if language == "Tokio":
            return [bins / "tokio", case, width, size, "work-receipt"]
        return [bins / "hirsel/runtime", case, size, "work-receipt"]
    runtime = case not in GAME_ROWS
    if language == "Tokio":
        return [bins / "tokio", case, width, size]
    directory = bins / language.lower()
    if runtime:
        args = [case, size]
        if language == "Go":
            args.insert(1, width)
        return [directory / "runtime", *args]
    if case == "knucleotide":
        args = {"Hirsel": ["input.fasta"], "Goose": [width]}.get(language, [])
    else:
        args = [size]
        if language == "Goose":
            args.append(width)
    return [directory / case, *args]


def sample_stdin(bins, case, language):
    return bins / "input.fasta" if case == "knucleotide" and language != "Hirsel" else Path(os.devnull)


def applicable(case, s0=False):
    if case in {"matmul_t16", "arc_pair"}:
        return ("Hirsel", "Rust")
    if s0 or case in S8_SIZES:
        return ("Hirsel", "Tokio")
    if case == "yield_many":
        return ("Tokio", "Go")
    if case in GAME_ROWS:
        return ("Hirsel", "Rust", "Go", "Goose")
    return ("Hirsel", "Tokio", "Go", "Goose")


def sample_env(width, language, test_pool):
    env = os.environ | {
        "LC_ALL": "C",
        "HIRSEL_MEMORY_LIMIT": "2G",
        "HIRSEL_JOBS": str(width),
        "GOMAXPROCS": str(width),
        "RAYON_NUM_THREADS": str(width),
    }
    if test_pool and language == "Hirsel":
        env["HIRSEL_S8_STATS"] = "1"
        env["HIRSEL_S8_WORKERS"] = str(width)
    return env


def measured_command(directory, cpus, command, profile=False, cycles=False):
    if profile:
        perf = ["perf", "record", "-N", "--sample-cpu", "-e", "cycles:u", "-F", "3997",
                "--call-graph", "dwarf,8192", "-o", directory / "perf.data"]
    else:
        events = "instructions:u,duration_time" + (",cycles:u" if cycles else "")
        perf = ["perf", "stat", "-x", ";", "-e", events,
                "-o", directory / "perf.csv"]
    return ["taskset", "-c", ",".join(map(str, cpus)), str(ROOT / "tools/capped"),
            *map(str, perf), "--", "/usr/bin/time", "-f", "%M", "-o", str(directory / "rss.txt"),
            *map(str, command)]


def observe(bins, out, case, language, cpus, size, wanted, kernel, lock_path, s0=False,
            test_pool=False, *, command=None, profile=False, allow_smt=False, cycles=False):
    width = len(cpus)
    siblings = sibling_cpus(cpus, allow_smt=allow_smt)
    command = command or sample_command(bins, case, language, width, size, s0)
    if not command[0].is_file():
        return {"status": "missing-binary"}
    directory = out / f"{case}-{language.lower()}-{width}"
    directory.mkdir()
    launch = measured_command(directory, cpus, command, profile, cycles)
    stdin = sample_stdin(bins, case, language)
    if lock_path:
        wait_load()
    with lock_path.open("a") if lock_path else nullcontext() as lock:
        if lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
        with (smt_activity(siblings, f"{case}/{language}/{width}") as busy, stdin.open("rb") as inp,
              (directory / "stdout").open("wb") as out_file, (directory / "stderr").open("wb") as err):
            result = subprocess.run(launch, cwd=ROOT, env=sample_env(width, language, test_pool),
                                    stdin=inp, stdout=out_file, stderr=err)
    cpu_list = ",".join(map(str, cpus))
    row = {"status": "ok", "cpus": cpu_list, "smt_busy_percent": busy}
    if result.returncode:
        if case == "mandelbrot":
            (directory / "stdout").unlink(missing_ok=True)
        row["status"] = f"exit:{result.returncode}"
        return row
    if test_pool and language == "Hirsel" and width > 1:
        receipt = f"pool workers={width} "
        if not any(line.startswith(receipt) for line in (directory / "stderr").read_text().splitlines()):
            row["status"] = "missing-test-pool-receipt"
            return row
    if profile:
        return row | {"status": "ok" if (directory / "stdout").read_bytes() == wanted else "wrong-output"}
    return validate_sample(directory, case, wanted, cpu_list, kernel) | {"smt_busy_percent": busy}


def validate_sample(directory, case, wanted, cpus, kernel=False):
    stdout = directory / "stdout"
    if case == "mandelbrot":
        # One standard PBM is 32 MB. Retain its exact digest instead of the bitmap.
        with stdout.open("rb") as file:
            sha256 = hashlib.file_digest(file, "sha256").hexdigest()
        stdout.unlink()
        valid = sha256 == wanted
    else:
        output = stdout.read_bytes()
        sha256 = digest(output)
        valid = output == wanted if isinstance(wanted, bytes) else matches(output, wanted)
    row = {"status": "ok", "cpus": cpus, "sha256": sha256}
    if not valid:
        row["status"] = "wrong-output"
        return row
    if isinstance(wanted, bytes):
        iterations, checksum = map(int, output.split())
        row.update(iterations=iterations, checksum=checksum)
    counters = {}
    perf = directory / "perf.csv"
    if perf.is_file():
        for line in perf.read_text().splitlines():
            fields = line.split(";")
            if len(fields) >= 3 and fields[0].strip().isdigit():
                counters[fields[2]] = int(fields[0])
    if any(counters.get(event, 0) <= 0 for event in ("instructions:u", "duration_time")):
        row["status"] = "missing-counter"
        return row
    rss = directory / "rss.txt"
    # GNU time may print a signal diagnostic before the final RSS line.
    rss_lines = rss.read_text().splitlines() if rss.is_file() else []
    if not rss_lines or not rss_lines[-1].isdigit():
        row["status"] = "missing-rss"
        return row
    if kernel:
        durations = re.findall(r"^multicore-kernel-ns=(\d+)$", (directory / "stderr").read_text(), re.M)
        if len(durations) != 1 or not 0 < int(durations[0]) <= counters["duration_time"]:
            row["status"] = "missing-or-invalid-kernel-time"
            return row
        row["kernel_seconds"] = int(durations[0]) / 1e9
    if "cycles:u" in counters:
        row["cycles"] = counters["cycles:u"]
    return row | {
        "instructions": counters["instructions:u"],
        "seconds": counters["duration_time"] / 1e9,
        "max_rss_kib": int(rss_lines[-1]),
    }


def table(rows):
    index = {(s["case"], s["language"], s["width"]): s for s in rows}
    widths = [width for width in WIDTHS if any(row["width"] == width for row in rows)]

    def value(case, language, width, field):
        row = index.get((case, language, width), {})
        if row.get("status") == "ok":
            return row.get(field)
        return None

    def ratio(left, right):
        return left / right if left and right else None

    def fmt(number):
        return f"{number:.3f}" if number is not None else "—"

    cases = [case for case in SIZES | S0_SIZES if any(row["case"] == case for row in rows)]
    if any(row["language"] == "Hirsel" for row in rows):
        peers = LANGUAGES[1:]
        print("| Workload | " + " | ".join(f"Hirsel/{peer} I at 1" for peer in peers) + " |")
        print("| --- |" + " ---: |" * len(peers))
        cohorts = {peer: [] for peer in peers}
        for case in cases:
            left = value(case, "Hirsel", 1, "instructions")
            ratios = []
            for peer in peers:
                r = ratio(left, value(case, peer, 1, "instructions"))
                if r is not None:
                    cohorts[peer].append(r)
                ratios.append(fmt(r))
            print(f"| {case} | " + " | ".join(ratios) + " |")
        aggregates = [f"{fmt(statistics.geometric_mean(rs))} ({len(rs)})" if rs else "—"
                      for rs in cohorts.values()]
        print("| Geomean (rows) | " + " | ".join(aggregates) + " |")

    measures = [("seconds", "Process wall speedup vs own 1-worker sample"),
                ("max_rss_kib", "Peak RSS in MiB")]
    if any("kernel_seconds" in row for row in rows):
        measures.insert(0, ("kernel_seconds", "Kernel speedup vs own 1-worker sample"))
    for field, heading in measures:
        print(f"\n{heading}\n")
        print("| Workload | Language | " + " | ".join(map(str, widths)) + " |")
        print("| --- | --- |" + " ---: |" * len(widths))
        for case in cases:
            for language in applicable(case):
                initial = value(case, language, 1, field)
                cells = []
                for width in widths:
                    observed = value(case, language, width, field)
                    metric = ratio(initial, observed) if field != "max_rss_kib" else (
                        observed / 1024 if observed is not None else None
                    )
                    cells.append(fmt(metric))
                if field == "kernel_seconds" and not any(
                    "kernel_seconds" in index.get((case, language, width), {}) for width in widths
                ):
                    continue
                if any((case, language, width) in index for width in widths):
                    print(f"| {case} | {language} | " + " | ".join(cells) + " |")
    if any(row["language"] == "Tokio" for row in rows):
        print("\nHirsel/Tokio multi-thread wall ratio\n")
        print("| Workload | " + " | ".join(map(str, widths)) + " |")
        print("| --- |" + " ---: |" * len(widths))
        for case in cases:
            if "Tokio" in applicable(case) and "Hirsel" in applicable(case):
                cells = [fmt(ratio(value(case, "Hirsel", w, "seconds"),
                                   value(case, "Tokio", w, "seconds"))) for w in widths]
                print(f"| {case} | " + " | ".join(cells) + " |")

    failures = [s for s in rows if s["status"] != "ok"]
    if failures:
        print("\nUnavailable samples\n")
        for row in failures:
            print(f"- {row['case']} {row['language']} {row['width']}: {row['status']}")


def stderr_metrics(directory):
    return {name: int(value) for name, value in
            re.findall(r"^([a-z][a-z0-9-]*)=(\d+)$", (directory / "stderr").read_text(), re.M)}


def pool_counters(directory, case, size):
    counters = Counter()
    parks = []
    for line in (directory / "stderr").read_text().splitlines():
        if line.startswith(("sequencer worker=", "pool workers=")):
            for key, value in re.findall(r"\b([a-z_][a-z_0-9]*)=(\d+)", line):
                if key not in {"worker", "workers"}:
                    counters[key] += int(value)
        if line.startswith("sequencer worker="):
            parks = [max(a, b) for a, b in zip(
                parks or [0] * 5, map(int, re.search(r"parks=\[([0-9, ]+)\]", line)[1].split(",")))]
    if counters:
        counters["parks"] = parks
        if operations := regimes.ordered_operations(case, size):
            counters["commits_per_workload_operation"] = counters["commits"] / operations
    return dict(counters)


def stop_server(pidfile, binary, sig):
    pid = int(pidfile.read_text())
    details = subprocess.run(["ps", "-o", "comm=", "-p", str(pid)], capture_output=True, text=True)
    exe = Path(f"/proc/{pid}/exe")
    try:
        same = exe.samefile(binary)
    except FileNotFoundError:
        return
    if details.stdout.strip() != binary.name[:15] or not same:
        raise RuntimeError(f"server identity changed for {pid}")
    os.kill(pid, sig)


def listening(process, directory, port):
    while process.poll() is None:
        if port == 0:
            match = re.search(r"^listening=(\d+)$", (directory / "stderr").read_text(), re.M)
            if match:
                return int(match[1])
        else:
            try:
                with socket.create_connection(("127.0.0.1", port)):
                    return port
            except ConnectionRefusedError:
                pass
        time.sleep(0.01)
    raise RuntimeError(f"server exited before listen ({process.returncode})")


def observe_service(bins, directory, case, language, cpus, load_cpus, size, rounds, wanted, *,
                    profile=False, allow_smt=False, cycles=False):
    width = len(cpus)
    checkbox = case == "checkbox"
    work = regimes.WORKLOADS[case]
    directory.mkdir()
    pidfile = directory / "server.pid"
    server_env = sample_env(width, language, True)
    if checkbox:
        board = bins / "checkbox"
        port = int((board / "port").read_text())
        listen = f"127.0.0.1:{port}"
        options = ["--listen", listen, "--per-ip-sessions", "20000",
                   "--per-ip-new-principals", "20000", "--principal-rate", "1000000000"]
        if language == "Hirsel":
            # The grant names this path at link time. Move the old tree aside
            # between samples and retain it with that sample's raw evidence.
            if any((board / "data").iterdir()):
                raise RuntimeError("checkbox build directory contains a previous store")
            server = [board / "checkbox", *options]
        else:
            server_env["TOKIO_WORKER_THREADS"] = str(width)
            server = [bins / "wire/release/wire-reference", *options,
                      "--wants", board / "wants.bin", "--n-place", "1048576",
                      "--wal", directory / "checkbox.wal"]
        load = [bins / "wire/release/wire-load", "converge", "--url", f"ws://{listen}/hirsel",
                "--wants", board / "wants.bin", "--clients", work.units, "--toggles", size, "--pace", "1000"]
    else:
        port = 0
        server = regimes.command(bins, "socket_server", language, width, size, rounds, port, work.connections)
    (directory / "command").write_text(shlex.join(map(str, server)) + "\n")
    launch = measured_command(directory, cpus, [
        "sh", "-c", 'printf "%s\\n" "$$" > "$1"; shift; exec "$@"', "server", pidfile, *server,
    ], profile, cycles)
    cpus_text = ",".join(map(str, cpus))
    data = {"cpus": cpus_text, "load_cpus": ",".join(map(str, load_cpus))}
    with (smt_activity(sibling_cpus(cpus, allow_smt=allow_smt), f"{case}/{language}/{width}") as busy,
          (directory / "stdout").open("wb") as output, (directory / "stderr").open("wb") as err,
          subprocess.Popen(launch, cwd=ROOT, env=server_env, stdout=output, stderr=err) as process):
        try:
            port = listening(process, directory, port)
            if not checkbox:
                load = regimes.command(bins, "socket_load", "Tokio", len(load_cpus), size,
                                       rounds, port, work.connections)
            command = ["taskset", "-c", data["load_cpus"], ROOT / "tools/capped", *load]
            (directory / "load-command").write_text(shlex.join(map(str, command)) + "\n")
            with ((directory / "load.stdout").open("wb") as reply,
                  (directory / "load.stderr").open("wb") as load_err):
                start = time.monotonic()
                result = subprocess.run(list(map(str, command)), cwd=ROOT, stdout=reply, stderr=load_err,
                                        env=sample_env(len(load_cpus), "Tokio", False))
                data["client_seconds"] = time.monotonic() - start
            data["load_status"] = result.returncode
        except RuntimeError as error:
            data["start_error"] = str(error)
        finally:
            if pidfile.exists() and (checkbox or data.get("load_status") != 0):
                stop_server(pidfile, server[0], signal.SIGINT)
    if checkbox and language == "Hirsel":
        shutil.move(board / "data", directory / "data")
        (board / "data").mkdir()
    data |= {"smt_busy_percent": busy, "server_status": process.returncode}
    if "start_error" in data:
        return data | {"status": "server-start-failed"}
    if data["load_status"]:
        return data | {"status": f"load-exit:{data['load_status']}"}
    reply = (directory / "load.stdout").read_bytes()
    if checkbox:
        match = re.search(rb"Returned=(\d+) .*checksum=(\d+)", reply)
        receipt = b" ".join(match.groups()) + b"\n" if match else b""
        if receipt != wanted:
            return data | {"status": "wrong-output"}
        if process.returncode not in (0, 130):
            return data | {"status": f"server-exit:{process.returncode}"}
        (directory / "server.stdout").write_bytes((directory / "stdout").read_bytes())
        (directory / "stdout").write_bytes(receipt)
    elif process.returncode:
        return data | {"status": f"server-exit:{process.returncode}"}
    elif reply != wanted:
        return data | {"status": "wrong-load-output"}
    metrics = {name: int(value) for name, value in
               re.findall(r"^([a-z][a-z0-9-]*)=(\d+)$", (directory / "load.stderr").read_text(), re.M)}
    if profile:
        return data | {"status": "ok", "metrics": metrics}
    counters = validate_sample(directory, case, wanted, cpus_text)
    service_seconds = data["client_seconds"] if checkbox else metrics["socket-ns"] / 1e9
    return counters | data | {"metrics": metrics, "service_seconds": service_seconds}


def replay_sample(bins, path, row, tape, cpu, wanted):
    if row["status"] == "ok":
        command = regimes.command(bins, row["case"], "Hirsel", 1, row["size"],
                                  row["rounds"], "replay", tape)
        with ((path / "replay.stdout").open("wb") as output,
              (path / "replay.stderr").open("wb") as err):
            result = subprocess.run(
                ["taskset", "-c", str(cpu), ROOT / "tools/capped", *map(str, command)],
                cwd=ROOT, stdout=output, stderr=err, env=sample_env(1, "Hirsel", True))
        if result.returncode or (path / "replay.stdout").read_bytes() != wanted:
            row["status"] = f"replay-failed:{result.returncode}"
    if (bins / tape).exists():
        shutil.move(bins / tape, path / "run.tape")


@contextmanager
def measurement_lock(path):
    with path.open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        yield


def host_snapshot(process):
    result = subprocess.run(["pgrep", "-x", "--", re.escape(process)], capture_output=True, text=True)
    if result.returncode not in (0, 1):
        raise RuntimeError(f"pgrep exit {result.returncode}: {result.stderr.strip()}")
    return {
        "time": datetime.now().astimezone().isoformat(timespec="seconds"),
        "load_average": os.getloadavg(),
        "pids": [int(pid) for pid in result.stdout.split()],
    }


def run_regime_block(args, parser, bins, out, workers):
    # Keep the process checks and every pair inside the same lane lock.
    with (measurement_lock(args.lock),
          measurement_lock(args.also_lock) if args.also_lock else nullcontext()):
        before = host_snapshot(args.block_process_check)
        after = None
        result = None
        try:
            if not before["pids"]:
                result = run_regimes(args, parser, bins, out, workers)
        finally:
            if not before["pids"]:
                after = host_snapshot(args.block_process_check)
            clean = after is not None and not after["pids"]
            block = {"process": args.block_process_check, "before": before, "after": after,
                     "clean": clean, "sample_exit": result}
            (out / "block.json").write_text(json.dumps(block, indent=2) + "\n")
        if not clean:
            print(f"block overlaps {args.block_process_check}: {out / 'block.json'}", file=sys.stderr)
            return 75
        return result


def run_regimes(args, parser, bins, out, workers):
    cases = tuple(dict.fromkeys(args.case or (case for case in regimes.WORKLOADS if case != "calibrate")))
    if not set(cases).issubset(regimes.WORKLOADS):
        parser.error("requested case is outside the grain sweep")
    cases = [case for case in regimes.WORKLOADS if case in cases]
    if args.rounds is None:
        parser.error("--rounds is required with --regimes (use 0 or trial rounds for calibration)")
    if any(case == "checkbox" or regimes.WORKLOADS[case].connections for case in cases):
        if not args.load_cpus:
            parser.error("socket and checkbox cases require --load-cpus")
        if not args.allow_smt:
            if set(args.load_cpus).intersection(args.measure_cpus):
                parser.error("socket and checkbox cases require disjoint --load-cpus")
            if set(args.load_cpus).intersection(sibling_cpus(args.measure_cpus)):
                parser.error("load CPUs must also be on separate physical cores")
    if args.mode == "recorded":
        if args.case and any(not regimes.WORKLOADS[case].recorded for case in cases):
            parser.error("a selected case has no recording and replay path")
        if args.profile:
            parser.error("profiles use the untaped fixtures")
        cases = [case for case in cases if regimes.WORKLOADS[case].recorded]
        if not cases:
            parser.error("no selected case supports recording and replay")
    rows = []
    for case in cases:
        work = regimes.WORKLOADS[case]
        size = args.size or work.size
        with_grains = [(0, 0)] if case == "checkbox" else list(zip(args.grain_ns, args.rounds))
        for grain, rounds in with_grains:
            receipt = regimes.receipt(case, size, rounds)
            wanted = ("%d %d\n" % receipt).encode()
            for width in sorted(set(workers)):
                repeats = args.repeats if width in args.repeat_workers else 1
                for repeat in range(1, repeats + 1):
                    point = out / f"{case}-{grain}ns-{rounds}r-w{width}-r{repeat}"
                    point.mkdir()
                    with nullcontext() if args.block_process_check else measurement_lock(args.lock):
                        group = []
                        for lang in ("Hirsel", "Tokio", "Go"):
                            if (args.languages and lang not in args.languages
                                    or lang == "Go" and (not work.go or args.mode == "recorded")):
                                continue
                            cpus = args.measure_cpus[:width]
                            path = point / f"{case}-{lang.lower()}-{width}"
                            recorded = args.mode == "recorded" and lang == "Hirsel"
                            tape = f"{case}-{grain}ns-{rounds}r-w{width}-r{repeat}.tape"
                            # The native Dir grant points to the private build directory.
                            tape = f"{out.name}-{tape}"
                            if work.connections or case == "checkbox":
                                row = observe_service(bins, path, case, lang, cpus, args.load_cpus,
                                                      size, rounds, wanted, profile=args.profile,
                                                      allow_smt=args.allow_smt, cycles=args.cycles)
                            else:
                                extra = ("record", tape) if recorded else ()
                                command = regimes.command(bins, case, lang, width, size, rounds, *extra)
                                row = observe(bins, point, case, lang, cpus, size, wanted, False,
                                              None, test_pool=True, command=command, profile=args.profile,
                                              allow_smt=args.allow_smt, cycles=args.cycles)
                                if path.exists():
                                    (path / "command").write_text(shlex.join(map(str, command)) + "\n")
                                    row["metrics"] = stderr_metrics(path)
                            row |= {"case": case, "language": lang, "width": width, "size": size,
                                    "rounds": rounds, "grain_ns": grain, "repeat": repeat,
                                    "mode": args.mode, "path": str(path.relative_to(out))}
                            if args.allow_smt and args.load_cpus and set(cpus).intersection(args.load_cpus):
                                row["placement"] = "SMT-bound, load shares the server CPUs"
                            if lang == "Hirsel" and path.exists():
                                row["pool"] = pool_counters(path, case, size)
                            if recorded:
                                chooser = 0 if width == 1 else 2
                                row["recorded_with"] = f"testing archive, tape.open/tape.tasks, chooser {chooser}"
                            group.append((path, row, tape if recorded else None))
                        for path, row, tape in group:
                            if tape:
                                replay_sample(bins, path, row, tape, args.measure_cpus[0], wanted)
                            lang = row["language"]
                            rows.append(row)
                            print(f"{case} {grain}ns {lang} {width} repeat {repeat}: {row['status']}",
                                  file=sys.stderr, flush=True)
                            (out / "samples.json").write_text(json.dumps(rows, indent=2) + "\n")
    return 0 if rows and all(row["status"] == "ok" for row in rows) else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bins", type=Path, help="binaries and FASTA from build.py")
    parser.add_argument("--out", type=Path, help="new evidence directory outside the repository")
    parser.add_argument("--case", nargs="+", choices=SIZES | S0_SIZES | S8_SIZES | regimes.WORKLOADS)
    parser.add_argument("--workers", nargs="+", type=int, choices=WIDTHS)
    selection = parser.add_mutually_exclusive_group()
    selection.add_argument("--languages", nargs="+", choices=LANGUAGES)
    selection.add_argument("--standard-rust", action="store_true", help="measure published Rust data inputs")
    parser.add_argument("--s0", action="store_true", help="validate both fields and measure only the S0 fixtures")
    parser.add_argument("--test-pool", action="store_true", help="select workers and collect counters from an S8 test-pool archive")
    parser.add_argument("--read", nargs="+", type=Path, help="render existing sample JSON files")
    parser.add_argument("--regimes", action="store_true", help="run the scheduler grain sweep")
    parser.add_argument("--grain-ns", nargs="+", type=int, default=regimes.GRAINS)
    parser.add_argument("--rounds", nargs="+", type=int, help="kernel iteration counts in grain order")
    parser.add_argument("--size", type=int, help="override the scheduler workload size")
    parser.add_argument("--mode", choices=("untaped", "recorded"), default="untaped")
    parser.add_argument("--load-cpus", type=cpu_list)
    parser.add_argument("--allow-smt", action="store_true",
                        help="allow benchmark and load CPU assignments to share cores or CPUs")
    parser.add_argument("--cycles", action="store_true", help="also count user cycles in grain samples")
    parser.add_argument("--repeats", type=int, default=3)
    parser.add_argument("--repeat-workers", nargs="+", type=int, default=(1, 4, 8))
    parser.add_argument("--crossover-width", type=int, default=8, help="width to compare to 1 when rendering grain results")
    parser.add_argument("--profile", action="store_true", help="collect perf stacks instead of stat counters")
    add_cpu_sets(parser, required=False)
    parser.add_argument("--lock", type=Path, default=LOCK)
    parser.add_argument("--also-lock", type=Path, help="acquire a second lock for the whole grain block")
    parser.add_argument("--block-process-check", metavar="NAME",
                        help="hold the lock for a grain block and reject it if NAME appears at either boundary")
    args = parser.parse_args()
    if args.block_process_check and (not args.regimes or args.read):
        parser.error("--block-process-check requires a grain measurement")
    if args.also_lock and (not args.block_process_check or args.also_lock.resolve() == args.lock.resolve()):
        parser.error("--also-lock requires a guarded block and a distinct lock path")
    if args.read:
        rows = [row for file in args.read for row in json.loads(file.read_text())]
        if args.regimes:
            regimes.render(rows, args.crossover_width)
            return 0 if rows and all(row["status"] == "ok" for row in rows) else 1
    else:
        if args.bins is None or args.out is None:
            parser.error("--bins and --out are required to measure")
        bins, out = args.bins.resolve(), args.out.resolve()
        if out.is_relative_to(ROOT) or out.exists():
            parser.error("--out must be a new directory outside the repository")
        workers = args.workers or ([1] if args.s0 else (1, 2, 4, 8, 16) if args.regimes else WIDTHS)
        if args.regimes:
            if args.s0 or args.standard_rust or set(workers) - {1, 2, 4, 8, 16}:
                parser.error("use the grain sweep by itself at widths 1, 2, 4, 8 or 16")
            if args.languages and set(args.languages) - {"Hirsel", "Tokio", "Go"}:
                parser.error("the grain sweep supports Hirsel, Tokio and Go")
            if args.rounds and (len(args.rounds) != len(args.grain_ns) or min(args.rounds) < 0):
                parser.error("supply one nonnegative --rounds value for each --grain-ns value")
            if (args.size is not None and args.size < 1) or args.repeats < 1:
                parser.error("--size and --repeats must be positive")
            validate_cpu_sets(parser, args, width=max(workers), sequential=True, allow_smt=args.allow_smt)
            if args.load_cpus and not set(args.load_cpus).issubset(os.sched_getaffinity(0)):
                parser.error("load CPUs outside the caller's affinity")
            os.sched_setaffinity(0, args.build_cpus)
            out.mkdir(parents=True)
            if args.block_process_check:
                return run_regime_block(args, parser, bins, out, workers)
            return run_regimes(args, parser, bins, out, workers)
        if args.s0 and args.standard_rust:
            parser.error("--s0 and --standard-rust select different workloads")
        validate_cpu_sets(parser, args, width=max(workers))
        os.sched_setaffinity(0, args.build_cpus)
        sizes = S0_SIZES if args.s0 else STANDARD_RUST_SIZES if args.standard_rust else SIZES
        languages = ("Rust",) if args.standard_rust else args.languages or LANGUAGES
        if args.test_pool and not (args.s0 or args.standard_rust):
            sizes = sizes | S8_SIZES
        cases = tuple(dict.fromkeys(args.case or sizes))
        if any(case not in sizes for case in cases):
            parser.error("the requested workload is outside the selected suite")
        oracles = {case: S0_WORK[case] if args.s0 else expected(case, bins / "input.fasta", args.standard_rust)
                   for case in cases}
        out.mkdir(parents=True)
        rows = []
        for case in cases:
            kernel = args.standard_rust or case == "mandelbrot"
            size = sizes[case]
            for width in sorted(set(workers)):
                for language in applicable(case, args.s0):
                    if language not in languages:
                        continue
                    try:
                        result = observe(bins, out, case, language, args.measure_cpus[:width], size,
                                         oracles[case], kernel, args.lock, args.s0, args.test_pool)
                    except ValueError as error:
                        parser.error(str(error))
                    rows.append(result | {"case": case, "language": language, "width": width, "size": size})
                    print(f"{case} {language} {width}: {result['status']}", file=sys.stderr, flush=True)
                    (out / "samples.json").write_text(json.dumps(rows, indent=2) + "\n")
    keys = [(row["case"], row["language"], row["width"]) for row in rows]
    if len(keys) != len(set(keys)):
        parser.error("more than one sample for the same point")
    for case in {row["case"] for row in rows}:
        if len({row.get("size") for row in rows if row["case"] == case}) > 1:
            parser.error(f"mixed input sizes for {case}")
    table(rows)
    return 0 if rows and all(s["status"] == "ok" for s in rows) else 1


if __name__ == "__main__":
    sys.exit(main())
