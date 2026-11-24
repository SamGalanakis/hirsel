"""Keep other host workloads off the local timing CPUs during a shared hold."""
import argparse
from contextlib import contextmanager, ExitStack
import fcntl
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

from bench_cpus import cpu_list

RUNTIME = Path("/run/user") / str(os.getuid())
REGIMES = RUNTIME / "hirsel-mc-regimes.lock"
BOARD = RUNTIME / "hirsel-board.lock"
SLICES = ("user.slice", "system.slice")
TIMING_SLICE = "hirsel_timing.slice"
RESERVED = set(range(4)) | set(range(16, 20))
STATE = RUNTIME / "hirsel-timing-shield.json"
CGROUP = Path("/sys/fs/cgroup")
SIGNALS = (signal.SIGHUP, signal.SIGINT, signal.SIGQUIT, signal.SIGTERM)


def local_host():
    return os.getuid() == 1000 and os.uname().nodename.split(".")[0] == "turbo2-sam"


def uses_shield(paths, force=False):
    return local_host() and (force or any(Path(path).resolve() in (REGIMES, BOARD) for path in paths))


def measurement_affinity(*paths, force=False):
    affinity = os.sched_getaffinity(0)
    # CPU validation can run while a different holder has confined this
    # process. The actual pin happens in the system scope after acquisition.
    if uses_shield(paths, force) and not in_timing_slice():
        try:
            masks = json.loads(STATE.read_text())
        except FileNotFoundError:
            pass
        else:
            affinity |= RESERVED | set(masks.get("also", ()))
    return affinity


def cgroup():
    for line in Path("/proc/self/cgroup").read_text().splitlines():
        if line.startswith("0::/"):
            return CGROUP / line[4:]
    raise RuntimeError("local timing requires cgroup v2")


def in_timing_slice():
    return cgroup().is_relative_to(CGROUP / TIMING_SLICE)


def enter_scope(properties=()):
    # Register our own PID with the system manager. exec can then preserve
    # the caller's UID, groups, environment, stdio and other open descriptors.
    unit = f"hirsel-timing-{os.getpid()}-{time.monotonic_ns()}.scope"
    props = (
        ("Slice", "s", TIMING_SLICE),
        ("CollectMode", "s", "inactive-or-failed"),
        ("PIDs", "au", "1", str(os.getpid())),
        *properties,
    )
    affinity = os.sched_getaffinity(0)
    subprocess.run([
        "sudo", "-n", "busctl", "call", "org.freedesktop.systemd1",
        "/org/freedesktop/systemd1", "org.freedesktop.systemd1.Manager",
        "StartTransientUnit", "ssa(sv)a(sa(sv))", unit, "fail", str(len(props)),
        *(part for prop in props for part in prop), "0",
    ], check=True, stdout=subprocess.DEVNULL)
    deadline = time.monotonic() + 5
    while cgroup() != CGROUP / TIMING_SLICE / unit:
        if time.monotonic() >= deadline:
            raise RuntimeError(f"systemd did not attach the timing process to {unit}")
        time.sleep(0.005)
    os.sched_setaffinity(0, affinity)


def scoped_command(command, *, memory_bytes, jobs=None, runtime_seconds=None):
    argv = [sys.executable, str(Path(__file__).resolve()), "scope", str(memory_bytes)]
    if jobs is not None:
        argv += ["--jobs", str(jobs)]
    if runtime_seconds is not None:
        argv += ["--runtime-seconds", str(runtime_seconds)]
    return [*argv, "--", *command]


def allowed_cpus(unit):
    return subprocess.check_output(
        ["systemctl", "show", unit, "--property=AllowedCPUs", "--value"], text=True,
    ).strip()


def set_cpus(unit, value):
    subprocess.run(["sudo", "-n", "systemctl", "set-property", "--runtime",
                    unit, f"AllowedCPUs={value}"], check=True)


def unshield():
    if not STATE.exists():
        return False
    masks = json.loads(STATE.read_text())
    errors = []
    for unit in SLICES:
        try:
            if allowed_cpus(unit) != masks[unit]:
                set_cpus(unit, masks[unit])
        except (OSError, subprocess.SubprocessError) as error:
            errors.append(f"{unit}: {error}")
    if errors:
        raise RuntimeError("restore timing shield: " + ", ".join(errors))
    STATE.unlink()
    return True


def shield(also):
    before = {unit: allowed_cpus(unit) for unit in SLICES}
    online = set(cpu_list(Path("/sys/devices/system/cpu/online").read_text().strip()))
    reserved = RESERVED | set(also)
    if not reserved <= online:
        raise RuntimeError("timing CPUs are offline")
    after = {unit: (set(cpu_list(",".join(value.split()))) if value else online) - reserved
             for unit, value in before.items()}
    minimum = 8 if also else 1
    for unit, cpus in after.items():
        if len(cpus & online) < minimum:
            raise RuntimeError(f"the timing shield would leave {unit} with fewer than {minimum} online CPUs")
    # Publish the previous settings before changing either slice. Keep them
    # until both slices are restored, including after a partial failure.
    pending = STATE.with_suffix(".tmp")
    pending.write_text(json.dumps(before | {"also": sorted(also)}))
    pending.replace(STATE)
    for unit, cpus in after.items():
        set_cpus(unit, ",".join(map(str, sorted(cpus))))


@contextmanager
def defer_signals():
    mask = signal.pthread_sigmask(signal.SIG_BLOCK, (*SIGNALS, signal.SIGALRM))
    try:
        yield
    finally:
        signal.pthread_sigmask(signal.SIG_SETMASK, mask)


def exit_on_signal(signum, _frame):
    raise SystemExit(128 + signum)


@contextmanager
def shielded(also):
    handlers = {sig: signal.signal(sig, exit_on_signal) for sig in SIGNALS}
    original = None
    keeper = None
    try:
        with defer_signals():
            unshield()
            if not in_timing_slice():
                original = cgroup()
                affinity = os.sched_getaffinity(0)
                # An empty transient scope can disappear before we return.
                # Keep the original cgroup occupied until our pipe closes.
                keeper = subprocess.Popen(["/bin/cat"], stdin=subprocess.PIPE,
                                          stdout=subprocess.DEVNULL)
                enter_scope()
            shield(also)
        yield
    finally:
        with defer_signals():
            try:
                unshield()
            finally:
                try:
                    if original is not None and cgroup() != original:
                        subprocess.run(
                            ["sudo", "-n", "tee", original / "cgroup.procs"], check=True,
                            input=f"{os.getpid()}\n", text=True, stdout=subprocess.DEVNULL,
                        )
                        os.sched_setaffinity(0, affinity)
                finally:
                    try:
                        if keeper is not None:
                            keeper.stdin.close()
                            keeper.wait()
                    finally:
                        for sig, handler in handlers.items():
                            signal.signal(sig, handler)


@contextmanager
def file_locks(paths, *, blocking=True):
    with ExitStack() as stack:
        for path in dict.fromkeys(Path(path).resolve() for path in paths):
            handle = stack.enter_context(path.open("a"))
            fcntl.flock(handle, fcntl.LOCK_EX | (0 if blocking else fcntl.LOCK_NB))
        yield


@contextmanager
def hold(*paths, blocking=True, force=False, also=()):
    local = uses_shield(paths, force)
    if also and not local:
        raise RuntimeError("extra timing CPUs require the local timing shield")
    # Hold both canonical locks during the update and restore. Older callers
    # may take either lock independently and must never have their CPUs moved.
    with file_locks((REGIMES, BOARD, *paths) if local else paths, blocking=blocking):
        if local:
            with shielded(also):
                yield
        else:
            yield


def clear_stale():
    if local_host() and STATE.exists():
        with ExitStack() as locks:
            try:
                locks.enter_context(file_locks((REGIMES, BOARD), blocking=False))
            except BlockingIOError:
                return
            with defer_signals():
                if unshield():
                    print("round-check: cleared stale timing shield")


def main():
    if sys.argv[1:] == ["clear-stale"]:
        clear_stale()
        return 0
    if sys.argv[1:2] == ["run"] and "--" in sys.argv:
        separator = sys.argv.index("--")
        parser = argparse.ArgumentParser(prog=f"{sys.argv[0]} run")
        parser.add_argument("--also", type=cpu_list, default=(), help="extra CPUs to shield")
        args = parser.parse_args(sys.argv[2:separator])
        command = sys.argv[separator + 1:]
        if not command:
            parser.error("a timing command is required after --")
        with hold(REGIMES, BOARD, also=args.also):
            status = subprocess.run(command).returncode
            return status if status >= 0 else 128 - status
    if sys.argv[1:2] == ["scope"] and "--" in sys.argv:
        separator = sys.argv.index("--")
        parser = argparse.ArgumentParser()
        parser.add_argument("memory_bytes", type=int)
        parser.add_argument("--jobs", type=int)
        parser.add_argument("--runtime-seconds", type=int)
        args = parser.parse_args(sys.argv[2:separator])
        command = sys.argv[separator + 1:]
        if not local_host() or not in_timing_slice() or not command:
            parser.error("a capped timing scope must be launched from a timing hold")
        props = [("MemoryMax", "t", str(args.memory_bytes)), ("MemorySwapMax", "t", "0")]
        if args.jobs is not None:
            props += [("CPUQuotaPerSecUSec", "t", str(args.jobs * 1_000_000)),
                      ("OOMPolicy", "s", "continue")]
        if args.runtime_seconds is not None:
            props += [("RuntimeMaxUSec", "t", str(args.runtime_seconds * 1_000_000)),
                      ("TimeoutStopUSec", "t", "2000000")]
        enter_scope(props)
        os.execvp(command[0], command)
    print(f"Usage: {sys.argv[0]} clear-stale | run [--also CPUS] -- command [args...]", file=sys.stderr)
    return 2


if __name__ == "__main__":
    raise SystemExit(main())
