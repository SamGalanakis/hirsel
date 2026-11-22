"""Server CPU admission and task ownership for the fair benchmark."""
from collections import Counter
from contextlib import ExitStack, contextmanager
from datetime import datetime, timedelta
from itertools import count
import os
from pathlib import Path
import re
import select
import signal
import subprocess
import time

from receipt import measurement_slot

MEASUREMENT_SECONDS = 240
IDLE_LIMIT = .05
TRACE_IDS = count(1)
FORK = re.compile(r"sched:sched_process_fork:.*?\bpid=(\d+).*?\bchild_pid=(\d+)")
SWITCH = re.compile(
    r"\[(\d+)\]\s+(\d+)\.(\d+):\s+sched:sched_switch:\s+(.*?):(\d+)\s+\[\d+\]\s+\S+"
    r"\s+==>\s+(.*?):(\d+)\s+\[\d+\]"
)


def cpu_ticks(cpus):
    wanted = set(cpus)
    rows = {}
    for line in Path("/proc/stat").read_text().splitlines():
        name, *values = line.split()
        if not name.startswith("cpu") or not name[3:].isdigit() or int(name[3:]) not in wanted:
            continue
        user, nice, system, idle, iowait, irq, softirq, steal, *_ = map(int, values)
        busy = user + nice + system + irq + softirq + steal
        rows[int(name[3:])] = (busy, busy + idle + iowait)
    if rows.keys() != wanted:
        raise RuntimeError("server CPUs missing from /proc/stat")
    return rows


def idle_sample(cpus):
    start = cpu_ticks(cpus)
    time.sleep(1)
    end = cpu_ticks(cpus)
    busy = {}
    for cpu in cpus:
        busy_ticks = end[cpu][0] - start[cpu][0]
        total_ticks = end[cpu][1] - start[cpu][1]
        busy[cpu] = busy_ticks / total_ticks if total_ticks else 1
    return busy


def board_window_open():
    current = datetime.now().astimezone()
    start = current.replace(hour=2, minute=45, second=0, microsecond=0)
    end = start.replace(hour=4, minute=0)
    if current >= end:
        start += timedelta(days=1)
    return (start - current).total_seconds() >= MEASUREMENT_SECONDS


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
                 "--clockid", "mono", "--delay=-1", "--control", f"fd:{ctl_r},{ack_w}",
                 "-e", "sched:sched_switch,sched:sched_process_fork", "-o", path],
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


def classify_tasks(path, server, roots, start, end, busy_ticks):
    output = subprocess.check_output(
        ["perf", "script", "--ns", "--show-lost-events", "-i", path,
         "-F", "trace:cpu,time,event,trace"], text=True)
    owned = set(roots)
    successors = []
    seen = Counter()
    running = {}
    lost = 0
    for line in output.splitlines():
        if "PERF_RECORD_LOST" in line or "LOST" in line:
            lost += 1
        if matched := FORK.search(line):
            successors.append(tuple(map(int, matched.groups())))
        elif matched := SWITCH.search(line):
            cpu, seconds, fraction, previous, previous_pid, next_name, next_pid = matched.groups()
            cpu = int(cpu)
            if cpu not in server:
                continue
            timestamp = int(seconds) * 1_000_000_000 + int(fraction.ljust(9, "0"))
            last, _, _ = running.get(cpu, (start, previous_pid, previous))
            if min(timestamp, end) > max(start, last):
                seen[cpu, int(previous_pid), previous] += 1
            running[cpu] = timestamp, int(next_pid), next_name
    for cpu, (timestamp, pid, command) in running.items():
        if end > max(start, timestamp):
            seen[cpu, pid, command] += 1
    # perf sorts records by their common monotonic timestamps.
    for parent, child in successors:
        if parent in owned:
            owned.add(child)
    own = sum(samples for (_, tid, _), samples in seen.items() if tid in owned)
    foreign = [{"cpu": cpu, "pid": tid, "command": command, "intervals": samples}
               for (cpu, tid, command), samples in sorted(seen.items()) if tid and tid not in owned]
    unobserved_busy = [cpu for cpu in server if cpu not in running and busy_ticks[cpu]]
    return {"owned_intervals": own, "foreign_tasks": foreign, "lost_notices": lost,
            "unobserved_busy_cpus": unobserved_busy}


@contextmanager
def hold(receipt, *, cpu_sets, output, lock_paths, gap, compiler_processes):
    server = cpu_sets["server"]
    trace_cpus = set().union(*cpu_sets.values())
    while True:
        if board_window_open():
            busy = idle_sample(server)
            if max(busy.values()) < IDLE_LIMIT:
                locks = ExitStack()
                try:
                    for path in lock_paths:
                        locks.enter_context(measurement_slot(Path(path), blocking=False))
                except BlockingIOError:
                    locks.close()
                else:
                    if board_window_open():
                        break
                    locks.close()
            else:
                print(f"Waiting for idle server CPUs, max busy {max(busy.values()):.1%}, "
                      f"one-minute load {os.getloadavg()[0]:.2f}", flush=True)
        time.sleep(gap)

    trace = output / f"schedule-{next(TRACE_IDS)}.perf.data"
    with locks:
        started = time.monotonic()
        receipt.update(started=datetime.now().astimezone().isoformat(), arm_pids=[],
                       cpu_trace=trace.name, idle_cpu_busy=busy, board_window_open=True,
                       load_start=os.getloadavg()[0], cc1plus_start=compiler_processes())
        try:
            with record_tasks(trace, trace_cpus):
                before = cpu_ticks(server)
                receipt["timed_start_ns"] = time.monotonic_ns()
                try:
                    yield True
                finally:
                    receipt["timed_end_ns"] = time.monotonic_ns()
                    after = cpu_ticks(server)
                    receipt["server_busy_ticks"] = {cpu: after[cpu][0] - before[cpu][0] for cpu in server}
        finally:
            receipt.update(hold_s=time.monotonic() - started, load_end=os.getloadavg()[0],
                           cc1plus_end=compiler_processes())
    receipt.update(classify_tasks(trace, server, receipt["arm_pids"],
                                  receipt["timed_start_ns"], receipt["timed_end_ns"],
                                  receipt["server_busy_ticks"]))
    receipt["clean"] = (receipt["hold_s"] < MEASUREMENT_SECONDS
                        and receipt["owned_intervals"] > 0 and not receipt["unobserved_busy_cpus"]
                        and not receipt["foreign_tasks"] and not receipt["lost_notices"])
