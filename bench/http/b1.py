"""Instruction attribution for the serial B1 HTTP/1.1 workload."""

import email.utils
import fcntl
import os
from pathlib import Path
import re
import select
import socket
import subprocess
import sys
import tempfile

REQUEST = b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: keep-alive\r\n\r\n"
BODY = b"Hello, world"
ALLOWED_CPUS = set(range(12)) | set(range(16, 28))
PERIOD = 131071
LOCK = "/tmp/hirsel-langarena-measure.lock"


def ready_line(stream):
    if not select.select([stream], [], [], 30)[0]:
        raise TimeoutError("server did not announce its listener")
    line = stream.readline().strip()
    match = re.fullmatch(rb"LISTEN 127\.0\.0\.1:(\d+)", line)
    if not match:
        raise ValueError(f"unexpected listener announcement: {line!r}")
    return int(match[1])


def verify_reply(stream, name):
    assert stream.readline(128) == b"HTTP/1.1 200 OK\r\n"
    if name == "tokio":
        assert stream.readline(128) == b"content-type: text/plain\r\n"
        assert stream.readline(128) == b"content-length: 12\r\n"
    else:
        assert stream.readline(128) == b"Content-Type: text/plain\r\n"
        assert stream.readline(128) == b"Content-Length: 12\r\n"
        line = stream.readline(128)
        assert line.startswith(b"Date: ") and line.endswith(b" GMT\r\n"), line
        date = email.utils.parsedate_to_datetime(line[6:-2].decode("ascii"))
        assert email.utils.format_datetime(date, usegmt=True).encode("ascii") == line[6:-2]
    assert stream.readline(128) == b"\r\n"
    assert stream.read(len(BODY)) == BODY


def control(writer, reader, command):
    writer.write(command + b"\n")
    if not select.select([reader], [], [], 30)[0] or reader.readline().lstrip(b"\0") != b"ack\n":
        raise RuntimeError(f"perf failed to acknowledge {command!r}")


def stop(process):
    for signal_child in (process.terminate, process.kill):
        if process.poll() is not None:
            return
        name = subprocess.run(["ps", "-o", "comm=", "-p", str(process.pid)], check=False,
                              capture_output=True, text=True).stdout.strip()
        if name in {"systemd", "dbus-daemon", "sshd", "tmux", "t3", "orca-ide",
                    "claude", "codex"}:
            raise RuntimeError(f"refusing to signal {name}")
        if name:
            signal_child()
        try:
            process.wait(timeout=5)
            return
        except subprocess.TimeoutExpired:
            continue
    raise RuntimeError("owned process did not exit")


def exchange(name, command, perf, cpu, requests, warmup):
    ctl_read, ctl_write = os.pipe()
    ack_read, ack_write = os.pipe()
    with (os.fdopen(ctl_write, "wb", buffering=0) as writer,
          os.fdopen(ack_read, "rb", buffering=0) as reader,
          open(LOCK, "a") as lock,
          tempfile.TemporaryFile() as errors):
        fcntl.flock(lock, fcntl.LOCK_EX)
        try:
            process = subprocess.Popen(
                ["taskset", "-c", str(cpu), "perf", *perf,
                 "--delay=-1", "--control", f"fd:{ctl_read},{ack_write}",
                 "--", *command, "--requests", str(warmup + requests), "--port", "0"],
                pass_fds=(ctl_read, ack_write), stdout=subprocess.PIPE, stderr=errors,
            )
        finally:
            os.close(ctl_read)
            os.close(ack_write)
        try:
            port = ready_line(process.stdout)
            with socket.create_connection(("127.0.0.1", port), timeout=30,
                                          source_address=("127.0.0.2", 0)) as client:
                client.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
                with client.makefile("rb") as replies:
                    for _ in range(warmup):
                        client.sendall(REQUEST)
                        verify_reply(replies, name)
                    control(writer, reader, b"enable")
                    for _ in range(requests):
                        client.sendall(REQUEST)
                        verify_reply(replies, name)
                    control(writer, reader, b"disable")
                    client.shutdown(socket.SHUT_WR)
                    assert replies.read(1) == b""
            output, _ = process.communicate(timeout=30)
            expected = f"SERVED {warmup + requests}".encode()
            if process.returncode or output.strip() != expected:
                raise RuntimeError(f"{name} exited {process.returncode}: {output!r}")
            errors.seek(0)
            diagnostic = errors.read().decode("utf-8", errors="replace")
            if re.search(r"\blost (?:event|samples)|lost \d+ (?:event|samples)", diagnostic, re.I):
                raise RuntimeError(diagnostic)
        except BaseException:
            stop(process)
            errors.seek(0)
            print(errors.read().decode("utf-8", errors="replace"), file=sys.stderr)
            raise
        finally:
            process.stdout.close()


def measure(name, command, server_cpu, requests, directory):
    stat_path = directory / f"{name}.stat"
    events = ("instructions:u", "cycles")
    exchange(name, command, ["stat", "-x", ",", "-o", str(stat_path), "-e", ",".join(events)],
             server_cpu, requests, warmup=1024)
    counters = {}
    for line in stat_path.read_text().splitlines():
        row = line.split(",")
        if len(row) >= 5 and row[2] in events:
            if float(row[4]) < 99.5:
                raise RuntimeError(f"{row[2]} ran for only {row[4]}% of the sample")
            counters[row[2]] = int(row[0]) / requests
    if counters.keys() != set(events):
        raise RuntimeError(f"missing B1 counters in {stat_path}")
    return counters["instructions:u"], counters["cycles"]


def profile(name, command, server_cpu, requests, directory):
    record_path = directory / f"{name}.perf"
    exchange(name, command,
             ["record", "-N", "-o", str(record_path), "-e", "instructions:u",
              "-c", str(PERIOD), "--call-graph", "fp"],
             server_cpu, requests, warmup=1024)
    return record_path


if __name__ == "__main__":
    import argparse
    import shlex

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("name", choices=("hirsel", "tokio", "hyper"))
    parser.add_argument("server")
    parser.add_argument("--requests", type=int, default=32768)
    parser.add_argument("--server-cpu", type=int, default=0)
    parser.add_argument("--client-cpu", type=int, default=1)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--profile-only", action="store_true")
    args = parser.parse_args()
    cpu_pair = {args.server_cpu, args.client_cpu}
    if len(cpu_pair) != 2 or not cpu_pair <= (ALLOWED_CPUS & os.sched_getaffinity(0)):
        parser.error("select two distinct inherited CPUs within 0-11 or 16-27")
    args.output.mkdir(parents=True, exist_ok=True)
    os.sched_setaffinity(0, {args.client_cpu})
    command = shlex.split(args.server)
    destination = args.output.resolve()
    if not args.profile_only:
        instructions, cycles = measure(args.name, command, args.server_cpu, args.requests, destination)
        (destination / f"{args.name}.per-request").write_text(str(instructions))
        (destination / f"{args.name}.cycles-per-request").write_text(str(cycles))
    profile(args.name, command, args.server_cpu, args.requests * 4, destination)
