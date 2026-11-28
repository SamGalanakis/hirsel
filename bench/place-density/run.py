#!/usr/bin/env python3
"""Measure idle Place density or wake/refuse churn on the production launcher."""
import argparse
from concurrent.futures import ThreadPoolExecutor
from contextlib import contextmanager
from decimal import Decimal
import http.client
from itertools import chain
import json
import math
import os
from pathlib import Path
import re
import select
import shutil
import signal
import socket
import statistics
import subprocess
import sys
import tempfile
import time

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
sys.path.insert(0, str(ROOT / "core/tools"))
from operator_fixture import group_spec, pin_host, submit_fixture
from bench_cpus import busy_percent, cpu_list, cpu_ticks, sibling_cpus
from timing_shield import BOARD, REGIMES, RESERVED, hold, in_timing_slice, local_host, scoped_command

PLACES = 64
SETTLE_SECONDS = 5
QUIET_SECONDS = 20
HOST_IDLE_MS = 120_000
CHURN_IDLE_MS = 50
CHURN_CYCLES = 1000
METRICS = {
    "pss_kib": ("Pss", "KiB"),
    "dirty_swap_kib": ("Private_Dirty + Swap", "KiB"),
    "idle_instructions_per_second": ("Idle CPU", "instructions/s"),
    "wake_p50_ms": ("Cold wake p50", "ms"),
    "wake_p90_ms": ("Cold wake p90", "ms"),
    "threads": ("Threads", ""),
    "fds": ("FDs", ""),
}
LIMITS = {"pss_kib": 5, "dirty_swap_kib": 5, "idle_instructions_per_second": 10}
IDLE_BASELINE_FLOOR = Decimal(1)  # instructions/s per Place
BASELINE = HERE / "baseline.json"
GOAL = "Place density"
ROW = f"{PLACES} live Places"
NOTE = ("Mean per Place across 64 processes sharing one executable. Each reopens its own Store via "
        "the launcher. Memory and descriptor counts follow a 5 s settle. Idle instructions cover a "
        "20 s quiet window. The idle gate allows growth of 10% of the recorded rate, "
        "with a 1 instruction/s per Place floor for the denominator. Wake time runs "
        "from fork to the first ready HTTP response and is ungated.")
ROOT_LIMIT = 512 << 20


def checked(command, **kwargs):
    try:
        return subprocess.run(command, check=True, capture_output=True, text=True, **kwargs).stdout.strip()
    except subprocess.CalledProcessError as error:
        raise RuntimeError(f"{command[0]} {command[1]} exited {error.returncode}: {error.stderr.strip()}") from error


def publish(compiler, scratch):
    source = scratch / "source"
    source.mkdir()
    for name in ("hirsel.hcfg", "main.hirsel"):
        shutil.copyfile(HERE / name, source / name)
    recipe = checked([
        compiler, "build", source, "--emit", "recipe", "--backend", "llvm", "--limit", str(ROOT_LIMIT),
        "--n-place", str(1 << 20), "--code-store", scratch / "code", "--store", scratch / "compile",
        "--dir", "live", "--grant", "Clock", "--grant", "Random", "--grant", "Tasks",
        "--grant", "Process", "--grant", "Stderr", "--grant", "Net=Listen 127.0.0.1/32 0-65535",
    ], timeout=120)
    # Prepare the native objects before the first operator wait. Admission
    # still verifies the recipe and links its image from those objects.
    checked([
        compiler, "build", "--recipe", recipe, "--code-store", scratch / "code",
        "--release", "-o", scratch / "prepared",
    ])
    return recipe


def wait_until(probe, message, seconds=20):
    deadline = time.monotonic() + seconds
    while True:
        result = probe()
        if result:
            return result
        if time.monotonic() >= deadline:
            raise RuntimeError(message)
        time.sleep(0.01)


def served_children(launcher):
    children = {}
    for pid_text in Path(f"/proc/{launcher}/task/{launcher}/children").read_text().split():
        proc = Path("/proc") / pid_text
        try:
            if b"--hirsel-wake" in (proc / "cmdline").read_bytes().split(b"\0"):
                children[(proc / "cwd").readlink()] = int(pid_text)
        except (FileNotFoundError, ProcessLookupError):
            continue
    return children


def stop(process, names, sig):
    if process.poll() is not None:
        return
    name = subprocess.run(["ps", "-o", "comm=", "-p", str(process.pid)],
                          capture_output=True, text=True).stdout.strip()
    if process.poll() is None:
        if name not in names:
            raise RuntimeError(f"unexpected process {process.pid}: {name!r}")
        process.send_signal(sig)


def stop_places(launcher):
    # The launcher owns and reaps these children. pidfds bind cleanup to the
    # child actually observed even if it exits while we are stopping the cohort.
    handles = []
    try:
        for pid in served_children(launcher).values():
            try:
                fd = os.pidfd_open(pid)
            except ProcessLookupError:
                continue
            handles.append(fd)
            name = subprocess.run(["ps", "-o", "comm=", "-p", str(pid)],
                                  capture_output=True, text=True).stdout.strip()
            if name and pid in served_children(launcher).values():
                try:
                    signal.pidfd_send_signal(fd, signal.SIGTERM)
                except ProcessLookupError:
                    continue
        deadline = time.monotonic() + 5
        while served_children(launcher) and time.monotonic() < deadline:
            time.sleep(0.02)
    finally:
        for fd in handles:
            os.close(fd)


@contextmanager
def host(compiler, state, log_path, *, measured=False, places=PLACES, idle_ms=HOST_IDLE_MS):
    command = [compiler, "host", "--state", state, "--max-places", str(places),
               "--idle-ms", str(idle_ms), "--lifecycle", "leader",
               "--admission", ROOT / "tools/tests/fixtures/host-admission.hcfg"]
    names = ("hirsel",)
    if measured and in_timing_slice():
        command = scoped_command(command, memory_bytes=2 << 30)
        names += (Path(sys.executable).name,)
    with log_path.open("w+") as log:
        process = subprocess.Popen(command, stdout=log, stderr=log)
        try:
            def listening():
                if process.poll() is not None:
                    raise RuntimeError(f"launcher exited {process.returncode}")
                with socket.socket(socket.AF_UNIX) as probe:
                    try:
                        probe.connect(str(state / "operator.sock"))
                    except (FileNotFoundError, ConnectionRefusedError):
                        return False
                return True

            wait_until(listening, "launcher did not open its operator socket")
            yield process.pid
        except Exception as error:
            log.seek(0)
            error.add_note(log.read()[-4000:])
            raise
        finally:
            try:
                if process.poll() is None:
                    stop_places(process.pid)
            finally:
                # The kernel terminates any remaining launcher children on
                # parent death, including a child still entering exec.
                stop(process, names, signal.SIGTERM)
                process.wait(timeout=10)


def ready(port):
    def request():
        connection = http.client.HTTPConnection("127.0.0.1", port, timeout=10)
        try:
            started = time.monotonic_ns()
            connection.request("GET", "/hirsel/status")
            response = connection.getresponse()
            payload = response.read()
            ended = time.monotonic_ns()
            body = json.loads(payload)
            if response.status != 200 or body["phase"] != "ready":
                raise RuntimeError(f"port {port} did not return Ready: {response.status} {body}")
            return started, ended, body["digest"]
        except ConnectionRefusedError:
            return None
        finally:
            connection.close()

    return wait_until(request, f"Place on port {port} did not listen")


def register_places(compiler, scratch, state, recipe, launcher, places=PLACES):
    addresses = {}
    sockets = [socket.socket() for _ in range(places)]
    try:
        for index, port in enumerate(sockets):
            port.bind(("127.0.0.1", 0))
            work = scratch / f"place-{index:02d}"
            (work / "live").mkdir(parents=True)
            addresses[work] = port.getsockname()[1]
    finally:
        for port in sockets:
            port.close()

    pin = scratch / "job-context.hcfg"
    pin_host(state, pin)
    def register(work):
        spec = work / "group.hcfg"
        group_spec(spec, work.name, f"127.0.0.1:{addresses[work]}", work / "live", work)
        answer = submit_fixture([
            compiler, "deploy", "--recipe", recipe, "--create", spec, "--context", pin,
            "--code-store", scratch / "code", "--json", "--wait",
        ], timeout=120)["result"]
        if answer["phase"] != "installed":
            raise RuntimeError(f"{work.name} registration failed: {answer}")
        return ready(addresses[work])[2]

    # Admit the cold recipe once before concurrent registrations. Otherwise
    # two admissions reserve the same missing objects against the staging cap.
    remaining = iter(addresses)
    first = register(next(remaining))
    with ThreadPoolExecutor(max_workers=2) as registrations:
        try:
            digests = set()
            for number, digest in enumerate(chain((first,), registrations.map(register, remaining)), 1):
                digests.add(digest)
                if number % 16 == 0 or number == places:
                    print(f"Ready after genesis: {number}/{places}", flush=True)
        except BaseException:
            registrations.shutdown(cancel_futures=True)
            raise
    if len(digests) != 1:
        raise RuntimeError("registrations did not serve one image")
    stop_places(launcher)
    wait_until(lambda: not served_children(launcher), "genesis Places did not go cold")
    # Give the host time to commit the launcher's clean exit reports.
    time.sleep(1)
    # The two Store ID words immediately follow the manifest seal prefix.
    stores = {(work / "live/MANIFEST").read_bytes()[32:48] for work in addresses}
    if len(stores) != places or any(len(store) != 16 for store in stores):
        raise RuntimeError(f"genesis did not create {places} distinct Stores")
    return addresses, digests.pop()


@contextmanager
def record_forks(launcher, path):
    read_cmd, write_cmd = os.pipe()
    read_ack, write_ack = os.pipe()
    with os.fdopen(read_cmd), os.fdopen(write_ack), os.fdopen(write_cmd, "wb") as commands:
        with os.fdopen(read_ack, "rb") as acks, tempfile.TemporaryFile(mode="w+") as log:
            process = subprocess.Popen([
                "perf", "record", "-q", "--no-inherit", "-p", str(launcher), "-k", "mono",
                "-e", "sched:sched_process_fork", "-D", "-1", "--control", f"fd:{read_cmd},{write_ack}",
                "-o", path,
            ], pass_fds=(read_cmd, write_ack), stderr=log)
            try:
                commands.write(b"enable\n")
                commands.flush()
                if not select.select([acks], [], [], 5)[0] or acks.readline() != b"ack\n":
                    raise RuntimeError("perf did not enable the fork trace")
                yield
            finally:
                stop(process, ("perf",), signal.SIGINT)
                exit_code = process.wait(timeout=5)
                if exit_code not in (0, -signal.SIGINT, 128 + signal.SIGINT) or not path.is_file():
                    log.seek(0)
                    raise RuntimeError(f"perf fork trace failed: {log.read().strip()}")


def fork_times(path, launcher):
    trace = checked(["perf", "script", "--ns", "-F", "trace:time,trace", "-i", path], timeout=10)
    times = {}
    for match in re.finditer(r"^\s*(\d+)\.(\d{9}): .*\bpid=(\d+) .*\bchild_pid=(\d+)$", trace, re.M):
        seconds, nanos, parent, child = map(int, match.groups())
        if parent == launcher:
            times[child] = seconds * 10**9 + nanos
    return times


def proc_values(pid):
    proc = Path(f"/proc/{pid}")
    smaps = dict(re.findall(r"^(\w+):\s+(\d+) kB$", (proc / "smaps_rollup").read_text(), re.M))
    return {metric: int(smaps[key]) for metric, key in (
        ("pss_kib", "Pss"), ("private_dirty_kib", "Private_Dirty"), ("swap_kib", "Swap"),
        ("rss_kib", "Rss"), ("shared_clean_kib", "Shared_Clean"),
    )} | resource_counts(pid)


def resource_counts(pid):
    proc = Path(f"/proc/{pid}")
    return {"threads": len(list((proc / "task").iterdir())), "fds": len(list((proc / "fd").iterdir()))}


def churn(compiler, scratch):
    recipe = publish(compiler, scratch)
    state = scratch / "host"
    state.mkdir(mode=0o700)
    # The platform persists its initial idle policy in the catalog at genesis.
    with host(compiler, state, scratch / "register.log", places=1, idle_ms=CHURN_IDLE_MS) as launcher:
        addresses, digest = register_places(compiler, scratch, state, recipe, launcher, places=1)
    work, = addresses
    port = addresses[work]
    with host(compiler, state, scratch / "churn.log", places=1, idle_ms=CHURN_IDLE_MS) as launcher:
        def cold():
            return not served_children(launcher)

        def host_pids():
            children = Path(f"/proc/{launcher}/task/{launcher}/children").read_text().split()
            if not children or not cold():
                raise RuntimeError("churn snapshot requires the platform host and a cold Place")
            return [launcher, *map(int, children)]

        # Warm up once so lazy startup does not count as resource growth.
        ready(port)
        wait_until(cold, "Place did not idle after warmup")
        pids = host_pids()
        start = [resource_counts(pid) for pid in pids]
        for cycle in range(1, CHURN_CYCLES + 1):
            connection = http.client.HTTPConnection("127.0.0.1", port, timeout=10)
            try:
                connection.request("GET", "/hirsel")
                response = connection.getresponse()
                response.read()
                if response.status != 400:
                    raise RuntimeError(f"cycle {cycle}: expected upgrade refusal 400, got {response.status}")
                if ready(port)[2] != digest or cold():
                    raise RuntimeError(f"cycle {cycle}: woke the wrong image")
            finally:
                connection.close()
            wait_until(cold, f"cycle {cycle}: Place did not idle")
        if host_pids() != pids:
            raise RuntimeError("platform processes changed during churn")
        end = [resource_counts(pid) for pid in pids]
    return {"cycles": CHURN_CYCLES, "before": start, "after": end}


def idle_instructions(pids):
    result = subprocess.run([
        "perf", "stat", "-x,", "--no-big-num", "--no-scale", "-e", "instructions:u,duration_time",
        "--timeout", str(QUIET_SECONDS * 1000), "-p", ",".join(map(str, pids)),
    ], capture_output=True, text=True, check=True, timeout=QUIET_SECONDS + 10,
       env=os.environ | {"LC_ALL": "C"})
    events = {}
    for line in result.stderr.splitlines():
        fields = line.split(",")
        if len(fields) >= 5 and fields[2] in ("instructions:u", "duration_time"):
            count, _, event, runtime, active, *_ = fields
            if active != "100.00" or event in events:
                raise RuntimeError(f"invalid perf counter: {line}")
            # A cohort with zero scheduled runtime has executed no instructions.
            events[event] = 0 if count == "<not counted>" and runtime == "0" else int(count)
    return events["instructions:u"], events["duration_time"]


def sample(compiler, state, addresses, digest, scratch):
    with host(compiler, state, scratch / "measure.log", measured=True) as launcher:
        if served_children(launcher):
            raise RuntimeError("a Place is live before the cold wake probes")
        trace = scratch / "forks.perf"
        with record_forks(launcher, trace):
            replies = {work: ready(port) for work, port in addresses.items()}
        forks = fork_times(trace, launcher)
        children = served_children(launcher)
        if children.keys() != addresses.keys():
            raise RuntimeError(f"expected {PLACES} Places after readiness, observed {len(children)}")
        if set(forks) != set(children.values()):
            raise RuntimeError("fork trace did not match the serving processes one to one")
        images = {(Path(f"/proc/{pid}/exe").stat().st_dev, Path(f"/proc/{pid}/exe").stat().st_ino)
                  for pid in children.values()}
        if len(images) != 1 or any(served != digest for _, _, served in replies.values()):
            raise RuntimeError("the Places do not share the registered executable inode")
        wakes = {}
        for work, (started, ended, _) in replies.items():
            forked = forks[children[work]]
            if not started <= forked < ended:
                raise RuntimeError(f"{work.name} did not spawn between the request and its Ready reply")
            wakes[work] = (ended - forked) / 1e6
        time.sleep(SETTLE_SECONDS)
        samples = [dict(place=work.name, pid=pid, wake_ms=wakes[work], **proc_values(pid))
                   for work, pid in sorted(children.items())]
        count, elapsed_ns = idle_instructions(children.values())
        if served_children(launcher) != children:
            raise RuntimeError("a Place exited during the quiet window")

    durations = sorted(wakes.values())
    result = {key: statistics.mean(place[key] for place in samples)
              for key in sorted(samples[0].keys() - {"place", "pid", "wake_ms"})}
    result.update(dirty_swap_kib=result["private_dirty_kib"] + result["swap_kib"],
                  idle_instructions_per_second=count / (elapsed_ns / 1e9) / PLACES,
                  wake_p10_ms=durations[math.ceil(PLACES * 0.1) - 1],
                  wake_p50_ms=statistics.median(durations),
                  wake_p90_ms=durations[math.ceil(PLACES * 0.9) - 1])
    return {"places": PLACES, "metrics": result, "samples": samples, "image": digest,
            "quiet_ns": elapsed_ns, "idle_instructions": count}


def check_idle(cpus):
    before = cpu_ticks(cpus)
    time.sleep(1)
    busy = busy_percent(before, cpu_ticks(cpus), unobserved=100.0)
    if any(pct > 2 for pct in busy.values()):
        raise RuntimeError(f"measurement CPUs are busy: {busy}")
    return busy


def setup(compiler, scratch):
    recipe = publish(compiler, scratch)
    print(f"Registering {PLACES} Places through the launcher", flush=True)
    state = scratch / "host"
    state.mkdir(mode=0o700)
    with host(compiler, state, scratch / "register.log") as launcher:
        addresses, digest = register_places(compiler, scratch, state, recipe, launcher)
    return addresses, digest


def measure(compiler, scratch, prepared, cpus):
    os.sched_setaffinity(0, cpus)
    if os.sched_getaffinity(0) != set(cpus):
        raise RuntimeError(f"could not pin the measurement to CPUs {cpus}")
    idle = check_idle(set(cpus) | set(sibling_cpus(cpus)))
    addresses, digest = prepared
    result = sample(compiler, scratch / "host", addresses, digest, scratch)
    cpu_model = re.search(r"^model name\s*:\s*(.+)$", Path("/proc/cpuinfo").read_text(), re.M)
    if not cpu_model:
        raise RuntimeError("cannot determine benchmark CPU model")
    return dict(result, cpus=list(cpus), cpu_model=cpu_model[1], preflight_busy_percent=idle,
                settle_seconds=SETTLE_SECONDS, quiet_seconds=QUIET_SECONDS,
                source_commit=checked(["git", "-C", ROOT, "rev-parse", "HEAD"], timeout=5))


def board_sample(result):
    if result["places"] != PLACES:
        raise ValueError(f"Place density requires {PLACES} live Places")
    metrics = {}
    for key, (label, unit) in METRICS.items():
        value = result["metrics"][key]
        if not math.isfinite(value) or value < 0 or (value == 0 and key != "idle_instructions_per_second"):
            raise ValueError(f"invalid Place density metric {key}: {value}")
        metrics[key] = {"label": label, "unit": unit, "value": value, "limit_pct": LIMITS.get(key)}
    return {"places": PLACES, "source_commit": result["source_commit"], "note": NOTE, "metrics": metrics,
            "cpus": result["cpus"], "cpu_model": result["cpu_model"]}


def compare(result, baseline):
    current, recorded = board_sample(result), board_sample(baseline)
    if (current["cpu_model"], current["cpus"]) != (recorded["cpu_model"], recorded["cpus"]):
        raise ValueError("Place density must use the baseline measurement CPU model and affinity")
    for key in ("settle_seconds", "quiet_seconds"):
        if result[key] != baseline[key]:
            raise ValueError(f"Place density must use the baseline {key}")
    for key, limit in LIMITS.items():
        metric = current["metrics"][key]
        before = Decimal(str(recorded["metrics"][key]["value"]))
        after = Decimal(str(metric["value"]))
        denominator = max(before, IDLE_BASELINE_FLOOR) if key == "idle_instructions_per_second" else before
        ceiling = before + denominator * limit / 100
        failed = after > ceiling
        yield failed, (f"{metric['label']} {before:,.2f} -> {after:,.2f} {metric['unit']} "
                       f"(limit +{limit}%, ceiling {ceiling:,.2f}){' FAIL' if failed else ''}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--hirsel", type=Path, required=True, help="hirsel from an installed toolchain")
    parser.add_argument("--cpus", type=cpu_list, default=(16, 17, 18, 19))
    parser.add_argument("--lock", type=Path, default=BOARD)
    parser.add_argument("--sample", type=Path, help=argparse.SUPPRESS)
    parser.add_argument("--churn", action="store_true", help="run 1,000 unshielded wake/refuse cycles")
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    compiler = args.hirsel.resolve()
    if args.churn:
        # Genesis writes the compiler's objects and native image under this
        # root. Disk-backed pages can be reclaimed within the board's cap.
        with tempfile.TemporaryDirectory(prefix="hirsel-churn-", dir="/tmp") as directory:
            result = churn(compiler, Path(directory))
        args.output.write_text(json.dumps(result, indent=2) + "\n")
        return 0
    if args.sample:
        addresses, digest = json.load(sys.stdin)
        prepared = {Path(work): port for work, port in addresses}, digest
        result = measure(compiler, args.sample, prepared, args.cpus)
        args.output.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
        return 0

    available = os.sched_getaffinity(0) | (RESERVED if local_host() else set())
    if not set(args.cpus) <= available:
        parser.error(f"CPUs {args.cpus} are unavailable to the timing shield")
    # Store I/O on tmpfs keeps disk and fsync contention out of the samples.
    with tempfile.TemporaryDirectory(prefix="hirsel-density-", dir="/dev/shm") as directory:
        scratch = Path(directory)
        addresses, digest = setup(compiler, scratch)
        places = [(str(path), port) for path, port in addresses.items()]
        print("Waiting for the timing shield", flush=True)
        with hold(REGIMES, args.lock):
            subprocess.run(["timeout", "--foreground", "300", sys.executable, __file__, *sys.argv[1:],
                            "--sample", scratch], input=json.dumps((places, digest)),
                           text=True, check=True)
    return 0


if __name__ == "__main__":
    for sig in (signal.SIGTERM, signal.SIGHUP):
        signal.signal(sig, signal.default_int_handler)
    raise SystemExit(main())
