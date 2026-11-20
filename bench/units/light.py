#!/usr/bin/env python3
"""mc-units-light measurements: the scale2 method on separately linked pools.

Sides are directories under SCRATCH holding a `session_u` (for example
`start`, `l1`), plus `tokiow8`. The controller and perf run on CPUs 6-7.
`smoke` runs one short trial with every process on CPUs 6-7 and takes no
timing lock; it is a functional check, never a measurement.
"""

import argparse
import json
import os
from pathlib import Path
import signal
import subprocess
import time

import part3u
import scale
import scale2

CTL = {6, 7}
# Ports apart from the other pool lanes, which use 28190 and 28200-28206.
scale.PORT, scale.TOOL_PORT = 28400, 28390
CELLS = {
    "A1-256": scale.SCALE_CELLS["A1-256"],
    "A1-8192": scale.SCALE_CELLS["A1-8192"],
    "F1-256": dict(sessions=256, f=1.0, par=False, rec=False, ready_ack=False),
    "P01-256": dict(sessions=256, f=0.1, par=True, rec=False, ready_ack=False),
    # The fairness arms (queue waits recorded), for repeats outside the pair.
    "A2-0": scale.SCALE_CELLS["A1-8192"] | {"hot": False},
    "A2-1": scale.SCALE_CELLS["A1-8192"] | {"hot": True},
}
GUESS = {"A1-256": (20000, 23000), "A1-8192": (14000, 18000), "F1-256": (18000, 19000), "P01-256": (2500, 1200)}


SERVER_CPUS = [c for part in part3u.SERVER.split(",") for c in (
    range(int(part.split("-")[0]), int(part.split("-")[-1]) + 1))]


def busy_jiffies(cpus):
    """Busy time (all but idle and iowait) of each listed CPU, from /proc/stat."""
    busy = {}
    for line in open("/proc/stat"):
        name, *v = line.split()
        if name.startswith("cpu") and name[3:].isdigit() and int(name[3:]) in cpus:
            v = list(map(int, v))
            busy[int(name[3:])] = sum(v[:8]) - v[3] - v[4]
    return busy


class Runner(scale2.Runner):
    def trial(self, trial_id, side, cell, rate, seed, tool_pid, profile_mode):
        # Other processes on the server CPUs during the trial (other agents'
        # searches run unpinned): busy CPU time there minus the server's own.
        trial_id = f"{trial_id}-{time.monotonic_ns() % 1_000_000}"
        before = busy_jiffies(SERVER_CPUS)
        r = super().trial(trial_id, side, cell, rate, seed, tool_pid, profile_mode)
        after = busy_jiffies(SERVER_CPUS)
        busy_s = sum(after[c] - before[c] for c in SERVER_CPUS) / scale.CPU_HZ
        r["server_cpus_busy_s"] = busy_s
        r["foreign_cpu_s"] = busy_s - r["server_cpu_s"] if r.get("server_cpu_s") is not None else None
        scale.log(f"{trial_id} foreign {r['foreign_cpu_s']}")
        return r

    def server_cmd(self, side, cell):
        # A side directory may add `env.json`: runtime settings for that side.
        argv, env, ports = super().server_cmd(side, cell)
        extra = self.scratch / side / "env.json"
        if side != "tokiow8" and extra.is_file():
            env |= json.loads(extra.read_text())
        return argv, env, ports

    def profile_setup(self, mode):
        if mode == "sched-late":
            # Context switches with stacks over 4.5-7.5 s of the load phase.
            return 4_500_000_000, ["-e", "sched:sched_switch", "-g"], "3"
        return super().profile_setup(mode)

    def collect_load(self, load, load_pidfile, server_pid, profile):
        # scale.Runner.collect_load with perf on CPUs 6-7 (the host rule here).
        real = subprocess.Popen

        def popen(cmd, *a, **k):
            if cmd[:3] == ["taskset", "-c", "4-5"]:
                cmd = ["taskset", "-c", "6-7", *cmd[3:]]
            return real(cmd, *a, **k)

        subprocess.Popen = popen
        try:
            return super().collect_load(load, load_pidfile, server_pid, profile)
        finally:
            subprocess.Popen = real


def smoke(runner, out, side, cell, rate):
    saved = (scale.SERVER, scale.LOAD, scale.TOOL)
    scale.SERVER = scale.LOAD = scale.TOOL = "6-7"
    try:
        with runner.tool() as tool:
            r = runner.trial(f"smoke-{os.getpid()}-{side}", side, cell, rate, 12345, tool, None)
    finally:
        scale.SERVER, scale.LOAD, scale.TOOL = saved
    out.write(json.dumps(r | {"smoke": True, "discarded": True}) + "\n")
    out.flush()
    print(json.dumps({k: r.get(k) for k in ("passed", "server_rc", "load_rc", "ustats", "peak_rss_kib")}))
    print("\n".join(r["server_err"][-10:]))
    return r


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("scratch", type=Path)
    parser.add_argument("data", type=Path)
    parser.add_argument("--deadline", type=float, required=True)
    parser.add_argument("--name", required=True)
    commands = parser.add_subparsers(dest="command", required=True)
    s = commands.add_parser("search")
    s.add_argument("cell", choices=CELLS)
    s.add_argument("--groups", type=int, nargs="+", default=[1, 2])
    s.add_argument("sides", nargs="+")
    f = commands.add_parser("fixed")
    f.add_argument("cell", choices=CELLS)
    f.add_argument("rate", type=int)
    f.add_argument("sides", nargs="+")
    f.add_argument("--profile", choices=("cpu", "sched-late"))
    f.add_argument("--repeat", type=int, default=1)
    f.add_argument("--seed", type=int, default=12345)
    fa = commands.add_parser("fairness")
    fa.add_argument("side")
    fa.add_argument("best_rate", type=int)
    sm = commands.add_parser("smoke")
    sm.add_argument("cell", choices=CELLS)
    sm.add_argument("rate", type=int)
    sm.add_argument("side")
    args = parser.parse_args()

    if os.sched_getaffinity(0) != CTL:
        parser.error("pin this controller with taskset -c 6-7")
    if time.time() >= args.deadline:
        parser.error("measurement deadline reached")
    runner = Runner(args.scratch.resolve(), args.data.resolve())
    signal.signal(signal.SIGALRM, scale.stop_at_deadline)
    signal.setitimer(signal.ITIMER_REAL, args.deadline - time.time())
    try:
        with (runner.data / f"{args.name}.jsonl").open("a") as out:
            if args.command == "search":
                pool, tokio = GUESS[args.cell]
                scale.GUESSES[args.cell] = {s: (tokio if s == "tokiow8" else pool) for s in args.sides}
                runner.search(out, args.cell, CELLS[args.cell], args.sides, args.groups, {})
            elif args.command == "fixed":
                for i in range(args.repeat):
                    sides = args.sides[i % len(args.sides):] + args.sides[:i % len(args.sides)]
                    runner.run_set(out, f"fixed-{args.profile or 'plain'}", args.cell, CELLS[args.cell], 0,
                                   [(side, args.rate, args.seed) for side in sides], args.profile)
            elif args.command == "fairness":
                for hot in (False, True):
                    cell = CELLS["A1-8192"] | {"hot": hot}
                    runner.run_set(out, "fairness", f"A2-{int(hot)}", cell, 1,
                                   [(args.side, args.best_rate / 2, 732242)])
            else:
                smoke(runner, out, args.side, CELLS[args.cell], args.rate)
    except scale.Deadline:
        scale.log("measurement deadline reached")
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)


if __name__ == "__main__":
    main()
