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

from blocks import BOARD, REGIMES, board_window_ok
from part3u import LIMIT_NS, LOAD, PARTITIONS, RUN, SERVER, SYNCS, TOOL, TOOL_DELAY_US, search_steps


PORT = 28200
TOOL_PORT = 28190
NOFILE = 524288
CPU_HZ = os.sysconf("SC_CLK_TCK")
HOLDS_GAP_S = 15
QUEUE_BUCKET_NS = 10_000_000
LOAD_NAMES = {"loadgen", "loadgen_drain", "loadgen_detail"}

SCALE_SIDES = ("untied", "S3k7", "tokiow8")
REC_SIDES = ("untied", "S3k7", "S1w2", "S1w8", "untied (1 GiB)")
SCALE_CELLS = {
    f"A1-{n}": dict(sessions=n, f=0.0, par=False, rec=False, ready_ack=True)
    for n in (256, 2048, 8192)
}
REC_CELLS = {
    f"B-{f}": dict(sessions=256, f=f, par=True, rec=True, ready_ack=False)
    for f in (0.5, 0.1)
}
GUESSES = {
    "A1-256": dict(zip(SCALE_SIDES, (20000, 23000, 22000), strict=True)),
    "A1-2048": dict(zip(SCALE_SIDES, (19000, 20000, 22000), strict=True)),
    "A1-8192": dict(zip(SCALE_SIDES, (18000, 19000, 21000), strict=True)),
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


def stop_at_deadline(*_):
    raise Deadline


@contextmanager
def timing_hold(receipt):
    # Check again after the blocking acquisitions because the board window
    # can close while another lane owns the lock.
    while not board_window_ok():
        time.sleep(60)
    with open(REGIMES, "a") as regimes, open(BOARD, "a") as board:
        fcntl.flock(regimes, fcntl.LOCK_EX)
        fcntl.flock(board, fcntl.LOCK_EX)
        receipt["started"] = datetime.now().astimezone().isoformat()
        t0 = time.monotonic()
        try:
            receipt["cc1plus_start"] = cc1plus()
            receipt["board_window_open"] = board_window_ok()
            yield not receipt["cc1plus_start"] and receipt["board_window_open"]
        finally:
            try:
                receipt["cc1plus_end"] = cc1plus()
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
        raise RuntimeError(f"unexpected listener on {sorted(busy)}")


class Runner:
    def __init__(self, scratch, data):
        self.scratch = scratch
        self.data = data
        self.bin = scratch / "bin"
        self.store = scratch / "store"
        self.tapes = scratch / "tapes"
        self.store.mkdir(exist_ok=True)
        self.tapes.mkdir(exist_ok=True)
        self.hold = 0

    @contextmanager
    def tool(self):
        assert_ports_free([TOOL_PORT])
        pidfile = self.scratch / "tool.pid"
        with (self.scratch / "tool.err").open("w+") as err:
            proc = launch(pinned(TOOL, [self.bin / "toolsrv", TOOL_PORT, TOOL_DELAY_US, 4]), pidfile, stderr=err)
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
        base = [PORT, cell["sessions"], PARTITIONS, int(cell["par"]), TOOL_PORT]
        recorded = cell["rec"]
        nports = 1
        if side == "tokiow8":
            argv = [self.bin / "twin", *base, 8, SYNCS, self.store]
        elif side.startswith("untied"):
            env |= {
                "HIRSEL_UNTIED": "8", "HIRSEL_UNITS": str(PARTITIONS + 1),
                "HIRSEL_FRONT": str(PORT), "HIRSEL_XSTATS": "1",
            }
            if side == "untied (1 GiB)":
                env["HIRSEL_RETENTION_MB"] = "1024"
            if "hot" in cell:
                env["HIRSEL_UNTIED_QUEUE_WAIT"] = "1"
            name = "session_u_rec" if recorded else "session_u"
            argv = [self.bin / name, *base, 2]
        else:
            if side == "S3k7":
                nports = 7
                env |= {"HIRSEL_UNITS": "8", "HIRSEL_XRING": "65536"}
            else:
                env["HIRSEL_S8_WORKERS"] = side.removeprefix("S1w")
            name = "session_rec" if recorded else "session"
            argv = [self.bin / name, *base, int(side == "S3k7")]
        if cell["ready_ack"]:
            argv.append(1)
        env = {k: v for k, v in os.environ.items() if not k.startswith("HIRSEL_")} | env
        return pinned(SERVER, argv), env, nports

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
                        cmd = ["taskset", "-c", "6-7", "perf", "record", "-N", "--clockid", "mono",
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
        assert_ports_free(range(PORT, PORT + 7))
        for directory in (self.store, self.tapes):
            for file in directory.iterdir():
                file.unlink()
        marker = self.scratch / "load-phase"
        marker.unlink(missing_ok=True)
        server_pidfile = self.scratch / "server.pid"
        load_pidfile = self.scratch / "load.pid"
        server_argv, env, nports = self.server_cmd(side, cell)
        if cell.get("slow_turns"):
            load_name = "loadgen_detail"
        else:
            load_name = "loadgen_drain" if profile_mode or "close_after_drain" in cell else "loadgen"
        lg_argv = [
            self.bin / load_name, f"port={PORT}", f"nports={nports}", f"sessions={cell['sessions']}",
            f"rate={rate:.0f}", f"f={int(cell['f'] * 1000)}", f"par={int(cell['par'])}",
            f"seed={seed}", "threads=4", "grace=5", f"ready_ack={int(cell['ready_ack'])}",
            f"reserved_hot={8 if 'hot' in cell else 0}", f"hot={int(cell.get('hot', False))}",
            f"close_after_drain={int(cell.get('close_after_drain', False))}",
            f"phase_file={marker}", *(f"{k}={v}" for k, v in RUN.items()),
        ]
        if cell.get("slow_turns"):
            lg_argv.append(f"slow_turns={self.data / (trial_id + '.tsv')}")
        profile = None
        if profile_mode:
            profile = {"mode": profile_mode, "path": self.data / f"{trial_id}.perf.data"}
        r = dict(side=side, cell=cell, trial=trial_id, rate=round(rate), search_rate=rate, seed=seed, nofile=NOFILE)
        started = time.monotonic()
        tool0 = proc_sample(tool_pid)
        with (self.scratch / "server.err").open("w+") as err:
            server = launch(server_argv, server_pidfile, env=env, stderr=err)
            try:
                load = launch(pinned(LOAD, lg_argv), load_pidfile, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
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
        tool1 = proc_sample(tool_pid)
        try:
            result = json.loads(out.strip().splitlines()[-1])
        except (ValueError, IndexError):
            result = None

        ustats = None
        queue = []
        ordinary_err = []
        for line in server_err:
            if line.startswith("untied-stats "):
                ustats = {key: int(value) for key, value in (kv.split("=", 1) for kv in line.split()[1:])}
            elif line.startswith("queue-wait "):
                queue.append(list(map(int, line.split()[1:])))
            else:
                ordinary_err.append(line)
        full_pass = bool(
            result and load.returncode == 0 and server.returncode == 0 and
            result["failures"] == result["timeouts"] == result["late"] == 0 and
            result["completed"] == result["issued"] and result["p99_ns"] <= LIMIT_NS
        )
        cpu = usage.ru_utime + usage.ru_stime if usage else None
        cpu_warm = cpu_at(samples, result["warm_ns"]) if result else None
        cpu_end = cpu_at(samples, result["end_ns"]) if result else None
        tape_files = list(self.tapes.iterdir()) if cell["rec"] else []
        tape_bytes = sum(p.stat().st_size for p in tape_files)
        r.update(
            load=result, load_rc=load.returncode, load_err=load_err[-12000:], load_timeout=load_timeout,
            server_pid=server.pid, load_pid=load.pid, tool_pid=tool_pid,
            server_rc=server.returncode, server_err=ordinary_err[-40:], masks=seen_masks, ustats=ustats,
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
        if queue and result:
            a, b = result["warm_ns"], result["end_ns"]
            r["queue_wait_buckets"] = queue
            r["queue_wait_max_ns"] = max((wait for at, wait in queue if a <= at and at + QUEUE_BUCKET_NS <= b), default=None)
            r["queue_wait_overlap_max_ns"] = max((wait for at, wait in queue if a < at + QUEUE_BUCKET_NS and at < b), default=None)
        if profile:
            r["profile"] = profile | {"path": profile["path"].name}
        l = result or {}
        log(f"{trial_id} {side} {rate:.0f}/s pass={full_pass} p99={l.get('p99_ns', 0) / 1e6:.3f} ms "
            f"done={l.get('completed')}/{l.get('issued')} late={l.get('late')} rc={server.returncode}/{load.returncode}")
        return r

    def run_set(self, out, phase, cell_name, cell, group, items, profile=None):
        """Run one trial per side under the locks, rerunning a discarded hold."""
        while True:
            self.hold += 1
            receipt = dict(hold=self.hold, phase=phase, cell_name=cell_name, group=group,
                           order=[side for side, _, _ in items], runner_pid=os.getpid())
            records = []
            try:
                with timing_hold(receipt) as usable:
                    if usable:
                        with self.tool() as tool:
                            for side, rate, seed in items:
                                tag = f"{phase}-{cell_name}-{os.getpid()}-{self.hold}-{side.replace(' ', '_')}"
                                records.append(self.trial(tag, side, cell, rate, seed, tool, profile))
            finally:
                clean = bool(receipt.get("board_window_open") and receipt.get("cc1plus_start") == []
                             and receipt.get("cc1plus_end") == [])
                for r in records:
                    out.write(json.dumps(r | receipt | {"discarded": not clean}) + "\n")
                out.write(json.dumps(receipt | {"hold_summary": True, "discarded": not clean}) + "\n")
                out.flush()
            log(f"hold {self.hold} {cell_name} {receipt.get('hold_s')} s clean={clean}")
            time.sleep(HOLDS_GAP_S)
            if clean:
                return records

    def search(self, out, cell_name, cell, sides, groups, best):
        for group in groups:
            rng = random.Random(f"{cell_name}-{group}")
            seed = rng.randrange(1, 1 << 30)
            guesses = GUESSES[cell_name]
            gens = {s: search_steps((best.get(s) or guesses[s]) * 0.9) for s in sides}
            pending = {s: next(g) for s, g in gens.items()}
            misses = {s: 0 for s in sides}
            step = group - 1
            while pending:
                live = [s for s in sides if s in pending]
                offset = step % len(live)
                ordered = live[offset:] + live[:offset]
                records = self.run_set(out, "search", cell_name, cell, group, [(s, pending[s], seed) for s in ordered])
                step += 1
                for r in records:
                    side = r["side"]
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
    if os.sched_getaffinity(0) != {4, 5}:
        parser.error("pin this controller with taskset -c 4-5")
    if time.time() >= args.deadline:
        raise SystemExit("measurement deadline reached")
    signal.signal(signal.SIGALRM, stop_at_deadline)
    signal.setitimer(signal.ITIMER_REAL, args.deadline - time.time())
    runner = Runner(args.scratch.resolve(), args.data.resolve())
    configs = SCALE_CELLS | REC_CELLS
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
                    cell = SCALE_CELLS[f"A1-{args.sessions}"] | {"hot": hot}
                    sides = SCALE_SIDES if not hot else SCALE_SIDES[1:] + SCALE_SIDES[:1]
                    runner.run_set(out, "fairness", f"A2-{int(hot)}", cell, 1, [(s, rate[s] / 2, 732242) for s in sides if s in rate])
            elif args.command == "drain":
                for defer in (False, True):
                    cell = SCALE_CELLS["A1-8192"] | {"hot": True, "close_after_drain": defer}
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
