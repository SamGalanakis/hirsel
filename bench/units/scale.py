#!/usr/bin/env python3
"""Run the session-scale, fairness and full-recording measurements.

Use a fresh scratch directory for the binaries, partition files, and tapes.
The output directory keeps the measurements and profiles, including failed
and discarded trials. See build_scale.sh for the binary layout.
"""

import argparse
from bisect import bisect_right
from contextlib import contextmanager
from datetime import datetime
import fcntl
import json
import os
from pathlib import Path
import random
import signal
import subprocess
import time

from blocks import BOARD, REGIMES, board_window_ok, cpuset
from part3u import (
    PARTITIONS, RUN, SYNCS, TOOL_DELAY_US, complete_in_time, search_steps,
)


PORT = 28200
# Session units behind the single listening port.
SESSION_UNITS = 8
TOOL_PORT = 28190
SERVER = "0-3,16-19"
LOAD = "24-25"
TOOL = "26-27"
CONTROLLER = "26-27"
NOFILE = 524288
CPU_HZ = os.sysconf("SC_CLK_TCK")
HOLDS_GAP_S = 15
LOAD_NAMES = {"loadgen", "loadgen_drain", "loadgen_detail", "loadgen_churn"}

SCALE_SIDES = ("untied", "tokiow8")
REC_SIDES = ("untied", "S3k7", "S1w2", "S1w8", "untied (1 GiB)")
SCALE_CELLS = {
    f"A1-{n}": dict(sessions=n, f=0.0, par=False, rec=False, ready_ack=True, fair=True)
    for n in (256, 2048, 8192)
}
REC_CELLS = {
    f"B-{f}": dict(sessions=256, f=f, par=True, rec=True, ready_ack=False)
    for f in (0.5, 0.1)
}
GUESSES = {
    **{cell: dict.fromkeys(SCALE_SIDES, 6000) for cell in SCALE_CELLS},
    **{cell: dict(zip(REC_SIDES, (750, 32, 48, 30, 1050), strict=True)) for cell in REC_CELLS},
}


def log(message):
    print(f"{datetime.now().isoformat(timespec='seconds')} {message}", flush=True)


def cc1plus():
    result = subprocess.run(["pgrep", "-x", "cc1plus"], capture_output=True, text=True)
    if result.returncode not in (0, 1):
        raise RuntimeError(f"pgrep failed: {result.stderr.strip()}")
    return result.stdout.split()


class Deadline(Exception):
    pass


class PortBusy(RuntimeError):
    pass


def stop_at_deadline(*_):
    raise Deadline


@contextmanager
def timing_hold(receipt):
    # Check again after the blocking acquisitions because the board window
    # can close while another lane owns the lock.
    while not board_window_ok():
        time.sleep(HOLDS_GAP_S)
    with open(REGIMES, "a") as regimes, open(BOARD, "a") as board:
        fcntl.flock(regimes, fcntl.LOCK_EX)
        fcntl.flock(board, fcntl.LOCK_EX)
        receipt["started"] = datetime.now().astimezone().isoformat()
        t0 = time.monotonic()
        try:
            receipt["cc1plus_start"] = cc1plus()
            receipt["board_window_open"] = board_window_ok()
            receipt["load_start"] = os.getloadavg()[0]
            yield receipt["board_window_open"]
        finally:
            try:
                receipt["cc1plus_end"] = cc1plus()
                receipt["load_end"] = os.getloadavg()[0]
                receipt["hold_s"] = round(time.monotonic() - t0, 3)
            finally:
                fcntl.flock(board, fcntl.LOCK_UN)
                fcntl.flock(regimes, fcntl.LOCK_UN)


def launch(argv, pidfile, **kwargs):
    proc = subprocess.Popen(argv, **kwargs)
    pidfile.write_text(f"{proc.pid}\n")
    return proc


def stop_owned(proc, pidfile, names):
    if proc.returncode is not None:
        return
    pid = int(pidfile.read_text())
    name = subprocess.run(
        ["ps", "-o", "comm=", "-p", str(pid)], capture_output=True, text=True, check=False,
    ).stdout.strip()
    if pid != proc.pid:
        raise RuntimeError(f"PID file disagrees with child handle: {pidfile}")
    if name in names | {"taskset", "prlimit"} and proc.poll() is None:
        os.kill(pid, signal.SIGKILL)
    elif name and proc.poll() is None:
        raise RuntimeError(f"unexpected command for child PID {pid}: {name}")
    proc.wait()


def pinned(cpus, argv):
    return ["taskset", "-c", cpus, "prlimit", f"--nofile={NOFILE}:{NOFILE}", "--", *map(str, argv)]


def proc_sample(pid):
    try:
        stat = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
    except FileNotFoundError:
        return None
    return time.monotonic_ns(), int(stat[11]) + int(stat[12]), int(stat[17])


def cpu_at(samples, when):
    i = bisect_right(samples, when, key=lambda sample: sample[0])
    if not 0 < i < len(samples):
        return None
    before, after = samples[i - 1], samples[i]
    mix = (when - before[0]) / (after[0] - before[0])
    return before[1] + (after[1] - before[1]) * mix


def poll_server(server, timeout):
    end = time.monotonic() + timeout
    while True:
        pid, status, usage = os.wait4(server.pid, os.WNOHANG)
        if pid:
            server.returncode = os.waitstatus_to_exitcode(status)
            return usage
        if time.monotonic() >= end:
            return None
        time.sleep(0.05)


def masks(pid):
    result = {}
    for p in Path(f"/proc/{pid}/task").glob("*/status"):
        try:
            fields = dict(line.split(":\t", 1) for line in p.read_text().splitlines() if ":\t" in line)
            key = f"{fields['Name']}:{fields['Cpus_allowed_list']}"
            result[key] = result.get(key, 0) + 1
        except FileNotFoundError:
            continue
    return result


def assert_ports_free(ports):
    result = subprocess.run(["ss", "-H", "-ltn"], capture_output=True, text=True, check=True)
    listening = {int(row.split()[3].rsplit(":", 1)[1]) for row in result.stdout.splitlines()}
    if busy := listening.intersection(ports):
        raise PortBusy(f"unexpected listener on {sorted(busy)}")


class Runner:
    def __init__(self, scratch, data, *, hold_guard=None, cpu_sets=None):
        self.scratch = scratch
        self.data = data
        self.bin = scratch / "bin"
        self.store = scratch / "store"
        self.tapes = scratch / "tapes"
        self.store.mkdir(exist_ok=True)
        self.tapes.mkdir(exist_ok=True)
        self.active_hold = None
        self.hold_guard = hold_guard or timing_hold
        self.cpu_sets = cpu_sets or {}
        self.hold = 0
        self.last_order = None

    @contextmanager
    def tool(self, precise=False):
        assert_ports_free([TOOL_PORT])
        pidfile = self.scratch / "tool.pid"
        with (self.scratch / "tool.err").open("w+") as err:
            cpus = self.cpu_sets.get("helper", TOOL)
            proc = launch(pinned(cpus, [self.bin / "toolsrv", TOOL_PORT, TOOL_DELAY_US,
                                        len(cpuset(cpus)), int(precise)]), pidfile, stderr=err)
            try:
                for _ in range(50):
                    if f"toolsrv ready port={TOOL_PORT}" in (self.scratch / "tool.err").read_text() and proc.poll() is None:
                        yield proc.pid
                        return
                    if proc.poll() is not None:
                        break
                    time.sleep(0.02)
                err.seek(0)
                raise RuntimeError(f"tool server failed to start: {err.read()}")
            finally:
                stop_owned(proc, pidfile, {"toolsrv"})

    def server_cmd(self, side, cell):
        env = {"HIRSEL_SYNC_HELPERS": str(SYNCS)}
        total_clients = cell["sessions"] + int(cell.get("churn_per_s", 0) * RUN["dur"])
        base = [PORT, total_clients, PARTITIONS, int(cell["par"]), TOOL_PORT]
        recorded = cell["rec"]
        if side == "tokiow8":
            twin = self.scratch / "software/twin" if cell.get("software_sha") else self.bin / "twin"
            argv = [twin, *base, 8, SYNCS, self.store]
        elif side.startswith("untied"):
            env |= {"HIRSEL_UNTIED": "8", "HIRSEL_XSTATS": "1"}
            if cell.get("software_sha"):
                env["HIRSEL_SHA256"] = "portable"
            if side == "untied (1 GiB)":
                env["HIRSEL_RETENTION_MB"] = "1024"
            if recorded:
                raise ValueError("session_u has no recorded build: a recording holds the root alone")
            argv = [self.bin / "session_u", *base, 0 if cell.get("fair") else SESSION_UNITS]
        else:
            raise ValueError(f"side {side}: the ring sides (S3k7, S1w*) went with the ring")
        if cell["ready_ack"]:
            argv.append(1)
        if side == "tokiow8" and cell.get("fair"):
            argv.append(1)
        env = {k: v for k, v in os.environ.items() if not k.startswith("HIRSEL_")} | env
        return pinned(self.cpu_sets.get("server", SERVER), argv), env

    def profile_setup(self, mode):
        event = ["-F", "999"] if mode == "cpu" else ["-e", "sched:sched_switch"]
        return 2_000_000_000, ["-g", *event], "3"

    def collect_load(self, load, load_pidfile, server_pid, profile):
        samples, masks_seen = [], {}
        profile_proc = None
        profile_pidfile = self.scratch / "perf.pid"
        if profile:
            offset, events, duration = self.profile_setup(profile["mode"])
        started = time.monotonic()
        # The load generator writes the scheduled start after setup.
        # Profile only during the timed phase, never during 8K connection setup.
        marker = self.scratch / "load-phase"
        try:
            while time.monotonic() - started < 35:
                sample = proc_sample(server_pid)
                if sample:
                    samples.append(sample)
                phase_ns = int(marker.read_text()) if marker.exists() else None
                if not masks_seen and phase_ns is not None and time.monotonic_ns() >= phase_ns + 1_000_000_000:
                    masks_seen = masks(server_pid)
                if profile and profile_proc is None and phase_ns is not None:
                    if time.monotonic_ns() >= phase_ns + offset:
                        cmd = ["taskset", "-c", self.cpu_sets.get("controller", CONTROLLER), "perf", "record", "-N", "--clockid", "mono",
                               *events, "-o", str(profile["path"]), "-p", str(server_pid), "--", "sleep", duration]
                        profile["launch_ns"] = time.monotonic_ns()
                        with profile["path"].with_suffix(".log").open("w") as err:
                            profile_proc = launch(cmd, profile_pidfile, stdout=err, stderr=err)
                        profile["pid"] = profile_proc.pid
                try:
                    out, err = load.communicate(timeout=0.075)
                    sample = proc_sample(server_pid)
                    if sample:
                        samples.append(sample)
                    return out, err, samples, masks_seen, False
                except subprocess.TimeoutExpired:
                    continue
            stop_owned(load, load_pidfile, LOAD_NAMES)
            out, err = load.communicate()
            return out, err, samples, masks_seen, True
        finally:
            if profile_proc is not None:
                # Wait even if the diagnostic outlasts the server.
                profile["rc"] = profile_proc.wait(timeout=5)

    def trial(self, trial_id, side, cell, rate, seed, tool_pid, profile_mode):
        assert_ports_free([PORT])
        for directory in (self.store, self.tapes):
            for file in directory.iterdir():
                file.unlink()
        marker = self.scratch / "load-phase"
        marker.unlink(missing_ok=True)
        server_pidfile = self.scratch / "server.pid"
        load_pidfile = self.scratch / "load.pid"
        server_argv, env = self.server_cmd(side, cell)
        if cell.get("churn_per_s"):
            load_name = "loadgen_churn"
        elif cell.get("slow_turns"):
            load_name = "loadgen_detail"
        else:
            load_name = "loadgen_drain" if profile_mode or "close_after_drain" in cell else "loadgen"
        load_cpus = self.cpu_sets.get("load", LOAD)
        lg_argv = [
            self.bin / load_name, f"port={PORT}", f"sessions={cell['sessions']}",
            f"rate={rate:.0f}", f"f={int(cell['f'] * 1000)}", f"par={int(cell['par'])}",
            f"seed={seed}", f"threads={len(cpuset(load_cpus))}", "grace=5", f"ready_ack={int(cell['ready_ack'])}",
            f"reserved_hot={int('hot' in cell and cell.get('skew_clients', 0) == 0)}",
            f"hot={int(cell.get('hot', False))}", f"skew_clients={cell.get('skew_clients', 0)}",
            f"close_after_drain={int(cell.get('close_after_drain', False))}",
            f"phase_file={marker}", *(f"{k}={v}" for k, v in RUN.items()),
        ]
        if cell.get("fair") and not cell.get("hot") and not cell.get("churn_per_s"):
            lg_argv.append("pipeline=1")
        if cell.get("slow_turns"):
            lg_argv.append(f"slow_turns={self.data / (trial_id + '.tsv')}")
        if cell.get("churn_per_s"):
            lg_argv.append(f"churn_per_s={cell['churn_per_s']}")
        profile = None
        if profile_mode:
            profile = {"mode": profile_mode, "path": self.data / f"{trial_id}.perf.data"}
        r = dict(side=side, cell=cell, trial=trial_id, rate=round(rate), search_rate=rate, seed=seed, nofile=NOFILE)
        started = time.monotonic()
        arm_start_ns = time.monotonic_ns()
        tool0 = proc_sample(tool_pid)
        with (self.scratch / "server.err").open("w+") as err:
            server = launch(server_argv, server_pidfile, env=env, stderr=err)
            if self.active_hold is not None and "arm_pids" in self.active_hold:
                self.active_hold["arm_pids"].append(server.pid)
            try:
                load = launch(pinned(load_cpus, lg_argv), load_pidfile, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
                try:
                    out, load_err, samples, seen_masks, load_timeout = self.collect_load(load, load_pidfile, server.pid, profile)
                finally:
                    stop_owned(load, load_pidfile, LOAD_NAMES)
                usage = poll_server(server, 3)
            finally:
                stop_owned(server, server_pidfile, {"session", "session_rec", "session_u", "session_u_rec", "twin"})
            err.seek(0)
            server_err = err.read().splitlines()
        elapsed = time.monotonic() - started
        arm_end_ns = time.monotonic_ns()
        tool1 = proc_sample(tool_pid)
        try:
            result = json.loads(out.strip().splitlines()[-1])
        except (ValueError, IndexError):
            result = None

        if self.active_hold is not None and "arm_windows" in self.active_hold:
            phase = result or {"warm_ns": arm_start_ns, "end_ns": arm_end_ns}
            self.active_hold["arm_windows"].append(
                dict(side=side, start_ns=phase["warm_ns"], end_ns=phase["end_ns"],
                     pids=[server.pid, load.pid, tool_pid]))

        ustats = None
        memory = None
        ordinary_err = []
        for line in server_err:
            if line.startswith("untied-stats "):
                ustats = {key: value if key == "net_reactor" else int(value)
                          for key, value in (kv.split("=", 1) for kv in line.split()[1:])}
            elif line.startswith("pool-memory "):
                memory = {key: int(value) for key, value in (kv.split("=", 1) for kv in line.split()[1:])}
            else:
                ordinary_err.append(line)
        summary = next((line for line in ordinary_err
                        if line.startswith(("session turns=", "twin turns="))), None)
        server_counts = (dict((key, int(value)) for key, value in
                              (word.split("=") for word in summary.split()[1:]))
                         if summary else {})
        full_pass = bool(
            complete_in_time(result) and not load_timeout and load.returncode == server.returncode == 0
            and result["issued"] == server_counts.get("turns")
        )
        if memory is not None:
            r["pool_memory"] = memory
        cpu = usage.ru_utime + usage.ru_stime if usage else None
        cpu_warm = cpu_at(samples, result["warm_ns"]) if result else None
        cpu_end = cpu_at(samples, result["end_ns"]) if result else None
        tape_files = list(self.tapes.iterdir()) if cell["rec"] else []
        tape_bytes = sum(p.stat().st_size for p in tape_files)
        r.update(
            load=result, load_rc=load.returncode, load_err=load_err[-12000:], load_timeout=load_timeout,
            server_pid=server.pid, load_pid=load.pid, tool_pid=tool_pid,
            server_rc=server.returncode, server_err=ordinary_err[-40:], masks=seen_masks, ustats=ustats,
            sync_groups=server_counts.get("groups"),
            sync_groups_per_turn=server_counts["groups"] / server_counts["turns"]
            if server_counts.get("turns") else None,
            wall_s=elapsed, server_cpu_s=cpu, peak_rss_kib=usage.ru_maxrss if usage else None,
            peak_threads_sampled=max((sample[2] for sample in samples), default=None),
            server_cpu_us_per_turn=cpu * 1e6 / result["completed"] if cpu and result and result["completed"] else None,
            server_measured_cpu_us_per_turn=(
                (cpu_end - cpu_warm) / CPU_HZ * 1e6 / result["completed_after_warm_in_run"]
                if cpu_end is not None and cpu_warm is not None and result["completed_after_warm_in_run"] else None
            ),
            tool_cpu_s=(tool1[1] - tool0[1]) / CPU_HZ if tool0 and tool1 else None,
            steals_per_s=ustats["steals"] / elapsed if ustats else None,
            cpu_samples=samples,
            tape_disk_bytes=tape_bytes if cell["rec"] else None,
            tape_files=len(tape_files) if cell["rec"] else None,
            passed=full_pass,
        )
        if cell["rec"] and full_pass and not tape_bytes:
            raise RuntimeError(f"{side} completed a recorded trial without producing a tape")
        if profile:
            r["profile"] = profile | {"path": profile["path"].name}
        l = result or {}
        log(f"{trial_id} {side} {rate:.0f}/s pass={full_pass} intended p99={l.get('e2e_p99_ns', 0) / 1e6:.3f} ms "
            f"done={l.get('completed')}/{l.get('issued')} late={l.get('late')} rc={server.returncode}/{load.returncode}")
        return r

    def run_set(self, out, phase, cell_name, cell, group, items, profile=None):
        """Run one trial per side under the locks, rerunning a discarded hold."""
        while True:
            self.hold += 1
            if len(items) == 2 and tuple(side for side, _, _ in items) == self.last_order:
                items = items[::-1]
            receipt = dict(hold=self.hold, phase=phase, cell_name=cell_name, group=group,
                           order=[side for side, _, _ in items], runner_pid=os.getpid())
            records = []
            self.active_hold = receipt
            try:
                with self.hold_guard(receipt) as usable:
                    if usable:
                        with self.tool(precise=cell.get("fair", False)) as tool:
                            self.last_order = tuple(receipt["order"])
                            for side, rate, seed in items:
                                tag = f"{phase}-{cell_name}-{os.getpid()}-{self.hold}-{side.replace(' ', '_')}"
                                records.append(self.trial(tag, side, cell, rate, seed, tool, profile))
            except PortBusy as error:
                receipt["port_conflict"] = str(error)
            finally:
                self.active_hold = None
                clean = (receipt.get("clean", receipt.get("board_window_open", False))
                         and "port_conflict" not in receipt and receipt.get("hold_s", 240) < 240
                         and len(records) == len(items))
                rows = [r | receipt | {"discarded": not clean} for r in records]
                for row in rows:
                    out.write(json.dumps(row) + "\n")
                out.write(json.dumps(receipt | {"hold_summary": True, "discarded": not clean}) + "\n")
                out.flush()
            log(f"hold {self.hold} {cell_name} {receipt.get('hold_s')} s clean={clean}")
            time.sleep(HOLDS_GAP_S)
            if clean:
                return rows

    def search(self, out, cell_name, cell, sides, groups, best):
        for group in groups:
            rng = random.Random(f"{cell_name}-{group}")
            seed = rng.randrange(1, 1 << 30)
            guesses = GUESSES[cell_name]
            gens = {s: search_steps((best.get(s) or guesses[s]) * 0.9) for s in sides}
            pending = {s: next(g) for s, g in gens.items()}
            control_rates = pending.copy()
            misses = {s: 0 for s in sides}
            step = group - 1
            while pending:
                offset = step % len(sides)
                ordered = sides[offset:] + sides[:offset]
                items = [(s, pending.get(s, control_rates[s]), seed) for s in ordered]
                records = self.run_set(out, "search", cell_name, cell, group, items)
                step += 1
                for r in records:
                    side = r["side"]
                    if side not in pending:
                        continue
                    if r["load"] is None:
                        misses[side] += 1
                        if misses[side] < 2:
                            continue
                        done = dict(max_rate=None, incomplete=True)
                    else:
                        misses[side] = 0
                        try:
                            pending[side] = gens[side].send(r["passed"])
                            continue
                        except StopIteration as e:
                            done = dict(max_rate=e.value, incomplete=False)
                            if e.value:
                                control_rates[side] = e.value
                                best[side] = max(e.value, best.get(side, 0))
                    del pending[side]
                    out.write(json.dumps(dict(summary=True, side=side, cell_name=cell_name, cell=cell, group=group) | done) + "\n")
                    out.flush()
                    log(f"{cell_name} search {group} {side}: {done}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("scratch", type=Path)
    parser.add_argument("data", type=Path)
    parser.add_argument("--deadline", type=float, required=True, help="absolute Unix time to stop")
    parser.add_argument("--name", required=True, help="name of append-only run log")
    parser.add_argument("--software-sha", action="store_true", help="run the scalar SHA-256 control on both ports")
    commands = parser.add_subparsers(dest="command", required=True)
    pilot = commands.add_parser("pilot")
    pilot.add_argument("cell", choices=SCALE_CELLS | REC_CELLS)
    search = commands.add_parser("search")
    search.add_argument("cells", nargs="+", choices=SCALE_CELLS | REC_CELLS)
    search.add_argument("--groups", type=int, nargs="+", default=[1, 2])
    fairness = commands.add_parser("fairness")
    fairness.add_argument("sessions", type=int)
    fairness.add_argument("rates", help="JSON mapping of side to best passing A1 rate")
    drain = commands.add_parser("drain")
    drain.add_argument("rate", type=float, help="open-loop rate from the untied hot-session run")
    attrib = commands.add_parser("profile")
    attrib.add_argument("cell", choices=SCALE_CELLS | REC_CELLS)
    attrib.add_argument("mode", choices=["cpu", "sched"])
    attrib.add_argument("rates", help="JSON mapping of side to fixed offered rate")
    attrib.add_argument("--hot", type=int, choices=(0, 1), help="use the paired fairness workload")
    attrib.add_argument("--ready-ack", action="store_true", help="wait for session setup before offering load")
    args = parser.parse_args()
    if os.sched_getaffinity(0) != cpuset(CONTROLLER):
        parser.error(f"pin this controller with taskset -c {CONTROLLER}")
    if time.time() >= args.deadline:
        raise SystemExit("measurement deadline reached")
    signal.signal(signal.SIGALRM, stop_at_deadline)
    signal.setitimer(signal.ITIMER_REAL, args.deadline - time.time())
    runner = Runner(args.scratch.resolve(), args.data.resolve())
    configs = {name: cell | {"software_sha": args.software_sha}
               for name, cell in (SCALE_CELLS | REC_CELLS).items()}
    try:
        with (args.data / f"{args.name}.jsonl").open("a") as out:
            if args.command == "pilot":
                sides = SCALE_SIDES if args.cell in SCALE_CELLS else REC_SIDES
                guesses = GUESSES[args.cell]
                items = [(s, 1500 if args.cell in SCALE_CELLS else guesses[s], 12345) for s in sides]
                runner.run_set(out, "pilot", args.cell, configs[args.cell], 0, items)
            elif args.command == "search":
                for name in args.cells:
                    sides = SCALE_SIDES if name in SCALE_CELLS else REC_SIDES
                    runner.search(out, name, configs[name], sides, args.groups, {})
            elif args.command == "fairness":
                rate = json.loads(args.rates)
                for hot in (False, True):
                    cell = configs[f"A1-{args.sessions}"] | {"hot": hot}
                    sides = SCALE_SIDES if not hot else SCALE_SIDES[1:] + SCALE_SIDES[:1]
                    runner.run_set(out, "fairness", f"A2-{int(hot)}", cell, 1, [(s, rate[s] / 2, 732242) for s in sides if s in rate])
            elif args.command == "drain":
                for defer in (False, True):
                    cell = configs["A1-8192"] | {"hot": True, "close_after_drain": defer}
                    runner.run_set(out, "drain", f"A2-{int(defer)}", cell, 1, [("untied", args.rate, 732242)])
            else:
                rates = json.loads(args.rates)
                cell = configs[args.cell]
                if args.hot is not None:
                    cell |= {"hot": bool(args.hot)}
                if args.ready_ack:
                    cell |= {"ready_ack": True}
                runner.run_set(out, "profile", args.cell, cell, 0, [(s, rate, 12345) for s, rate in rates.items()], args.mode)
    except Deadline:
        log("measurement deadline reached")
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)


if __name__ == "__main__":
    main()
