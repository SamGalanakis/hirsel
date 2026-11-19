#!/usr/bin/env python3
"""Profile the five largest Hirsel instruction ratios to either peer."""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess

from measure import ROOT, measurement_slot, rows
from bench_cpus import add_cpu_sets, sibling_cpus, smt_activity, validate_cpu_sets


def ratios(samples):
    if any(s["status"] != "ok" or not s.get("instructions") for s in samples.values()):
        return 0
    return samples["hirsel"]["instructions"] / min(samples[side]["instructions"] for side in ("rust", "goose"))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("build", type=Path)
    parser.add_argument("samples", type=Path)
    parser.add_argument("out", type=Path)
    parser.add_argument("--upstream", type=Path, required=True)
    add_cpu_sets(parser)
    args = parser.parse_args()
    validate_cpu_sets(parser, args)
    siblings = sibling_cpus(args.measure_cpus[:1])
    os.sched_setaffinity(0, args.build_cpus)
    programs = dict(rows(args.build.resolve(), False, args.upstream))
    samples = json.loads(args.samples.read_text())
    ranked = sorted(((ratios(result), name) for name, result in samples.items()), reverse=True)
    os.environ.update(HIRSEL_MEMORY_LIMIT="2G", HIRSEL_JOBS="1")
    args.out.mkdir(parents=True)
    failures = 0
    for ratio, name in ranked[:5]:
        if ratio <= 0:
            break
        print(name, flush=True)
        binary, marker = programs[name]["hirsel"]
        data = args.out / f"{name}.data"
        with measurement_slot(), smt_activity(siblings, name) as busy:
            run = subprocess.run([
                "taskset", "-c", str(args.measure_cpus[0]),
                ROOT / "tools/capped", "perf", "record", "-e", "instructions:u",
                "-c", "200003", "--no-buildid-cache", "--call-graph", "dwarf,4096",
                "-o", data, "--", *binary,
            ], capture_output=True)
        report = args.out / f"{name}.txt"
        report.write_bytes(run.stderr + f"SMT busy percent: {busy}\n".encode())
        if run.returncode != 0 or not re.fullmatch(marker, run.stdout):
            failures += 1
            report.write_bytes(report.read_bytes() + run.stdout)
        else:
            with report.open("ab") as stream:
                summary = subprocess.run(["perf", "report", "--stdio", "--no-children",
                                          "--percent-limit", "1", "-i", data],
                                         stdout=stream, stderr=stream)
            failures += summary.returncode != 0
        data.unlink(missing_ok=True)
    return int(failures != 0)


if __name__ == "__main__":
    raise SystemExit(main())
