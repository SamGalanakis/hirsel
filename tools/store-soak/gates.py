#!/usr/bin/env python3
"""Check N4 receipts and freeze the matched 24-hour control envelopes."""
import argparse
import ctypes
from itertools import combinations
import json
import math
from pathlib import Path
from statistics import median
import subprocess
import sys
import time

from observe import MIB, Receipts, atomic_json, digest, memory, receipts

HOUR = 3600
LEAK_CEILING = MIB  # bytes per hour, screened by the 2 MiB/hour positive control
CEILINGS = {
    "private_pss_bytes": 2 * 1024 * MIB,
    "private_rss_bytes": 2 * 1024 * MIB,
    "vmas": 4096,
    "space_amplification": 128,
    "p99_ms": 1000,
    "resident_bytes": 768 * MIB,
}
RESOLUTION = {
    "private_pss_bytes": 4096,
    "private_rss_bytes": 4096,
    "vmas": 1,
    "space_amplification": 0.05,
    "p99_ms": 1,
    "resident_bytes": 4096,
}
MEMORY = ("private_pss_bytes", "private_rss_bytes")
ZERO = ("open_snapshots", "pins", "leases", "retired_packs", "m_files", "pending_multipart", "debris")


class GateFailure(Exception):
    pass


def differences(points):
    return [(right - left) * HOUR / (t_right - t_left)
            for (t_left, left), (t_right, right) in combinations(points, 2)]


def mad(values):
    center = median(values)
    return median(abs(value - center) for value in values)


def leak_screen(points):
    slopes = {metric: median(differences([(t, row[metric]) for t, row in points]))
              for metric in MEMORY}
    if any(slope > LEAK_CEILING for slope in slopes.values()):
        detail = ", ".join(f"{name}={slope / MIB:.3f} MiB/h" for name, slope in slopes.items())
        raise GateFailure(f"N4 private-memory slope exceeded 1.000 MiB/h: {detail}")
    return slopes


def exact(places, baseline=None):
    if any(places["A"][key] == places["B"][key] for key in ("pid", "cgroup", "device", "store_id")):
        raise GateFailure("N4 Places share a process, cgroup, volume or Store ID")
    for name, place in places.items():
        wrong = {key: place[key] for key in ZERO if place[key] != 0}
        if wrong or place["closed_fds"] or place["cgroup_events"]["oom_kill"]:
            raise GateFailure(f"N4 Place {name} quiescent resources: {wrong}, "
                              f"debris paths {place['debris_paths']}, closed FDs {place['closed_fds']}, "
                              f"cgroup {place['cgroup_events']}")
        if baseline is not None and place["fds"] != baseline[name]:
            raise GateFailure(f"N4 Place {name} descriptors changed: {baseline[name]} -> {place['fds']}")
        for key, ceiling in CEILINGS.items():
            if not math.isfinite(place[key]) or not 0 <= place[key] <= ceiling:
                raise GateFailure(f"N4 Place {name} {key} exceeds {ceiling}: {place[key]}")


def check_rows(rows, complete=True):
    start = rows[0]
    if start["kind"] != "start":
        raise GateFailure("N4 missing start receipt")
    if complete and rows[-1]["kind"] != "complete":
        raise GateFailure(f"N4 run has no completion receipt, last receipt is {rows[-1]['kind']}")
    if any(row["kind"] == "failed" for row in rows):
        raise GateFailure("N4 run contains a failure receipt")
    samples = [row for row in rows if row["kind"] == "sample"]
    if not samples:
        raise GateFailure("N4 run contains no samples")
    first = samples[0]["places"]
    baseline = {name: first[name]["fds"] for name in first}
    last = -1
    pid_a = (first["A"]["pid"], first["A"]["start_tick"])
    for sample in samples:
        places = sample["places"]
        if (places["A"]["pid"], places["A"]["start_tick"]) != pid_a:
            raise GateFailure("N4 Place A restarted")
        if sample["elapsed_s"] <= last:
            raise GateFailure("N4 sample times are out of order")
        last = sample["elapsed_s"]
        exact(places, baseline)
    if complete:
        profile = start["profile"]
        for field, expected in profile["required_events"].items():
            actual = sum(event["event"] == field for row in samples for event in row["events"])
            if actual != expected:
                raise GateFailure(f"N4 expected {expected} completed {field} events, observed {actual}")
    return start, baseline, samples


def elapsed_points(samples):
    return [(row["elapsed_s"], row["places"]["A"]) for row in samples if row["hour"] > 2]


def hourly_points(samples, expected_hours):
    hourly = [sample["hour"] for sample in samples if sample["hour"]]
    if hourly != list(range(1, expected_hours + 1)):
        raise GateFailure(f"N4 hourly samples incomplete or duplicated: {hourly}")
    return elapsed_points(samples)


def freeze(start, baseline, samples):
    if start["profile"]["mode"] != "control":
        raise GateFailure("N4 calibration requires the complete 24-hour control")
    points = hourly_points(samples, 24)
    leak_screen(points)
    limits = {}
    for metric, ceiling in CEILINGS.items():
        matched = [(t, row[metric]) for t, row in points]
        values = [val for _, val in matched]
        slopes = differences(matched)
        limits[metric] = {"median": median(values), "mad": mad(values),
                          "upper": min(ceiling, max(values) + max(3 * mad(values), RESOLUTION[metric])),
                          "slope": median(slopes), "slope_mad": mad(slopes)}
    resident = limits["resident_bytes"]
    resident["class"] = "exact" if all(
        row["resident_bytes"] == resident["median"] for _, row in points) else "zero-slope"
    if resident["class"] == "zero-slope":
        noise = max(3 * resident["slope_mad"], RESOLUTION["resident_bytes"] / 21)
        if abs(resident["slope"]) > noise:
            raise GateFailure(f"N4 Store resident control drifts: {resident['slope']:.1f} B/h > {noise:.1f} B/h")
        resident["max_abs_slope"] = noise
    for metric in MEMORY:
        lim = limits[metric]
        lim["max_slope"] = min(LEAK_CEILING, max(0, lim["slope"]) + max(
            3 * lim["slope_mad"], RESOLUTION[metric] / 21))
    return {"v": 1, "identity": start["identity"], "fds": baseline, "metrics": limits}


def measured(start, samples, frozen, complete=False):
    if frozen["v"] != 1 or frozen["identity"] != start["identity"]:
        raise GateFailure("N4 measurement profile differs from the frozen control")
    for sample in samples:
        exact(sample["places"], frozen["fds"])
    points = hourly_points(samples, 72) if complete else elapsed_points(samples)
    for metric, limits in frozen["metrics"].items():
        values = [row[metric] for _, row in points]
        if not values:
            continue
        if max(values) > limits["upper"]:
            raise GateFailure(f"N4 {metric} plateau {max(values)} exceeds {limits['upper']}")
        if limits.get("class") == "exact" and any(v != limits["median"] for v in values):
            raise GateFailure(f"N4 Store resident exact equality failed: {values}")
        if len(points) >= 3 and metric in (*MEMORY, "resident_bytes"):
            slope = median(differences([(t, row[metric]) for t, row in points]))
            if metric in MEMORY and slope > limits["max_slope"]:
                raise GateFailure(f"N4 {metric} slope {slope:.1f} B/h exceeds {limits['max_slope']:.1f} B/h")
            if limits.get("class") == "zero-slope" and abs(slope) > limits["max_abs_slope"]:
                raise GateFailure(f"N4 Store resident slope {slope:.1f} B/h exceeds {limits['max_abs_slope']:.1f} B/h")


def allocate():
    """Keep each separately committed glibc allocation alive until process exit."""
    libc = ctypes.CDLL("libc.so.6")
    libc.malloc.argtypes = [ctypes.c_size_t]
    libc.malloc.restype = ctypes.c_void_p
    print("READY", flush=True)
    for request in sys.stdin:
        if request != "leak\n":
            raise ValueError("unexpected allocation request")
        allocation = libc.malloc(2 * MIB)
        if not allocation:
            raise MemoryError("malloc")
        ctypes.memset(allocation, 0xA5, 2 * MIB)
        print("ALLOCATED 2097152", flush=True)


def positive(path):
    book = Receipts(path)
    process = subprocess.Popen([sys.executable, __file__, "_allocator"], stdin=subprocess.PIPE,
                               stdout=subprocess.PIPE, text=True)
    points = []
    with process:
        try:
            if process.stdout.readline() != "READY\n":
                raise RuntimeError("leak process failed to start")
            started = time.monotonic()
            for hour in range(6):
                if hour:
                    time.sleep(max(0, started + hour - time.monotonic()))
                    process.stdin.write("leak\n")
                    process.stdin.flush()
                    if process.stdout.readline() != "ALLOCATED 2097152\n":
                        raise RuntimeError("seeded allocation failed")
                sample = memory(process.pid)
                if points and any(sample[m] <= points[-1][1][m] for m in MEMORY):
                    raise RuntimeError(f"seeded {MEMORY} samples are not monotonic")
                points.append((hour * HOUR, sample))
                book.emit("positive_sample", pid=process.pid, equivalent_elapsed_s=hour * HOUR,
                          allocated_bytes=hour * 2 * MIB, **sample)
        finally:
            process.stdin.close()
    if process.returncode:
        raise RuntimeError(f"allocator exited {process.returncode}")
    try:
        leak_screen(points)
    except GateFailure as error:
        book.emit("positive_rejected", diagnostic=str(error))
        raise
    raise RuntimeError("seeded leak passed the N4 evaluator")


def check_positive(path):
    rows = receipts(path)
    if (len(rows) != 7 or rows[-1]["kind"] != "positive_rejected"
            or any(row["kind"] != "positive_sample" or row["allocated_bytes"] != hour * 2 * MIB
                   or row["equivalent_elapsed_s"] != hour * HOUR for hour, row in enumerate(rows[:-1]))):
        raise GateFailure("N4 positive control proof is incomplete")
    try:
        leak_screen([(row["equivalent_elapsed_s"], row) for row in rows[:-1]])
    except GateFailure as error:
        if str(error) == rows[-1]["diagnostic"]:
            return digest(rows[-1])
        raise
    raise GateFailure("N4 positive control slopes passed")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("positive", "check", "freeze", "evaluate"))
    parser.add_argument("receipts", type=Path)
    parser.add_argument("envelopes", type=Path, nargs="?")
    parser.add_argument("--live", action="store_true")
    args = parser.parse_args()
    if args.command == "positive":
        positive(args.receipts)
    else:
        start, baseline, samples = check_rows(receipts(args.receipts), not args.live)
        if args.command == "freeze":
            if args.live or args.envelopes is None or args.envelopes.exists():
                parser.error("freeze requires a complete control and a fresh envelope path")
            atomic_json(args.envelopes, freeze(start, baseline, samples))
        if args.command == "evaluate":
            if args.envelopes is None:
                parser.error("evaluate requires frozen envelopes")
            measured(start, samples, json.loads(args.envelopes.read_text()), complete=not args.live)
        print(f"N4 {args.command}: {len(samples)} healthy samples")


if __name__ == "__main__":
    if sys.argv[1:] == ["_allocator"]:
        allocate()
    else:
        try:
            main()
        except (GateFailure, ValueError) as problem:
            print(problem, file=sys.stderr)
            sys.exit(1)
