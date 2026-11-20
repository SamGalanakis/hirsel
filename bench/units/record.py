#!/usr/bin/env python3
"""Measure the cost of recording on the multicore pool.

A side is BINARY:MODE. BINARY names a directory under the scratch holding
session_u and session_u_rec (see build_record.sh); MODE is off (session_u),
order (session_u_rec with HIRSEL_TAPE_ORDER_ONLY=1) or full (session_u_rec).
Each hold runs one trial per side still searching, in a rotating order.
"""

import argparse
import json
import os
from pathlib import Path
import signal
import time

import scale

CELLS = {
    "A1-256": dict(sessions=256, f=0.0, par=False, rec=True, ready_ack=True),
    "B-0.1": dict(sessions=256, f=0.1, par=True, rec=True, ready_ack=False),
    "B-0.5": dict(sessions=256, f=0.5, par=True, rec=True, ready_ack=False),
}
GUESS = {
    "A1-256": dict(off=19000, order=17000, full=7000),
    "B-0.1": dict(off=2600, order=1500, full=750),
    "B-0.5": dict(off=2500, order=1500, full=750),
}
MODES = ("off", "order", "full")


def split(side):
    binary, mode = side.rsplit(":", 1)
    if mode not in MODES:
        raise ValueError(side)
    return binary, mode


class Runner(scale.Runner):
    def __init__(self, scratch, data, env):
        super().__init__(scratch, data)
        self.extra_env = env

    def server_cmd(self, side, cell):
        binary, mode = split(side)
        argv, env, ports = super().server_cmd("untied", cell | {"rec": mode != "off"})
        name = "session_u" if mode == "off" else "session_u_rec"
        exe = self.scratch / binary / name
        if not exe.is_file():
            raise FileNotFoundError(exe)
        argv[argv.index(str(self.bin / name))] = str(exe)
        if mode == "order":
            env["HIRSEL_TAPE_ORDER_ONLY"] = "1"
        env |= self.extra_env
        return argv, env, ports

    def trial(self, trial_id, side, cell, rate, seed, tool_pid, profile_mode):
        cell = cell | {"rec": split(side)[1] != "off"}
        return super().trial(trial_id, side, cell, rate, seed, tool_pid, profile_mode)

    def profile_setup(self, mode):
        if mode == "cpu":
            return 2_000_000_000, ["-e", "cpu-clock", "-F", "499", "--call-graph", "dwarf,8192"], "3"
        if mode == "sched":
            return 2_000_000_000, ["-e", "sched:sched_switch", "-e", "sched:sched_wakeup", "-g"], "3"
        raise ValueError(mode)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("scratch", type=Path)
    parser.add_argument("data", type=Path)
    parser.add_argument("--deadline", type=float, required=True, help="absolute Unix deadline")
    parser.add_argument("--name", required=True, help="stem of the append-only JSONL receipt")
    parser.add_argument("--env", action="append", default=[], help="KEY=VALUE for every server")
    commands = parser.add_subparsers(dest="command", required=True)
    search = commands.add_parser("search")
    search.add_argument("cell", choices=CELLS)
    search.add_argument("sides", nargs="+")
    search.add_argument("--groups", type=int, nargs="+", default=[1, 2])
    fixed = commands.add_parser("fixed", help="one trial per side at a fixed rate")
    fixed.add_argument("cell", choices=CELLS)
    fixed.add_argument("rate", type=int)
    fixed.add_argument("sides", nargs="+")
    fixed.add_argument("--profile", choices=("cpu", "sched"))
    args = parser.parse_args()

    if os.sched_getaffinity(0) != {6, 7}:
        parser.error("pin this controller with taskset -c 6-7")
    if time.time() >= args.deadline:
        parser.error("measurement deadline reached")
    for side in getattr(args, "sides", []):
        split(side)
    env = dict(kv.split("=", 1) for kv in args.env)
    runner = Runner(args.scratch.resolve(), args.data.resolve(), env)
    signal.signal(signal.SIGALRM, scale.stop_at_deadline)
    signal.setitimer(signal.ITIMER_REAL, args.deadline - time.time())
    try:
        with (runner.data / f"{args.name}.jsonl").open("a") as out:
            cell = CELLS[args.cell]
            if args.command == "search":
                scale.GUESSES[args.cell] = {s: GUESS[args.cell][split(s)[1]] for s in args.sides}
                runner.search(out, args.cell, cell, args.sides, tuple(args.groups), {})
            else:
                runner.run_set(out, args.profile or "fixed", args.cell, cell, 0,
                               [(side, args.rate, 12345) for side in args.sides], args.profile)
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)


if __name__ == "__main__":
    main()
