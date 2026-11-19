#!/usr/bin/env python3
"""Measure one process-tree sample per workload, language, and worker count."""
import argparse
from collections import Counter
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import statistics
import struct
import subprocess
import sys

from build import GAME_ROWS, ROOT, STANDARD_RUST_SIZES

sys.path.insert(0, str(ROOT / "core/tools"))
from bench_cpus import add_cpu_sets, require_idle, sibling_cpus, validate_cpu_sets
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
            # Both upstream ports format float32. Hirsel and Goose format float64.
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


def observe(bins, out, case, language, cpus, size, wanted, kernel, lock_path, s0=False, test_pool=False):
    width = len(cpus)
    siblings = sibling_cpus(cpus)
    command = sample_command(bins, case, language, width, size, s0)
    if not command[0].is_file():
        return {"status": "missing-binary"}
    directory = out / f"{case}-{language.lower()}-{width}"
    directory.mkdir()
    perf = directory / "perf.csv"
    rss = directory / "rss.txt"
    cpus = ",".join(map(str, cpus))
    env = os.environ | {
        "LC_ALL": "C",
        "HIRSEL_MEMORY_LIMIT": "2G",
        "HIRSEL_JOBS": str(width),
        "GOMAXPROCS": str(width),
        "RAYON_NUM_THREADS": str(width),
    }
    if test_pool:
        env["HIRSEL_S8_STATS"] = "1"
        if language == "Hirsel":
            env["HIRSEL_S8_WORKERS"] = str(width)
    launch = [ROOT / "tools/capped", "perf", "stat", "-x", ";", "-e",
              "instructions:u,duration_time", "-o", perf, "--", "/usr/bin/time",
              "-f", "%M", "-o", rss, *command]
    stdin = bins / "input.fasta" if case == "knucleotide" and language != "Hirsel" else Path(os.devnull)
    wait_load()
    with lock_path.open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        require_idle(siblings)
        with stdin.open("rb") as inp, (directory / "stdout").open("wb") as out_file, (directory / "stderr").open("wb") as err:
            result = subprocess.run(["taskset", "-c", cpus, *map(str, launch)],
                                    cwd=ROOT, env=env, stdin=inp, stdout=out_file, stderr=err)
    row = {"status": "ok", "cpus": cpus}
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
    return validate_sample(directory, case, wanted, cpus, kernel)


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
    if not rss.is_file() or not rss.read_text().strip().isdigit():
        row["status"] = "missing-rss"
        return row
    if kernel:
        durations = re.findall(r"^multicore-kernel-ns=(\d+)$", (directory / "stderr").read_text(), re.M)
        if len(durations) != 1 or not 0 < int(durations[0]) <= counters["duration_time"]:
            row["status"] = "missing-or-invalid-kernel-time"
            return row
        row["kernel_seconds"] = int(durations[0]) / 1e9
    return row | {
        "instructions": counters["instructions:u"],
        "seconds": counters["duration_time"] / 1e9,
        "max_rss_kib": int(rss.read_text()),
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


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bins", type=Path, help="binaries and FASTA from build.py")
    parser.add_argument("--out", type=Path, help="new evidence directory outside the repository")
    parser.add_argument("--case", nargs="+", choices=SIZES | S0_SIZES | S8_SIZES)
    parser.add_argument("--workers", nargs="+", type=int, choices=WIDTHS)
    selection = parser.add_mutually_exclusive_group()
    selection.add_argument("--languages", nargs="+", choices=LANGUAGES)
    selection.add_argument("--standard-rust", action="store_true", help="measure published Rust data inputs")
    parser.add_argument("--s0", action="store_true", help="validate both fields and measure only the S0 fixtures")
    parser.add_argument("--test-pool", action="store_true", help="select workers and collect counters from an S8 test-pool archive")
    parser.add_argument("--read", nargs="+", type=Path, help="render existing sample JSON files")
    add_cpu_sets(parser, required=False)
    parser.add_argument("--lock", type=Path, default=LOCK)
    args = parser.parse_args()
    if args.read:
        rows = [row for file in args.read for row in json.loads(file.read_text())]
    else:
        if args.bins is None or args.out is None:
            parser.error("--bins and --out are required to measure")
        bins, out = args.bins.resolve(), args.out.resolve()
        if out.is_relative_to(ROOT) or out.exists():
            parser.error("--out must be a new directory outside the repository")
        workers = args.workers or ([1] if args.s0 else WIDTHS)
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
