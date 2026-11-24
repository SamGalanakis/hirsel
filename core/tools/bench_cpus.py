"""CPU assignments and Linux SMT activity for benchmark runners."""
import argparse
from contextlib import contextmanager
import json
import os
from pathlib import Path
import re
import subprocess
import sys


def positive_int(value):
    try:
        number = int(value)
    except ValueError:
        number = 0
    if number <= 0:
        raise argparse.ArgumentTypeError("expected a positive integer")
    return number


def cpu_list(value):
    """Parse the Linux CPU list syntax (for example, 2-3,8)."""
    cpus = {}
    for part in value.split(","):
        match = re.fullmatch(r"([0-9]+)(?:-([0-9]+))?", part)
        if match is None:
            raise argparse.ArgumentTypeError("expected CPU IDs or ascending ranges separated by commas")
        start, end = int(match[1]), int(match[2] or match[1])
        if start > end:
            raise argparse.ArgumentTypeError("CPU ranges must be ascending")
        cpus.update(dict.fromkeys(range(start, end + 1)))
    return tuple(cpus)


def add_cpu_sets(parser, *, required=True):
    parser.add_argument("--build-cpus", type=cpu_list, required=required, help="CPU list for setup and builds")
    parser.add_argument("--measure-cpus", type=cpu_list, required=required, help="CPU list for measured processes")


def sibling_cpus(measure_cpus, build_cpus=(), *, allow_smt=False):
    measure_cpus, build_cpus = set(measure_cpus), set(build_cpus)
    collisions = measure_cpus & build_cpus
    if collisions:
        raise ValueError(f"CPUs assigned to both build and measurement: {sorted(collisions)}")
    siblings = set()
    for cpu in measure_cpus:
        topology = Path(f"/sys/devices/system/cpu/cpu{cpu}/topology/thread_siblings_list")
        peers = set(cpu_list(topology.read_text().strip())) - {cpu}
        if shared := peers & build_cpus:
            raise ValueError(f"CPU {cpu} has SMT siblings in the build set: {sorted(shared)}")
        if (shared := peers & measure_cpus) and not allow_smt:
            raise ValueError(f"CPU {cpu} has SMT siblings in the measurement set: {sorted(shared)}")
        siblings.update(peers - measure_cpus)
    return sorted(siblings)


def validate_cpu_sets(parser, args, *, width=1, sequential=False, allow_smt=False, measure_affinity=None):
    if args.build_cpus is None or args.measure_cpus is None:
        parser.error("--build-cpus and --measure-cpus are required to measure")
    if len(args.measure_cpus) < width:
        parser.error(f"at least {width} measurement CPUs are required")
    assigned = os.sched_getaffinity(0)
    measure_affinity = assigned if measure_affinity is None else measure_affinity
    outside = (set(args.build_cpus) - assigned) | (set(args.measure_cpus) - measure_affinity)
    if outside:
        parser.error(f"CPUs outside the caller's affinity: {sorted(outside)}")
    try:
        return sibling_cpus(args.measure_cpus, () if sequential else args.build_cpus, allow_smt=allow_smt)
    except (ValueError, OSError) as error:
        parser.error(str(error))


def cpu_ticks(cpus):
    if not cpus:
        return {}
    names = {f"cpu{cpu}": cpu for cpu in cpus}
    ticks = {}
    with Path("/proc/stat").open() as stat:
        for line in stat:
            name, *fields = line.split()
            if name in names:
                user, nice, system, idle, iowait, irq, softirq, steal = map(int, fields[:8])
                ticks[names[name]] = (user + nice + system + irq + softirq + steal, idle + iowait)
    if missing := names.values() - ticks.keys():
        raise ValueError(f"SMT siblings missing from /proc/stat: {sorted(missing)}")
    return ticks


def busy_percent(before, after, *, unobserved=0.0):
    result = {}
    for cpu, (busy, idle) in before.items():
        active = after[cpu][0] - busy
        elapsed = active + max(0, after[cpu][1] - idle)
        if active < 0:
            raise ValueError(f"CPU {cpu} busy ticks went backwards")
        result[cpu] = 100 * active / elapsed if elapsed else unobserved
    return result


@contextmanager
def smt_activity(siblings, label=None, *, unobserved=0.0):
    before = cpu_ticks(siblings)
    busy = {}
    try:
        yield busy
    finally:
        busy.update(busy_percent(before, cpu_ticks(siblings), unobserved=unobserved))
        if busy and label is not None:
            details = ", ".join(f"CPU {cpu} {value:.2f}% busy" for cpu, value in busy.items())
            print(f"{label}: SMT {details}", file=sys.stderr, flush=True)


def run_with_smt(siblings, output, command):
    # Run inside the cap so slot admission and teardown cannot mask busy ticks.
    with smt_activity(siblings, unobserved=None) as busy:
        result = subprocess.run(command)
    output.write_text(json.dumps(busy))
    return result.returncode


if __name__ == "__main__":
    output, cpus, *command = sys.argv[1:]
    sys.exit(run_with_smt(cpu_list(cpus) if cpus else (), Path(output), command))
