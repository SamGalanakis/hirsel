#!/usr/bin/env python3
"""Build and run B1, the service sweep, and the two-hour HTTP soaks."""

import argparse
from contextlib import nullcontext
import os
from pathlib import Path
import subprocess
import sys
import tempfile

from b1 import ALLOWED_CPUS
from build import CAPPED, HERE, ROOT, build_b1, build_services
from services import ROUTES, SERVERS

sys.path.insert(0, str(ROOT / "core/tools"))
from timing_shield import BOARD, REGIMES, hold, measurement_affinity


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True,
                        help="directory for trial logs and the minute-by-minute soak ratios")
    parser.add_argument("--server-cpu", type=int, default=0)
    parser.add_argument("--client-cpu", type=int, default=1)
    parser.add_argument("--stage", choices=("all", "b1", "services"), default="all")
    parser.add_argument("--check", action="store_true", help="build and probe the service routes")
    parser.add_argument("--knee-only", action="store_true", help="sweep service knees without the soaks")
    parser.add_argument("--shared-timing-locks", action="store_true", help="shield the timed service sweep")
    parser.add_argument("--plan", action="store_true", help="list the service knees")
    args = parser.parse_args()
    if args.check and args.stage == "b1":
        parser.error("--check probes the service routes")
    if (args.knee_only or args.shared_timing_locks) and (args.stage != "services" or args.check):
        parser.error("service sweep options require --stage services")
    if args.plan:
        if not args.knee_only:
            parser.error("--plan requires --knee-only")
        for route in ROUTES:
            for server in SERVERS:
                print(f"{server} /{route}")
        return
    pair = {args.server_cpu, args.client_cpu}
    locks = (REGIMES, BOARD) if args.shared_timing_locks else ()
    allowed = ALLOWED_CPUS & measurement_affinity(*locks)
    if len(pair) != 2 or not pair <= allowed:
        parser.error("choose two distinct inherited CPUs within 0-11 or 16-27")
    if not locks:
        os.sched_setaffinity(0, allowed)
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    env = {**os.environ, "HIRSEL_MEMORY_LIMIT": "2G"}
    cpus = ["--server-cpu", str(args.server_cpu), "--client-cpu", str(args.client_cpu)]

    def measure(script, *arguments):
        subprocess.run(["taskset", "-c", f"{args.server_cpu},{args.client_cpu}",
                        CAPPED, sys.executable, HERE / script, "--output", output,
                        *cpus, *arguments], env=env, cwd=ROOT, check=True)

    print(f"Server CPU {args.server_cpu}, client CPU {args.client_cpu}. "
          "One server runtime worker per process.", flush=True)
    # Executables and rebuild stores are scratch. The output directory contains only trial data.
    with tempfile.TemporaryDirectory(prefix="hirsel-http-") as scratch:
        artifacts = Path(scratch)
        if args.stage in ("all", "b1") and not args.check:
            binaries, source = build_b1(artifacts)
            commands = [part for name, path in binaries.items() for part in (f"--{name}", path)]
            measure("compare_b1.py", "--source", source, *commands)
        if args.stage in ("all", "services"):
            build_services(artifacts)
            options = ["--check"] if args.check else ["--soak-seconds", "0"] if args.knee_only else []
            with hold(*locks) if locks else nullcontext():
                measure("services.py", "--artifacts", artifacts, *options)


if __name__ == "__main__":
    main()
