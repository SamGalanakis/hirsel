"""Collect B1 counter and frame-pointer samples, with a Callgrind phase fallback."""

import argparse
import fcntl
import os
from pathlib import Path
import runpy
import tempfile

import attribution
import b1
from build import MICRO, ROOT
import callgrind_phase

NAMES = ("tokio", "hyper", "hirsel")


def run(binaries, source, server_cpu, client_cpu, output):
    os.sched_setaffinity(0, {client_cpu})
    results = {}
    skid = {}
    cycles = {}
    with tempfile.TemporaryDirectory(prefix="b1-", dir=output) as scratch:
        scratch = Path(scratch)
        for name in NAMES:
            command = [str(binaries[name])]
            cost, cycles[name] = b1.measure(name, command, server_cpu, 32768, scratch)
            profile = b1.profile(name, command, server_cpu, 131072, scratch)
            buckets, _, skid[name] = attribution.phases(
                profile, binaries[name], source if name == "hirsel" else None)
            results[name] = cost, buckets
        hirsel, tokio, hyper = (results[name][0] for name in ("hirsel", "tokio", "hyper"))
        print(f"B1 against Hyper: **{hirsel / hyper:.2f}× user instructions**, "
              f"**{cycles['hirsel'] / cycles['hyper']:.2f}× cycles**.")
        print("Hirsel also records replay, charges budgets, validates Host and rejects "
              "duplicate Content-Length.")
        print()
        print("Tokio+httparse floor (minimal server: no protocol validation, static response): "
              f"{hirsel / tokio:.2f}× user instructions, "
              f"{cycles['hirsel'] / cycles['tokio']:.2f}× cycles.")
        print("Hirsel also records replay, charges budgets, validates Host and rejects "
              "duplicate Content-Length.")
        print()
        print("Kernel-leaf sample shares: " + ", ".join(f"{n} {skid[n]:.1%}" for n in NAMES) + ".")
        print()
        if any(value > 0.10 for value in skid.values()):
            print("Phase ratios use startup-subtracted Callgrind self instructions "
                  "because instruction sampling landed in kernel frames.")
            print()
            exchange = runpy.run_path(str(MICRO / "instructions"))["exchange"]
            for name in NAMES:
                for count in (1024, 8192):
                    path = scratch / f"{name}.{count}.callgrind"
                    with open(b1.LOCK, "a") as lock:
                        fcntl.flock(lock, fcntl.LOCK_EX)
                        exchange(["taskset", "-c", str(server_cpu), "valgrind",
                                  "--tool=callgrind", "--quiet", "--dump-instr=yes",
                                  "--compress-strings=no", "--compress-pos=no",
                                  f"--callgrind-out-file={path}", str(binaries[name])],
                                 count, has_date=name != "tokio")
            results = callgrind_phase.estimate(scratch, binaries, source, ROOT / "core/std/http1.hirsel")
        attribution.phase_counts(results)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in NAMES:
        parser.add_argument(f"--{name}", type=Path, required=True)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--server-cpu", type=int, required=True)
    parser.add_argument("--client-cpu", type=int, required=True)
    args = parser.parse_args()
    run({name: getattr(args, name) for name in NAMES}, args.source,
        args.server_cpu, args.client_cpu, args.output)


if __name__ == "__main__":
    main()
