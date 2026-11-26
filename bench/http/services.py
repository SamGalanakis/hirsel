"""Sweep fixed offered rates and run the continuous HTTP service soaks."""

import argparse
from contextlib import contextmanager
import http.client
import json
import math
import os
from pathlib import Path
import shutil
from statistics import median
import struct
import subprocess
import sys
import tempfile
import time

from b1 import ALLOWED_CPUS, ready_line, stop

SERVERS = ("hirsel", "axum", "go")
ROUTES = ("plaintext", "json", "store")
SOAK_ROUTES = ("plaintext", "store")
LATENCIES = (("p50", "P50NS"), ("p99", "P99NS"), ("p99.9", "P999NS"), ("max", "MaxNS"))


class ServiceSet:
    def __init__(self, binaries, loader, hirsel_db, sampler, out, server_cpu, client_cpu, connections):
        self.binaries = binaries
        self.loader = loader
        self.hirsel_db = hirsel_db
        self.sampler = sampler
        self.out = out
        self.server_cpu = server_cpu
        self.client_cpu = client_cpu
        self.connections = connections

    @contextmanager
    def server(self, name, label, soak):
        with tempfile.TemporaryDirectory(prefix="service-", dir=self.hirsel_db.parent) as scratch:
            scratch = Path(scratch)
            env_command = ["taskset", "-c", str(self.server_cpu)]
            argv = [str(self.binaries[name])]
            if name == "hirsel":
                self.hirsel_db.mkdir()
            else:
                argv.append(str(scratch / "counter.sqlite"))
            c_count, go_count = scratch / "c-count", scratch / "go-count"
            if soak:
                env_command += ["env", f"LD_PRELOAD={self.sampler}", f"HTTP_BENCH_C_ALLOC={c_count}"]
                if name == "go":
                    env_command.append(f"HTTP_BENCH_GO_ALLOC={go_count}")
            with (self.out / f"{label}.stderr").open("wb") as errors:
                process = subprocess.Popen([*env_command, *argv], stdout=subprocess.PIPE, stderr=errors)
                try:
                    port = ready_line(process.stdout)
                    yield process, port, c_count, go_count
                finally:
                    stop(process)
                    process.stdout.close()
                    if name == "hirsel":
                        shutil.rmtree(self.hirsel_db)

    def probe(self, name):
        with self.server(name, f"{name}-probe", False) as (_, port, _, _):
            client = http.client.HTTPConnection("127.0.0.1", port, timeout=5)
            try:
                expected = [("plaintext", "text/plain", b"Hello, world"),
                            ("json", "application/json", b'{"message":"Hello, world"}'),
                            ("store", "text/plain", b"1"), ("store", "text/plain", b"2")]
                for route, content_type, body in expected:
                    client.request("GET", f"/{route}")
                    reply = client.getresponse()
                    observed = (reply.status, reply.getheader("Content-Type"), reply.read())
                    if observed != (200, content_type, body):
                        raise ValueError(f"{name} /{route} replied {observed!r}")
            finally:
                client.close()

    def trial(self, name, route, rate, seconds, label, soak=False):
        print(f"{name} {route} {label}", flush=True, file=sys.stderr)
        with self.server(name, label, soak) as (server, port, c_count, go_count):
            command = ["taskset", "-c", str(self.client_cpu), str(self.loader),
                       "--address", f"127.0.0.1:{port}", "--route", route, "--rate", str(rate),
                       "--seconds", str(seconds), "--connections", str(self.connections)]
            with (self.out / f"{label}.load.stderr").open("wb") as errors:
                load = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=errors, text=True)
                memory = []
                try:
                    if soak:
                        memory = soak_minutes(load, server, seconds, c_count, go_count)
                    stdout, _ = load.communicate()
                    if load.returncode or server.poll() is not None:
                        raise RuntimeError(f"{label} exited early, see its stderr logs")
                finally:
                    stop(load)
                    load.stdout.close()
            row = json.loads(stdout)
            row["Seconds"] = seconds
            row["Rate"] = rate
            row["Memory"] = memory
            (self.out / f"{label}.json").write_text(json.dumps(row) + "\n")
            return row

    def sweep(self, name, route, low_rate, max_rate, dwell, tail):
        def sweep_trial(rate):
            return self.trial(name, route, rate, dwell, f"{name}-{route}-sweep-{rate}")

        baseline = sweep_trial(low_rate)
        while not healthy(baseline) and low_rate > 1:
            low_rate //= 2
            baseline = sweep_trial(low_rate)
        if not healthy(baseline):
            raise RuntimeError(f"{name} {route} has no healthy low rate")
        low, high = low_rate, min(max_rate, low_rate * 2)
        knee = baseline
        while high > low:
            row = sweep_trial(high)
            if not healthy(row) or row["P99NS"] > 3 * baseline["P99NS"]:
                break
            low, knee = high, row
            high = min(max_rate, high * 2)
        if low == high:
            raise RuntimeError(f"{name} {route} knee exceeds the sweep range")
        while high > low * 1.10 and high - low > 1:
            rate = (low + high) // 2
            row = sweep_trial(rate)
            if healthy(row) and row["P99NS"] <= 3 * baseline["P99NS"]:
                low, knee = rate, row
            else:
                high = rate
        tails = {}
        for level in (50, 90):
            rate = max(1, low * level // 100)
            duration = max(tail, math.ceil(5000 / rate))
            row = self.trial(name, route, rate, duration, f"{name}-{route}-{level}pct")
            if not healthy(row):
                row["Censored"] = True
            tails[level] = row
        return {"knee": knee, "tails": tails}


def healthy(row):
    return (row["Offered"] > 0 and row["Completed"] == row["Offered"]
            and row["OnTime"] >= 0.99 * row["Offered"] and not row["Dropped"] and not row["Failed"])


def read_rss(process):
    values = {}
    for line in Path(f"/proc/{process.pid}/status").read_text().splitlines():
        key, _, value = line.partition(":")
        if key in ("VmHWM", "VmRSS"):
            values[key] = int(value.split()[0])
    return values["VmHWM"], values["VmRSS"]


def allocations(c_count, go_count, go):
    count, = struct.unpack("=Q", c_count.read_bytes())
    return count + (int(go_count.read_bytes()) if go else 0)


def soak_minutes(load, server, seconds, c_count, go_count):
    start = time.monotonic()
    while go_count.exists() and not go_count.stat().st_size:
        if time.monotonic() - start > 5:
            raise RuntimeError("Go allocation sampler did not publish its first count")
        time.sleep(0.05)
    go = go_count.exists()
    last_time = time.monotonic()
    last_count = allocations(c_count, go_count, go)
    samples = []
    for minute in range(1, seconds // 60 + 1):
        at = start + minute * 60
        while (delay := at - time.monotonic()) > 0:
            if load.poll() is not None:
                raise RuntimeError("load generator exited during the soak")
            time.sleep(min(delay, 5))
        now = time.monotonic()
        peak, current = read_rss(server)
        count = allocations(c_count, go_count, go)
        if count < last_count:
            raise RuntimeError("nonmonotonic allocation counter")
        samples.append({"peak": peak, "rss": current, "allocation_rate": (count - last_count) / (now - last_time)})
        last_time, last_count = now, count
    return samples


def relative(left, right):
    return f"{left / right:.2f}×" if right else "—"


def latency_table(rows):
    print("| Route | Offered load | Metric | Hirsel / Hyper+Axum | Hirsel / Go |")
    print("| --- | --- | --- | ---: | ---: |")
    for route in ROUTES:
        left, axum, go = (rows[name, route] for name in SERVERS)
        ratios = [relative(left["knee"]["OnTime"] / left["knee"]["Seconds"],
                           baseline["knee"]["OnTime"] / baseline["knee"]["Seconds"])
                  for baseline in (axum, go)]
        print(f"| {route} | Knee | Throughput | {' | '.join(ratios)} |")
        for level in (50, 90):
            for label, key in LATENCIES:
                data = [row["tails"][level] for row in (left, axum, go)]
                ratios = ["censored" if data[0].get("Censored") or base.get("Censored")
                          else relative(data[0][key], base[key]) for base in data[1:]]
                print(f"| {route} | {level}% of knee | {label} | {' | '.join(ratios)} |")


def soak_table(rows, out):
    print("| Route | Metric | Hirsel / Hyper+Axum | Hirsel / Go |")
    print("| --- | --- | ---: | ---: |")
    per_minute = ["route\tminute\tmetric\tHirsel/Axum\tHirsel/Go"]
    for route in SOAK_ROUTES:
        frames = [rows[name, route]["Memory"] for name in SERVERS]
        if not all(healthy(rows[name, route]) for name in SERVERS):
            print(f"| {route} | Soak completion | censored | censored |")
        for key, metric in (("peak", "Peak RSS"), ("rss", "Current RSS"),
                            ("allocation_rate", "Allocations per second")):
            for minute, (hirsel, axum, go) in enumerate(zip(*frames, strict=True), 1):
                per_minute.append("\t".join([route, str(minute), key,
                                             relative(hirsel[key], axum[key]),
                                             relative(hirsel[key], go[key])]))
            if key == "rss":
                continue
            aggregate = max if key == "peak" else median
            hirsel, axum, go = (aggregate(row[key] for row in samples) for samples in frames)
            print(f"| {route} | {metric} | {relative(hirsel, axum)} | {relative(hirsel, go)} |")
        for name, samples in zip(SERVERS, frames, strict=True):
            span = len(samples) // 3
            if span:
                growth = median(s["rss"] for s in samples[-span:]) / median(s["rss"] for s in samples[:span])
                print(f"\n{route} {name} resident memory last / first third {growth:.2f}×"
                      + (" (growth)" if growth > 1.05 else ""))
    (out / "soak-ratios.tsv").write_text("\n".join(per_minute) + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--artifacts", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--server-cpu", type=int, default=0)
    parser.add_argument("--client-cpu", type=int, default=1)
    parser.add_argument("--connections", type=int, default=64)
    parser.add_argument("--dwell", type=int, default=8)
    parser.add_argument("--tail-seconds", type=int, default=30)
    parser.add_argument("--minimum-rate", type=int, default=128)
    parser.add_argument("--maximum-rate", type=int, default=131072)
    parser.add_argument("--soak-seconds", type=int, default=7200)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    pair = {args.server_cpu, args.client_cpu}
    if len(pair) != 2 or not pair <= (ALLOWED_CPUS & os.sched_getaffinity(0)):
        parser.error("select two distinct inherited CPUs within 0-11 or 16-27")
    if not 0 <= args.soak_seconds <= 7200 or args.soak_seconds % 60:
        parser.error("soak must be zero or a whole number of minutes up to two hours")
    artifacts = args.artifacts.resolve()
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=True)
    service = ServiceSet({name: artifacts / name for name in SERVERS}, artifacts / "load",
                         artifacts / "hirsel-db", artifacts / "allocation_count.so", out,
                         args.server_cpu, args.client_cpu, args.connections)
    for name in SERVERS:
        service.probe(name)
    if args.check:
        return
    knees = {(name, route): service.sweep(name, route, args.minimum_rate, args.maximum_rate,
                                         args.dwell, args.tail_seconds)
             for route in ROUTES for name in SERVERS}
    (out / "knees.json").write_text(json.dumps({f"{name}/{route}": data
                                              for (name, route), data in knees.items()}, indent=2) + "\n")
    latency_table(knees)
    if args.soak_seconds:
        soaks = {}
        for route in SOAK_ROUTES:
            for name in SERVERS:
                rate = max(1, knees[name, route]["knee"]["Rate"] * 70 // 100)
                soaks[name, route] = service.trial(name, route, rate, args.soak_seconds,
                                                 f"{name}-{route}-soak", soak=True)
        print()
        soak_table(soaks, out)


if __name__ == "__main__":
    main()
