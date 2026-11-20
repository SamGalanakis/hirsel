#!/usr/bin/env python3
"""Repeat the mc-units-scale experiment with separately linked pool binaries."""

import argparse
import os
from pathlib import Path
import signal
import time

import scale


class Runner(scale.Runner):
    def server_cmd(self, side, cell):
        if side == "tokiow8":
            return super().server_cmd(side, cell)
        argv, env, ports = super().server_cmd("untied", cell)
        exe = self.scratch / side / "session_u"
        if not exe.is_file():
            raise FileNotFoundError(exe)
        argv[argv.index(str(self.bin / "session_u"))] = str(exe)
        return argv, env, ports

    def profile_setup(self, mode):
        if mode == "drain":
            return 5_500_000_000, [
                "-e", "cpu-clock", "-F", "499", "--call-graph", "dwarf,8192",
            ], "5"
        if mode == "drain-sched":
            return 5_500_000_000, [
                "-e", "sched:sched_switch", "-g",
            ], "5"
        if mode == "cpu":
            return 2_000_000_000, [
                "-e", "cpu-clock", "-F", "499", "--call-graph", "dwarf,8192",
            ], "3"
        if mode == "sync":
            return 100_000_000, [
                "-e", "syscalls:sys_enter_fdatasync", "-e", "syscalls:sys_exit_fdatasync",
            ], "9"
        return super().profile_setup(mode)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("scratch", type=Path)
    parser.add_argument("data", type=Path)
    parser.add_argument("--deadline", type=float, required=True, help="absolute Unix deadline")
    parser.add_argument("--name", required=True, help="stem of the append-only JSONL receipt")
    commands = parser.add_subparsers(dest="command", required=True)
    search = commands.add_parser("search")
    search.add_argument("sides", nargs="+")
    profile = commands.add_parser("profile")
    profile.add_argument("mode", choices=("cpu", "drain", "drain-sched", "sync"))
    profile.add_argument("rate", type=int)
    profile.add_argument("sides", nargs="+")
    profile.add_argument("--sessions", type=int, choices=(256, 8192), default=256)
    fairness = commands.add_parser("fairness")
    fairness.add_argument("side")
    fairness.add_argument("best_rate", type=int)
    args = parser.parse_args()

    if os.sched_getaffinity(0) != {4, 5}:
        parser.error("pin this controller with taskset -c 4-5")
    if time.time() >= args.deadline:
        parser.error("measurement deadline reached")
    runner = Runner(args.scratch.resolve(), args.data.resolve())
    signal.signal(signal.SIGALRM, scale.stop_at_deadline)
    signal.setitimer(signal.ITIMER_REAL, args.deadline - time.time())
    try:
        with (runner.data / f"{args.name}.jsonl").open("a") as out:
            if args.command == "search":
                for name in ("A1-256", "A1-8192"):
                    guesses = scale.GUESSES[name]
                    for side in args.sides:
                        guesses.setdefault(side, guesses["untied"])
                    runner.search(out, name, scale.SCALE_CELLS[name], args.sides, (1, 2), {})
            elif args.command == "profile":
                drain = args.mode.startswith("drain")
                name = "A1-8192" if drain else f"A1-{args.sessions}"
                cell = scale.SCALE_CELLS[name]
                if drain:
                    cell = cell | {"hot": True}
                runner.run_set(
                    out, args.mode, name, cell, 0,
                    [(side, args.rate, 732242 if drain else 12345)
                     for side in args.sides],
                    args.mode,
                )
            else:
                for hot in (False, True):
                    cell = scale.SCALE_CELLS["A1-8192"] | {"hot": hot}
                    runner.run_set(
                        out, "fairness", f"A2-{int(hot)}", cell, 1,
                        [(args.side, args.best_rate / 2, 732242)],
                    )
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)


if __name__ == "__main__":
    main()
