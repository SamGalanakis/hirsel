"""Run the object transfer matrix and write one JSON record per sample."""

import argparse
from contextlib import ExitStack, contextmanager
import datetime as dt
import fcntl
import json
import os
from pathlib import Path
import random
import re
import struct
import subprocess
import tempfile

BODY = 8 * 1024 * 1024
RUST = {
    "epoll": False, "ring": True, "uring": False, "uring-ring": True,
    "aws-lc": True, "ktls": True, "ktls-send": True,
}
LANG = {"hirsel-plain": False, "hirsel-tls": True}
MODES = RUST | LANG


def command(*args):
    return list(map(str, args))


def advance(process):
    process.stdin.write("x")
    process.stdin.flush()


@contextmanager
def counters(client, event, path):
    if event is None:
        yield client, (), None
        return
    with ExitStack() as stack:
        control_read, control_write = os.pipe()
        ack_read, ack_write = os.pipe()
        child_fds = (
            stack.enter_context(os.fdopen(control_read, "rb")),
            stack.enter_context(os.fdopen(ack_write, "wb")),
        )
        control = stack.enter_context(os.fdopen(control_write, "wb", buffering=0))
        stack.callback(os.close, ack_read)

        def toggle(action):
            control.write((action + "\n").encode())
            # perf writes a NUL after the newline. Consume the whole
            # acknowledgement so it cannot prefix the next one.
            if os.read(ack_read, 4096).rstrip(b"\0\n") != b"ack":
                raise RuntimeError(f"perf did not acknowledge {action}")

        yield (
            command("perf", "stat", "--delay=-1", "-x", "\t", "-e", event,
                    f"--control=fd:{control_read},{ack_write}", "-o", path, "--", *client),
            child_fds, toggle,
        )


def sample(client, event, work, instrument):
    perf_path = work / "perf.tsv"
    alloc_path = work / "allocations"
    alloc_path.unlink(missing_ok=True)
    env = os.environ.copy()
    if instrument:
        env["LD_PRELOAD"] = str(instrument)
        env["HTTP_BENCH_C_ALLOC"] = str(alloc_path)

    def c_allocations():
        return struct.unpack("=Q", alloc_path.read_bytes())[0]

    with counters(client, event, perf_path) as (argv, fds, toggle):
        with (work / "client.stderr").open("w+") as error, subprocess.Popen(
            argv, pass_fds=tuple(fd.fileno() for fd in fds), stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=error, text=True, env=env,
        ) as process:
            for fd in fds:
                fd.close()
            try:
                ready = process.stdout.readline()
                if ready == "READY\n":
                    before = c_allocations() if instrument else None
                    if toggle:
                        toggle("enable")
                    advance(process)
                    done = process.stdout.readline()
                    elapsed = re.fullmatch(r"DONE ([1-9][0-9]*)\n", done)
                    if elapsed:
                        if toggle:
                            toggle("disable")
                        allocations = c_allocations() - before if instrument else None
                        advance(process)
                    else:
                        allocations = None
                else:
                    done, elapsed, allocations = "", None, None
            finally:
                process.stdin.close()
            details = process.stdout.read()
            code = process.wait()
            error.seek(0)
            stderr = error.read()
    if code or elapsed is None:
        raise RuntimeError(f"{' '.join(client[:7])}: {code=}, {ready=}, {done=}, {stderr}")
    ring = re.fullmatch(r"SEND_ZC ([0-9]+) ([0-9]+)\n", details)
    if event:
        values = [line.split("\t") for line in perf_path.read_text().splitlines()]
        count, = [int(row[0]) for row in values if len(row) > 2 and row[2] == event and row[0].isdigit()]
    else:
        count = None
    if not instrument:
        marks = re.findall(r"^BENCH_ALLOCATIONS (READY|DONE) ([0-9]+)$", stderr, re.M)
        if [phase for phase, _ in marks] == ["READY", "DONE"]:
            allocations = int(marks[1][1]) - int(marks[0][1])
    return int(elapsed[1]), count, allocations, tuple(map(int, ring.groups())) if ring else (None, None)


def timing_window():
    now = dt.datetime.now().time()
    if dt.time(2, 45) <= now < dt.time(4):
        raise RuntimeError("local timing is closed from 02:45 to 04:00")


@contextmanager
def server(binary, cert, cpu, tls, measure):
    with subprocess.Popen(
        command("taskset", "-c", cpu, binary, "serve", "tls" if tls else "plain", cert, measure),
        stdout=subprocess.PIPE, text=True,
    ) as process:
        try:
            yield int(process.stdout.readline())
        finally:
            if process.poll() is None:
                name = subprocess.check_output(["ps", "-o", "comm=", "-p", str(process.pid)], text=True).strip()
                if name != binary.name[:15]:
                    raise RuntimeError(f"server PID {process.pid} is {name}, expected {binary.name}")
                process.terminate()


def run(args, work, results):
    cpu = command("taskset", "-c", args.client_cpu)
    modes = args.mode or [mode for mode in RUST if mode != "ktls"] + (list(LANG) if args.hirsel else [])
    selection = [(mode, method) for mode in modes for method in ("put", "get")]
    event = {"time": "cycles", "instructions": "instructions:u"}.get(args.stage)
    measure = "verify" if args.stage == "smoke" else "measure"
    pem = (args.cert / "cert.pem").read_text() if any(mode in LANG for mode in modes) else None
    randomizer = random.Random(1792)
    with ExitStack() as stack:
        if args.stage == "time" and not args.boat:
            for name in ("hirsel-mc-regimes.lock", "hirsel-board.lock"):
                lock = stack.enter_context((Path("/run/user") / str(os.getuid()) / name).open("a"))
                fcntl.flock(lock, fcntl.LOCK_EX)
            timing_window()
        ports = {
            tls: stack.enter_context(server(args.rust, args.cert, args.server_cpu, tls, measure))
            for tls in dict.fromkeys(MODES[mode] for mode in modes)
        }
        for repetition in range(args.samples):
            randomizer.shuffle(selection)
            for mode, method in selection:
                if args.stage == "time" and not args.boat:
                    timing_window()
                tls = MODES[mode]
                instrument = None
                if mode in LANG:
                    client = [*cpu, *command(args.hirsel, method, ports[tls], args.requests, measure)]
                    if tls:
                        client.append(pem)
                else:
                    client = [*cpu, *command(args.rust, "client", mode, method, ports[tls], args.cert, args.requests, measure)]
                    if args.stage == "allocations":
                        instrument = args.preload
                ns, count, alloc, (zc, copied) = sample(client, event, work, instrument)
                if args.stage == "allocations" and alloc is None:
                    raise RuntimeError(f"{mode} did not publish allocation counts")
                result = dict(mode=mode, method=method, run=repetition, requests=args.requests,
                              bytes=BODY * args.requests, ns=ns, event=event, event_count=count,
                              allocations=alloc, send_zc_bytes=zc, send_copied_bytes=copied)
                print(json.dumps(result), file=results, flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("stage", choices=["smoke", "time", "instructions", "allocations"])
    parser.add_argument("--rust", type=lambda path: Path(path).resolve(), required=True)
    parser.add_argument("--hirsel", type=lambda path: Path(path).resolve())
    parser.add_argument("--cert", type=lambda path: Path(path).resolve(), required=True)
    parser.add_argument("--preload", type=lambda path: Path(path).resolve())
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--mode", action="append", choices=MODES)
    parser.add_argument("--samples", type=int)
    parser.add_argument("--requests", type=int)
    parser.add_argument("--client-cpu", type=int)
    parser.add_argument("--server-cpu", type=int)
    parser.add_argument("--boat", action="store_true", help="use a Boat VM without the local host timing locks")
    args = parser.parse_args()
    timed = args.stage == "time"
    args.samples = args.samples if args.samples is not None else (60 if timed else 1)
    args.requests = args.requests if args.requests is not None else (1 if args.stage == "smoke" else 8)
    args.client_cpu = args.client_cpu if args.client_cpu is not None else (16 if timed else 8)
    args.server_cpu = args.server_cpu if args.server_cpu is not None else (18 if timed else 10)
    if min(args.samples, args.requests) < 1 or args.client_cpu == args.server_cpu:
        parser.error("the sample and request counts must be positive and CPUs distinct")
    if timed and not args.boat and not {args.client_cpu, args.server_cpu} <= set(range(16, 20)):
        parser.error("local timing requires CPUs 16-19")
    if args.mode and any(mode in LANG for mode in args.mode) and not args.hirsel:
        parser.error("Hirsel modes require --hirsel")
    if args.stage == "allocations" and not args.preload and (not args.mode or any(mode in RUST for mode in args.mode)):
        parser.error("Rust allocation counts require the glibc allocation preload")
    with tempfile.TemporaryDirectory(dir=args.output.resolve().parent, prefix="samples-") as work:
        with args.output.open("x") as results:
            run(args, Path(work), results)


if __name__ == "__main__":
    main()
