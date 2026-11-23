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
import socket
import struct
import subprocess
import tempfile

BODY = 8 * 1024 * 1024
PUT_START = b"PUT /object HTTP/1.1\r\nHost: localhost\r\nContent-Length: 8388608\r\n\r\n\xa5"
REACTORS = ("epoll", "io_uring")
RUST = {
    "epoll": False, "ring": True, "uring": False, "uring-ring": True,
    "aws-lc": True, "ktls": True, "ktls-send": True,
}
# Each Hirsel cell names its Net reactor; `auto` takes io_uring where the host allows it.
LANG = {"hirsel-plain": False, "hirsel-tls": True, "hirsel-plain-epoll": False, "hirsel-tls-epoll": True}
MODES = RUST | LANG


def command(*args):
    return list(map(str, args))


def advance(process):
    process.stdin.write("x")
    process.stdin.flush()


@contextmanager
def counters(client, event, path, record):
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

        operation = (
            ["record", "-N", "--strict-freq", "-F", "997", "--call-graph", "dwarf,8192"]
            if record else ["stat", "-x", "\t"]
        )
        yield (
            command("perf", *operation, "--delay=-1", "-e", event,
                    f"--control=fd:{control_read},{ack_write}", "-o", path, "--", *client),
            child_fds, toggle,
        )


def sample(cpu, client, event, work, instrument, profile=None):
    perf_path = profile or work / "perf.tsv"
    alloc_path = work / "allocations"
    alloc_path.unlink(missing_ok=True)
    preload = []
    if instrument:
        # taskset must exec before the allocation counter's constructor runs.
        preload = ["env", f"LD_PRELOAD={instrument}", f"HTTP_BENCH_C_ALLOC={alloc_path}"]

    def c_allocations():
        return struct.unpack("=Q", alloc_path.read_bytes())[0]

    with counters([*cpu, *preload, *client], event, perf_path, profile is not None) as (argv, fds, toggle):
        with (work / "client.stderr").open("w+") as error, subprocess.Popen(
            argv, pass_fds=tuple(fd.fileno() for fd in fds), stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=error, text=True,
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
        raise RuntimeError(f"{' '.join(client[:4])}: {code=}, {ready=}, {done=}, {stderr}")
    ring = re.fullmatch(r"SEND_ZC ([0-9]+) ([0-9]+)\n", details)
    if event and profile is None:
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


def lock_timing(stack):
    for name in ("hirsel-mc-regimes.lock", "hirsel-board.lock"):
        lock = stack.enter_context((Path("/run/user") / str(os.getuid()) / name).open("a"))
        fcntl.flock(lock, fcntl.LOCK_EX)
    timing_window()


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
            lock_timing(stack)
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
                    reactor = "epoll" if mode.endswith("-epoll") else "auto"
                    client = command("env", f"HIRSEL_NET_REACTOR={reactor}", args.hirsel, method,
                                     ports[tls], args.requests, measure)
                    if tls:
                        client.append(pem)
                else:
                    client = command(args.rust, "client", mode, method, ports[tls], args.cert, args.requests, measure)
                    if args.stage == "allocations":
                        instrument = args.preload
                ns, count, alloc, (zc, copied) = sample(cpu, client, event, work, instrument)
                if args.stage == "allocations" and alloc is None:
                    raise RuntimeError(f"{mode} did not publish allocation counts")
                result = dict(mode=mode, method=method, run=repetition, requests=args.requests,
                              bytes=BODY * args.requests, ns=ns, event=event, event_count=count,
                              allocations=alloc, send_zc_bytes=zc, send_copied_bytes=copied)
                print(json.dumps(result), file=results, flush=True)


def probe_client(args, reactor, method, port, count):
    return command("env", f"HIRSEL_NET_REACTOR={reactor}", args.hirsel, method, port, count)


def profile(args, work, results):
    selection = [(reactor, method) for reactor in args.reactor for method in ("put", "get")]
    randomizer = random.Random(1858)
    cpu = command("taskset", "-c", args.client_cpu)
    with server(args.rust, work, args.server_cpu, False, "measure") as port:
        with ExitStack() as stack:
            lock_timing(stack)
            for repetition in range(args.samples):
                randomizer.shuffle(selection)
                for reactor, method in selection:
                    timing_window()
                    path = args.output.with_suffix(f".{reactor}.{method}.{repetition}.data")
                    if path.exists():
                        raise FileExistsError(path)
                    client = probe_client(args, reactor, method, port, args.requests)
                    ns, _, _, _ = sample(cpu, client, "cycles", work, None, path)
                    print(json.dumps(dict(reactor=reactor, method=method, run=repetition,
                                          requests=args.requests, bytes=BODY * args.requests,
                                          ns=ns, profile=str(path.resolve()))), file=results, flush=True)


def acknowledge_put(peer):
    head = bytearray()
    while len(head) < len(PUT_START):
        chunk = peer.recv(len(PUT_START) - len(head))
        if not chunk:
            break
        head.extend(chunk)
    if head != PUT_START:
        raise RuntimeError(f"peer received {len(head)}/{len(PUT_START)} bytes before PUT acknowledgement")
    peer.sendall(b"x")
    # Leave the connection open without reading any more of the body.


def abort_trials(args, work, results):
    # The Python process is the peer. Pin it even when the caller uses a wider cpuset.
    os.sched_setaffinity(0, {args.server_cpu})
    with ExitStack() as stack:
        if not args.check:
            lock_timing(stack)
        for reactor in args.reactor:
            with socket.socket() as listener, (work / "abort.stderr").open("w+") as error:
                listener.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 4096)
                listener.bind(("127.0.0.1", 0))
                listener.listen()
                listener.settimeout(10)
                method = "check-abort" if args.check else "abort"
                client = probe_client(args, reactor, method, listener.getsockname()[1], args.samples)
                with subprocess.Popen(command("taskset", "-c", args.client_cpu, *client),
                                      stdout=subprocess.PIPE, stderr=error, text=True) as process:
                    try:
                        for trial in range(args.samples):
                            if not args.check:
                                timing_window()
                            peer, _ = listener.accept()
                            with peer:
                                peer.settimeout(10)
                                acknowledge_put(peer)
                                line = process.stdout.readline()
                                pattern = "ABORTED\n" if args.check else r"ABORT ([1-9][0-9]*)\n"
                                if not (elapsed := re.fullmatch(pattern, line)):
                                    raise RuntimeError(f"expected Aborted from {reactor}, got {line!r}")
                                row = dict(reactor=reactor, trial=trial, result="Aborted")
                                if not args.check:
                                    row["ns"] = int(elapsed[1])
                                print(json.dumps(row), file=results, flush=True)
                        if process.wait():
                            raise RuntimeError(f"{reactor} abort client exited {process.returncode}")
                    except (OSError, RuntimeError) as exc:
                        listener.close()
                        status = process.wait()
                        error.seek(0)
                        raise RuntimeError(f"{reactor} abort probe: {exc} (client exit {status})\n{error.read()}") from exc


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("stage", choices=["smoke", "time", "instructions", "allocations", "profile", "abort"])
    parser.add_argument("--rust", type=lambda path: Path(path).resolve())
    parser.add_argument("--hirsel", type=lambda path: Path(path).resolve())
    parser.add_argument("--cert", type=lambda path: Path(path).resolve())
    parser.add_argument("--preload", type=lambda path: Path(path).resolve())
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--mode", action="append", choices=MODES)
    parser.add_argument("--reactor", action="append", choices=REACTORS)
    parser.add_argument("--check", action="store_true", help="prove the abort scenario without publishing timings")
    parser.add_argument("--samples", type=int)
    parser.add_argument("--requests", type=int)
    parser.add_argument("--client-cpu", type=int)
    parser.add_argument("--server-cpu", type=int)
    parser.add_argument("--boat", action="store_true", help="use a Boat VM without the local host timing locks")
    args = parser.parse_args()
    probe = args.stage in ("profile", "abort")
    if args.check and args.stage != "abort":
        parser.error("--check is only available for the abort scenario")
    if args.stage == "abort" and args.requests is not None:
        parser.error("abort uses --samples to select its trial count")
    timed = args.stage == "time" or (probe and not args.check)
    default_samples = {"time": 60, "abort": 1 if args.check else 200, "profile": 4}
    default_requests = {"smoke": 1, "profile": 1024}
    args.samples = args.samples if args.samples is not None else default_samples.get(args.stage, 1)
    args.requests = args.requests if args.requests is not None else default_requests.get(args.stage, 8)
    args.client_cpu = args.client_cpu if args.client_cpu is not None else (16 if timed else 8)
    args.server_cpu = args.server_cpu if args.server_cpu is not None else (18 if timed else 10)
    if min(args.samples, args.requests) < 1 or args.client_cpu == args.server_cpu:
        parser.error("the sample and request counts must be positive and CPUs distinct")
    if timed and not args.boat and not {args.client_cpu, args.server_cpu} <= set(range(16, 20)):
        parser.error("local timing requires CPUs 16-19")
    if probe:
        if not args.hirsel or args.mode or args.boat:
            parser.error("profile and abort require --hirsel with the net-probe binary and local timing CPUs")
        args.reactor = args.reactor or REACTORS
    elif args.reactor or not args.cert:
        parser.error("the transfer matrix requires --cert and uses --mode to select reactors")
    if args.stage != "abort" and not args.rust:
        parser.error("this stage requires --rust")
    if args.mode and any(mode in LANG for mode in args.mode) and not args.hirsel:
        parser.error("Hirsel modes require --hirsel")
    if args.stage == "allocations" and not args.preload and (not args.mode or any(mode in RUST for mode in args.mode)):
        parser.error("Rust allocation counts require the glibc allocation preload")
    with tempfile.TemporaryDirectory(dir=args.output.resolve().parent, prefix="samples-") as work:
        with args.output.open("x") as results:
            stage = {"profile": profile, "abort": abort_trials}.get(args.stage, run)
            stage(args, Path(work), results)


if __name__ == "__main__":
    main()
