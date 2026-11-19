#!/usr/bin/env python3
"""Sample the S8u fixtures, then check each result and its work receipt."""
import argparse
import fcntl
import os
from pathlib import Path
import subprocess

from run import LOCK, ROOT, wait_load
from bench_cpus import add_cpu_sets, require_idle, sibling_cpus, validate_cpu_sets

MASK = (1 << 64) - 1
CASES = ("knapsack", "portfolio")
SIDES = ("hirsel", "rust", "region")


def knapsack_oracle():
    prefixes = [0] + [
        100 * n + 17 * n * (n - 1) // 2 + (((n + 1199) * 11400714819323198485 & MASK) >> 61)
        for n in range(1, 61)
    ]
    weights = [b - a for a, b in zip(prefixes, prefixes[1:])]
    capacity = sum(weights) // 3 + 7
    best = [0] * (capacity + 1)
    for weight in weights:
        for limit in range(capacity, weight - 1, -1):
            best[limit] = max(best[limit], best[limit - weight] + weight + 300)

    def check(bits):
        chosen = [weight for i, weight in enumerate(weights) if bits >> i & 1]
        return bits >> 60 == 0 and sum(chosen) <= capacity and sum(w + 300 for w in chosen) == best[-1]

    return best[-1], check


def portfolio_oracle():
    # xorshift64(0x94d049bb133111eb) and one further advance.
    unary = 0x57c9f356597e6908
    edges = 0x9562b0e6c8c3d7da
    costs = [unary & 1, 1 - (unary & 1)]
    for i in range(1, 64):
        want = unary >> i & 1
        edge = edges >> (i - 1) & 1
        costs = [
            (bit != want) + min(costs[prev] + 2 * ((bit ^ prev) != edge) for prev in (0, 1))
            for bit in (0, 1)
        ]
    target = min(costs)

    def check(bits):
        sites = (bits ^ unary).bit_count()
        breaks = ((bits ^ (bits >> 1) ^ edges) & (MASK >> 1)).bit_count()
        return bits <= MASK and sites + 2 * breaks == target

    return target, check


def sample(case, side, cpus, bins, out, lock_path):
    width = len(cpus)
    siblings = sibling_cpus(cpus)
    directory = out / f"{case}-{side}-{width}"
    directory.mkdir()
    if side == "hirsel":
        program = [bins / "hirsel" / f"s8u_{case}"]
    else:
        program = [bins / "rust" / "s8u", case, side, str(width)]
    env = os.environ | {
        "LC_ALL": "C",
        "HIRSEL_MEMORY_LIMIT": "2G",
        "HIRSEL_JOBS": str(width),
        "HIRSEL_S8_WORKERS": str(width),
        "HIRSEL_S8_STATS": "0",
    }
    cpus = ",".join(map(str, cpus))
    command = [
        "taskset", "-c", cpus, ROOT / "tools/capped",
        "perf", "stat", "-x", ";",
        "-e", "instructions:u,duration_time", "-o", directory / "perf.csv", "--", *program,
    ]
    wait_load()
    with lock_path.open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        require_idle(siblings)
        with (directory / "stdout").open("wb") as stdout, (directory / "stderr").open("wb") as stderr:
            subprocess.run(command, cwd=ROOT, env=env, stdout=stdout, stderr=stderr, check=True)



def read_samples(out):
    results = {}
    for case, oracle in zip(CASES, (knapsack_oracle, portfolio_oracle)):
        target, check = oracle()
        for side in SIDES:
            for width in (1, 16):
                directory = out / f"{case}-{side}-{width}"
                elapsed, work, value, witness, lower, upper, rounds = map(
                    int, (directory / "stdout").read_text().split()
                )
                if not (elapsed > 0 and work > 0 and 0 <= lower <= upper <= elapsed):
                    raise ValueError(f"{directory}: invalid timing or work receipt")
                if value != target or not check(witness):
                    raise ValueError(f"{directory}: invalid solution; oracle is {target}")
                if (rounds > 0) != (side != "rust"):
                    raise ValueError(f"{directory}: invalid round count")
                results[case, side, width] = (elapsed, work, value, lower, upper)
        t1 = results[case, "hirsel", 1]
        t16 = results[case, "hirsel", 16]
        if t1[1] != t16[1]:
            raise ValueError(f"{case}: deterministic Hirsel work differs across widths")
    return results


def summarize(results):
    verdict = False
    print("case side T1/T16 speedup/twin work(T1/T16) work/twin(T1/T16) lag-ns(T1/T16) quality")
    for case in CASES:
        rust1 = results[case, "rust", 1]
        rust16 = results[case, "rust", 16]
        rust_speedup = rust1[0] / rust16[0]
        ratios = {}
        for side in SIDES:
            one, many = (results[case, side, width] for width in (1, 16))
            speedup = one[0] / many[0]
            ratio = speedup / rust_speedup
            ratios[side] = ratio
            print(
                f"{case} {side} {speedup:.3f} {ratio:.3f} {one[1]}/{many[1]} "
                f"{one[1]/rust1[1]:.3f}/{many[1]/rust16[1]:.3f} "
                f"[{one[3]},{one[4]}]/[{many[3]},{many[4]}] {one[2]}"
            )
        verdict |= ratios["hirsel"] < 0.60 and ratios["region"] >= 0.85
    print("R27 threshold met" if verdict else "R27 threshold not met")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bins", type=Path)
    parser.add_argument("--out", type=Path, required=True)
    add_cpu_sets(parser, required=False)
    parser.add_argument("--lock", type=Path, default=LOCK)
    args = parser.parse_args()
    out = args.out.resolve()
    if out.is_relative_to(ROOT):
        parser.error("--out must be outside the repository")
    if args.bins:
        validate_cpu_sets(parser, args, width=16)
        os.sched_setaffinity(0, args.build_cpus)
        bins = args.bins.resolve()
        for case in CASES:
            program = bins / "hirsel" / f"s8u_{case}"
            if b"HIRSEL_S8_WORKERS" not in program.read_bytes():
                parser.error(f"{program} requires the test-pool archive")
        out.mkdir(parents=True)
        for case in CASES:
            for width in (1, 16):
                for side in SIDES:
                    try:
                        sample(case, side, args.measure_cpus[:width], bins, out, args.lock)
                    except ValueError as error:
                        parser.error(str(error))
    summarize(read_samples(out))


if __name__ == "__main__":
    main()
