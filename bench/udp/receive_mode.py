"""Profile or measure UDP readiness receive inside the timing shield."""
import argparse
import csv
import json
import os
from pathlib import Path
import select
import socket
import struct
import subprocess
import sys
import time

SHAPES = {"s1": 1_000_000, "s2": 100_000, "group": 100_000}
EVENTS = (
    "instructions:u",
    "cycles:u",
    "raw_syscalls:sys_enter",
    "syscalls:sys_enter_recvmmsg",
    "syscalls:sys_enter_io_uring_enter",
)


def ready(process):
    if not select.select([process.stdout], [], [], 5)[0]:
        raise RuntimeError(f"process {process.pid} did not get ready")
    if process.stdout.readline().strip() != "READY":
        raise RuntimeError(f"process {process.pid} failed before READY ({process.poll()})")


def result(process):
    output, stderr = process.communicate(timeout=10)
    if process.returncode:
        raise RuntimeError(f"process {process.pid} failed: {stderr}\n{output}")
    return output.splitlines(), stderr


def clock_schedule(process, absolute):
    deadline = time.monotonic() + 5
    descriptor = process.stderr.fileno()
    preceding = bytearray()
    line = bytearray()
    while select.select([descriptor], [], [], max(0, deadline - time.monotonic()))[0]:
        byte = os.read(descriptor, 1)
        if not byte:
            break
        line.extend(byte)
        if byte == b"\n":
            if line.startswith(b"UDP_CLOCK_EPOCH "):
                break
            preceding.extend(line)
            line.clear()
    else:
        line.clear()
    if not line.startswith(b"UDP_CLOCK_EPOCH "):
        raise RuntimeError(f"process {process.pid} did not publish its clock epoch: "
                           f"{(preceding + line).decode(errors='replace')}")
    epoch = int(line.split()[1])
    process.stdin.buffer.write(struct.pack("<QQ", epoch, absolute - epoch))
    process.stdin.flush()


def child(cpus, *command, **kwargs):
    return subprocess.Popen(["taskset", "-c", cpus, *map(str, command)], **kwargs)


def port_number():
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def stats(log, group):
    counts = {}
    workers = {}
    for row in csv.reader(log.read_text().splitlines(), delimiter=","):
        if group:
            if not row or not row[0].startswith("hirsel-w"):
                continue
            worker, *row = row
            worker = worker.rsplit("-", 1)[0]
        if len(row) >= 5 and row[2] in EVENTS:
            if float(row[4]) < 99:
                raise RuntimeError(f"counter multiplexed or disabled: {row}")
            value = int(row[0])
            counts[row[2]] = counts.get(row[2], 0) + value
            if group:
                workers.setdefault(worker, {})[row[2]] = value
    if set(counts) != set(EVENTS):
        raise RuntimeError(f"missing counters in {log}")
    if group and (set(workers) != {"hirsel-w0", "hirsel-w1", "hirsel-w2"}
                  or any(set(counter) != set(EVENTS) for counter in workers.values())):
        raise RuntimeError(f"missing group worker counters in {log}")
    return counts, workers


def sample(args, shape, label, profile):
    rate = SHAPES[shape]
    port = port_number()
    start = time.monotonic_ns() + 300_000_000
    end = start + 1_000_000_000
    stop = end + 100_000_000
    env = os.environ | {
        "HIRSEL_NET_REACTOR": "io_uring",
        "HIRSEL_UDP_MEASURE_CLOCK": "1",
        "HIRSEL_UDP_MEASURE_GROUP": "1" if shape == "group" else "0",
    }
    env.pop("HIRSEL_UNTIED", None)
    rx_env = env | {"HIRSEL_UNTIED": "3"} if shape == "group" else env
    rx_cpu = "17-19" if shape == "group" else "17"
    processes = []
    pipes = {"stdout": subprocess.PIPE, "stderr": subprocess.PIPE, "text": True}
    perf_path = args.output / (label + (".perf" if profile else ".csv"))
    try:
        rx = child(rx_cpu, args.load, shape, port, env=rx_env, stdin=subprocess.PIPE, **pipes)
        processes.append(rx)
        clock_schedule(rx, stop)
        ready(rx)
        tx_command = (args.flows, "s1" if shape == "s1" else "s2", port, start, rate)
        tx = child("16", *tx_command, env=env, stdin=subprocess.PIPE, **pipes)
        processes.append(tx)
        ready(tx)
        if profile:
            perf = child("19", "perf", "record", "-N", "-e", "cycles:u", "-g",
                         "-o", perf_path, "-p", rx.pid, **pipes)
        else:
            per_thread = ["--per-thread"] if shape == "group" else []
            perf = child("19", "perf", "stat", *per_thread, "-x,", "-e", ",".join(EVENTS),
                         "-o", perf_path, "-p", rx.pid, **pipes)
        processes.append(perf)
        if time.monotonic_ns() >= start:
            raise RuntimeError("setup missed the first scheduled packet")
        try:
            sent_rows, _ = result(tx)
        except RuntimeError as failure:
            if rx.poll() is not None:
                receive_rows, diagnostic = rx.communicate()
                raise RuntimeError(f"{failure}\nreceiver: {receive_rows}\n{diagnostic}") from failure
            raise
        receive_rows, diagnostic = result(rx)
        result(perf)
        sent, elapsed = map(int, sent_rows[-1].split()[1:])
        offered_valid = sent == rate and 990_000_000 <= elapsed <= 1_010_000_000
        if not profile and not offered_valid:
            raise RuntimeError(f"offered rate invalid: {sent} packets in {elapsed} ns")
        samples = [list(map(int, row.split()[1:])) for row in receive_rows if row.startswith("RECV ")]
        if len(samples) != (9 if shape == "group" else 1):
            raise RuntimeError(f"missing receive results: {receive_rows}")
        delivered, p99, p999 = samples[-1]
        if not 0 < delivered <= sent:
            raise RuntimeError(f"invalid delivery count {delivered}")
        row = dict(shape=shape, sent=sent, delivered=delivered,
                   p99_ns=p99, p999_ns=p999, tx_ns=elapsed, offered_valid=offered_valid)
        if shape == "group":
            row["members"] = samples[:-1]
            readings = [
                [int(value) for value in line.split()[1:]]
                for line in diagnostic.splitlines()
                if line.startswith("UDP_GROUP ")
            ]
            if len(readings) != 1:
                raise RuntimeError("missing group counters and intake-to-delivery latency")
            names = ("delivered", "full", "dropped", "capture_discard", "overflow",
                     "p99_ns", "p999_ns", "home", "away", "mixed")
            intake = dict(zip(names, readings[0], strict=True))
            if intake["overflow"] or intake["delivered"] != delivered:
                raise RuntimeError(f"invalid intake distribution: {intake}")
            if not profile and (intake["home"], intake["away"], intake["mixed"]) != (4, 4, 0):
                raise RuntimeError(f"incorrect group placement: {intake}")
            row["intake"] = intake
            delivered_or_discarded = delivered + sum(intake[key] for key in (
                "full", "dropped", "capture_discard"
            ))
            if delivered_or_discarded > sent:
                raise RuntimeError(f"more accounted datagrams than sent: {intake}")
        else:
            delivered_or_discarded = delivered
        row["kernel_or_unread"] = sent - delivered_or_discarded
        if not profile:
            row["counters"], workers = stats(perf_path, shape == "group")
            if workers:
                row["workers"] = workers
            if row["counters"]["syscalls:sys_enter_recvmmsg"] == 0:
                raise RuntimeError("receiver made no recvmmsg calls")
        return row
    finally:
        for process in processes:
            if process.poll() is None:
                process.terminate()
                process.wait(timeout=5)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("load", type=Path)
    parser.add_argument("flows", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--phase", choices=("profile", "measure"), required=True)
    parser.add_argument("--resume", action="store_true", help="Continue after the last saved sample")
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    if args.phase == "measure":
        profile = args.output / "profile.jsonl"
        if not profile.exists() or len(profile.read_text().splitlines()) != len(SHAPES):
            parser.error("profile each workload before measuring")
    path = args.output / (args.phase + ".jsonl")
    plan = [(shape, run) for shape in SHAPES
            for run in range(1 if args.phase == "profile" else 60)]
    rows = [json.loads(line) for line in path.read_text().splitlines()] if args.resume else []
    if len(rows) > len(plan) or any(
        row["shape"] != shape
        for row, (shape, _) in zip(rows, plan)
    ):
        parser.error(f"{path} does not match the sample sequence")
    with path.open("a" if args.resume else "x") as stream:
        for shape, run in plan[len(rows):]:
            label = f"{args.phase}-{shape}-{run:02}"
            row = sample(args, shape, label, args.phase == "profile")
            rows.append(row)
            print(json.dumps(row), file=stream, flush=True)


if __name__ == "__main__":
    main()
