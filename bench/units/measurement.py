"""Server CPU admission and task ownership for the fair benchmark."""
from collections import Counter
from contextlib import ExitStack, contextmanager
from datetime import datetime, timedelta, timezone
from itertools import count
import os
from pathlib import Path
import re
import select
import signal
import subprocess
import time

from timing_shield import hold as timing_hold
from bench.units.contract import (
    FOREIGN_SHARE_LIMIT, IDLE_LIMIT, MEASUREMENT_SECONDS, admits_arm, admits_hold, timing_window_open,
)

TRACE_IDS = count(1)
SWITCH = re.compile(
    r"^\s*(-?\d+)/(-?\d+)\s+\[(\d+)\]\s+(\d+)\.(\d+):\s+sched:sched_switch:\s+.*?:(\d+)\s+\[\d+\]\s+(\S+)"
    r"\s+==>\s+.*?:(\d+)\s+\[\d+\]"
)


def cpu_runtime_ns(cpus):
    wanted = set(cpus)
    rows = {}
    for line in Path("/proc/schedstat").read_text().splitlines():
        name, *values = line.split()
        if name.startswith("cpu") and name[3:].isdigit() and int(name[3:]) in wanted:
            # The seventh CPU field is cumulative task runtime in nanoseconds.
            rows[int(name[3:])] = int(values[6])
    if rows.keys() != wanted:
        raise RuntimeError("measurement CPUs missing from /proc/schedstat")
    return rows


def idle_sample(cpus):
    start = cpu_runtime_ns(cpus)
    started = time.monotonic_ns()
    time.sleep(1)
    end = cpu_runtime_ns(cpus)
    elapsed = time.monotonic_ns() - started
    return {cpu: (end[cpu] - start[cpu]) / elapsed for cpu in cpus}


def board_window_open():
    current = datetime.now(timezone.utc)
    return timing_window_open(current, current + timedelta(seconds=MEASUREMENT_SECONDS))


def check_ack(fd):
    ready, _, _ = select.select([fd], [], [], 5)
    if not ready or os.read(fd, 128).strip(b"\0\r\n ") != b"ack":
        raise RuntimeError("scheduler trace did not acknowledge recording")


@contextmanager
def record_tasks(path, cpus):
    ctl_r, ctl_w = os.pipe()
    ack_r, ack_w = os.pipe()
    with path.with_suffix(".log").open("wb") as log:
        try:
            child = subprocess.Popen(
                ["perf", "record", "-N", "-a", "-C", ",".join(map(str, sorted(set(cpus)))),
                 "--clockid", "mono", "-m", "1024", "--delay=-1", "--control", f"fd:{ctl_r},{ack_w}",
                 "-e", "sched:sched_switch", "-o", path],
                pass_fds=(ctl_r, ack_w), stdout=log, stderr=log,
            )
            os.close(ctl_r)
            os.close(ack_w)
            ctl_r = ack_w = None
            try:
                os.write(ctl_w, b"enable\n")
                check_ack(ack_r)
                yield
            finally:
                if child.poll() is None:
                    try:
                        os.write(ctl_w, b"stop\n")
                    except BrokenPipeError:
                        pass
                try:
                    status = child.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    name = subprocess.check_output(
                        ["ps", "-o", "comm=", "-p", str(child.pid)], text=True).strip()
                    if name != "perf":
                        raise RuntimeError(f"unexpected scheduler trace command {name!r}")
                    child.send_signal(signal.SIGINT)
                    status = child.wait(timeout=5)
                if status:
                    raise RuntimeError(f"scheduler trace exited {status}, see {path.with_suffix('.log')}")
        finally:
            for fd in (ctl_r, ctl_w, ack_r, ack_w):
                if fd is not None:
                    os.close(fd)


def classify_tasks(path, cpu_sets, windows, start, end, busy_ns):
    """Charge each sched-switch interval to the arm and CPU set it overlaps."""
    cpus = set(cpu_sets["server"]) | set(cpu_sets["load"])
    seen = [Counter() for _ in windows]
    running = {}
    gaps = set()
    lost = 0

    def account(cpu, tgid, left, right):
        for arm, intervals in zip(windows, seen, strict=True):
            elapsed = min(right, arm["end_ns"]) - max(left, arm["start_ns"])
            if elapsed > 0:
                intervals[cpu, tgid] += elapsed

    command = ["perf", "script", "--ns", "--show-lost-events", "-i", path,
               "-F", "trace:pid,tid,cpu,time,event,trace"]
    with subprocess.Popen(command, stdout=subprocess.PIPE, text=True) as reader:
        for line in reader.stdout:
            if "PERF_RECORD_LOST" in line or line.lstrip().startswith("LOST"):
                lost += 1
            if matched := SWITCH.match(line):
                tgid, tid, cpu, seconds, fraction, previous, state, following = matched.groups()
                tgid, tid, cpu, previous, following = map(int, (tgid, tid, cpu, previous, following))
                if cpu not in cpus:
                    continue
                timestamp = int(seconds) * 1_000_000_000 + int(fraction.ljust(9, "0"))
                last, expected = running.get(cpu, (start, previous))
                if expected != previous or (cpu in running and timestamp < last):
                    gaps.add(cpu)
                # The event was emitted by the outgoing thread before the context switch.
                # perf script can drop the TID on exit while keeping the sample's TGID.
                has_owner = tgid > 0 and (tid == previous or (tid == -1 and state in ("X", "Z")))
                if previous:
                    account(cpu, tgid if has_owner else None, last, timestamp)
                running[cpu] = timestamp, following
            elif "sched:sched_switch:" in line:
                lost += 1
        if reader.wait():
            raise RuntimeError(f"could not decode scheduler trace {path}")

    for cpu, (timestamp, tid) in running.items():
        if tid:
            # Without a switch-out there is no verified TGID for this interval.
            account(cpu, None, timestamp, end)
    unobserved = sorted(cpu for cpu in cpus if cpu not in running and busy_ns.get(cpu, 0))
    arms = {}
    for arm, intervals in zip(windows, seen, strict=True):
        owned = set(arm["pids"])
        duration = arm["end_ns"] - arm["start_ns"]
        foreign = Counter()
        local = Counter()
        for (cpu, tgid), elapsed in intervals.items():
            (local if tgid in owned else foreign)[cpu] += elapsed
        normalized = {
            role: sum(foreign[cpu] for cpu in cpu_sets[role]) / (duration * len(cpu_sets[role]))
            if duration > 0 and cpu_sets[role] else None
            for role in ("server", "load")
        }
        complete = (start <= arm["start_ns"] < arm["end_ns"] <= end
                    and not lost and not gaps and not unobserved and sum(local.values()) > 0)
        result = {
            "start_ns": arm["start_ns"], "end_ns": arm["end_ns"], "duration_ns": duration,
            "pids": arm["pids"], "owned_cpu_ns": dict(local),
            "foreign_cpu_ns": {cpu: foreign[cpu] for cpu in sorted(cpus)},
            "foreign_share": normalized, "trace_complete": complete,
        }
        arms[arm["side"]] = result | {"admitted": admits_arm(result)}
    return {"arm_cpu": arms, "lost_notices": lost, "trace_gap_cpus": sorted(gaps),
            "unobserved_busy_cpus": unobserved}


@contextmanager
def hold(receipt, *, cpu_sets, output, lock_paths, compiler_processes):
    observed = sorted(set(cpu_sets["server"]) | set(cpu_sets["load"]))
    while True:
        if board_window_open():
            busy = idle_sample(observed)
            if max(busy.values()) < IDLE_LIMIT:
                locks = ExitStack()
                try:
                    locks.enter_context(timing_hold(*lock_paths, blocking=False))
                except BlockingIOError:
                    locks.close()
                except BaseException:
                    locks.close()
                    raise
                else:
                    if board_window_open():
                        break
                    locks.close()
            else:
                print(f"Waiting for idle server and load CPUs, max busy {max(busy.values()):.1%}, "
                      f"one-minute load {os.getloadavg()[0]:.2f}", flush=True)
        time.sleep(1)

    trace = output / f"schedule-{next(TRACE_IDS)}.perf.data"
    with locks:
        started = time.monotonic()
        receipt.update(started=datetime.now(timezone.utc).isoformat(), arm_pids=[], arm_windows=[],
                       foreign_share_limit=FOREIGN_SHARE_LIMIT, clean=False, cpu_trace=trace.name, idle_cpu_busy=busy, board_window_open=True,
                       load_start=os.getloadavg()[0], cc1plus_start=compiler_processes())
        try:
            with record_tasks(trace, observed):
                before = cpu_runtime_ns(observed)
                receipt["timed_start_ns"] = time.monotonic_ns()
                try:
                    yield True
                finally:
                    receipt["timed_end_ns"] = time.monotonic_ns()
                    after = cpu_runtime_ns(observed)
                    receipt["busy_ns"] = {cpu: after[cpu] - before[cpu] for cpu in observed}
        finally:
            receipt.update(hold_s=time.monotonic() - started, load_end=os.getloadavg()[0],
                           finished=datetime.now(timezone.utc).isoformat(),
                           cc1plus_end=compiler_processes())
    receipt.update(classify_tasks(trace, cpu_sets, receipt["arm_windows"],
                                  receipt["timed_start_ns"], receipt["timed_end_ns"], receipt["busy_ns"]))
    receipt["clean"] = admits_hold(receipt)
