#!/usr/bin/env python3
"""Run two native glibc Places under a delegated systemd user service."""
import argparse
from collections import deque
from datetime import datetime, timedelta, timezone
import errno
from hashlib import file_digest
import json
import math
import os
from pathlib import Path
import re
import select
import shutil
import signal
import subprocess
import sys
import time

from archive import KEYS, Sink
from gates import check_positive, exact, measured
from observe import History, Receipts, atomic_json, resources, utc_now, wait_until

MIB = 1024**2
HOUR = 3600
IO_DEADLINE = 180
PACK = re.compile(r"p-([1-9][0-9]*)(\.tmp)?")
JOURNAL = re.compile(r"j-([1-9][0-9]*)(\.end)?")
MEASURE_FIELDS = (
    "logical_bytes", "resident_bytes", "pins", "open_snapshots", "export_leases",
    "scrub_leases", "prev_first", "first_live", "tail", "archived_version",
)


def child_groups():
    cgroup = next(line[3:] for line in Path("/proc/self/cgroup").read_text().splitlines() if line.startswith("0::"))
    parent = Path("/sys/fs/cgroup") / cgroup.lstrip("/")
    if "memory" not in (parent / "cgroup.controllers").read_text().split():
        raise ValueError("launch in a systemd user service with Delegate=yes and memory control")
    owner = parent / "supervisor"
    owner.mkdir()
    (owner / "cgroup.procs").write_text(str(os.getpid()))
    (parent / "cgroup.subtree_control").write_text("+memory")
    result = {}
    for name in ("A", "B"):
        group = parent / f"place-{name}"
        group.mkdir()
        (group / "memory.max").write_text(str(2 * 1024 * MIB))
        (group / "memory.swap.max").write_text("0")
        result[name] = group
    return result


def fill_until_enospc(path):
    count = 0
    block = b"\0" * MIB
    with path.open("xb", buffering=0) as handle:
        try:
            while True:
                n = handle.write(block)
                if not n:
                    raise OSError("zero-byte pressure write")
                count += n
        except OSError as error:
            if error.errno != errno.ENOSPC:
                raise
        os.fsync(handle.fileno())
    return count


class Place:
    def __init__(self, name, executable, group, store, evidence, endpoint):
        self.name = name
        self.executable = executable
        self.group = group
        self.store = store
        self.evidence = evidence
        self.endpoint = endpoint
        self.history = History(evidence / f"{name}-history.jsonl")
        self.latencies = deque()
        self.process = None
        self.generation = 0
        self.store_id = None
        self.stderr = None
        self.previous_process = None

    def open(self):
        self.generation += 1
        mode = "create" if self.generation == 1 else "reopen"
        self.stderr = (self.evidence / f"{self.name}-{self.generation}.stderr").open("xb")
        self.buffer = b""
        self.process = subprocess.Popen(
            [sys.executable, __file__, "_enter", self.group, self.executable, mode,
             "archive" if self.name == "A" else "plain", self.endpoint],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self.stderr,
            cwd=self.store, env=dict(os.environ, SOAK_ARCHIVE_SECRET=KEYS["writer"].decode()),
        )
        ready = self.read().split()
        if len(ready) != 2 or ready[0] != "READY" or not re.fullmatch(r"[0-9a-f]{32}", ready[1]):
            raise ValueError(f"Place {self.name} failed to open: {ready}")
        if self.store_id is not None and ready[1] != self.store_id:
            raise ValueError(f"Place {self.name} reopened with a different Store ID")
        self.store_id = ready[1]

    def read(self):
        deadline = time.monotonic() + IO_DEADLINE
        while b"\n" not in self.buffer:
            if not select.select([self.process.stdout], [], [], max(0, deadline - time.monotonic()))[0]:
                raise ValueError(f"Place {self.name} timed out; see {self.stderr.name}")
            data = os.read(self.process.stdout.fileno(), 65536)
            if not data:
                details = Path(self.stderr.name).read_text(errors="replace").strip()
                raise ValueError(f"Place {self.name} closed stdout: {details}; see {self.stderr.name}")
            self.buffer += data
            if len(self.buffer) > 1_000_000:
                raise ValueError(f"Place {self.name} sent an oversized response")
        raw, self.buffer = self.buffer.split(b"\n", 1)
        return raw.decode("ascii")

    def expect(self, wanted):
        received = self.read()
        if received != wanted:
            raise ValueError(f"Place {self.name} expected {wanted!r}, got {received!r}")

    def send(self, command, extra=b""):
        self.process.stdin.write(bytes([command]) + extra)
        self.process.stdin.flush()

    def write(self, crash=False):
        op = self.history.begin()
        start = time.monotonic()
        self.send(1, op.to_bytes(8, "little"))
        self.expect(f"INFLIGHT {op}")
        if crash:
            return op
        self.send(1)
        tag, op_ack, chain = self.read().split()
        end = time.monotonic()
        if tag != "ACK" or int(op_ack) != op:
            raise ValueError(f"Place {self.name} invalid ACK for {op}: {tag} {op_ack}")
        self.history.acknowledge(op, int(chain))
        self.latencies.append((end, (end - start) * 1000))
        return op

    def state(self):
        self.send(2)
        tag, op, chain = self.read().split()
        if tag != "META":
            raise ValueError(f"Place {self.name} missing META")
        result = History.state(int(op), int(chain), {}, {}, {})
        categories = {"ROW": "rows", "BLOB": "blobs", "EVENT": "events"}
        while (line := self.read()) != "END":
            tag, key, value = line.split()
            category = result[categories[tag]]
            if int(key) in category:
                raise ValueError(f"Place {self.name} repeated {tag} {key}")
            category[int(key)] = value if tag == "BLOB" else int(value)
        return self.history.verify(result)

    def cycle(self):
        if self.name == "A":
            self.send(3)
            self.expect("CYCLE")
        else:
            self.send(5)
            self.expect("CUT")

    def measure(self, archive):
        self.send(4)
        tag, *values = self.read().split()
        fields = dict(zip(MEASURE_FIELDS, map(int, values), strict=True))
        if tag != "MEASURE":
            raise ValueError(f"Place {self.name} did not report MEASURE")
        packs, retired = set(), set()
        while (line := self.read()) != "END":
            label, raw = line.split()
            if label not in {"PACK", "RETIRED"}:
                raise ValueError(f"Place {self.name} invalid pack observation {line}")
            (packs if label == "PACK" else retired).add(int(raw))
        names = [item.name for item in self.store.iterdir()]
        allocated = sum(item.stat().st_blocks * 512 for item in self.store.iterdir())
        unreachable = []
        if self.name == "B":
            for name in names:
                pack = PACK.fullmatch(name)
                journal = JOURNAL.fullmatch(name)
                if (name == "MANIFEST.tmp" or (pack and (pack[2] or int(pack[1]) not in packs))
                        or (journal and not fields["first_live"] <= int(journal[1]) <= fields["tail"])):
                    unreachable.append(name)
        fields["leases"] = fields["export_leases"] + fields["scrub_leases"]
        cutoff = time.monotonic() - HOUR
        while self.latencies and self.latencies[0][0] < cutoff:
            self.latencies.popleft()
        if not self.latencies:
            raise ValueError(f"Place {self.name} has no acknowledgements for p99")
        times = sorted(latency for _, latency in self.latencies)
        return dict(
            resources(self.process.pid, self.group, self.store), **fields,
            store_id=self.store_id, device=self.store.stat().st_dev, allocated_bytes=allocated,
            space_amplification=allocated / max(fields["logical_bytes"], 1),
            retired_packs=sum(f"p-{seq}" in names for seq in retired),
            m_files=sum(name.startswith("m-") for name in names),
            debris=len(unreachable), debris_paths=sorted(unreachable),
            pending_multipart=archive["pending_multipart"] if self.name == "A" else 0,
            p99_ms=times[math.ceil(len(times) * 0.99) - 1], latency_samples=len(times),
        )

    def reap(self, expected=None, kill=False):
        proc = self.process
        if proc is None:
            return None
        if proc.poll() is None:
            if kill:
                command = subprocess.check_output(["ps", "-o", "comm=", "-p", str(proc.pid)], text=True).strip()
                named = Path(f"/proc/{proc.pid}/exe").resolve(strict=True)
                if command != self.executable.name[:15] or named != self.executable:
                    raise ValueError(f"refusing SIGKILL for PID {proc.pid}: {command} {named}")
                proc.send_signal(signal.SIGKILL)
            else:
                proc.stdin.close()
        code = proc.wait(timeout=IO_DEADLINE)
        self.previous_process = proc.pid
        proc.stdout.close()
        proc.stdin.close()
        self.stderr.close()
        self.process = None
        if expected is not None and code != expected:
            raise ValueError(f"Place {self.name} returned {code}, expected {expected}; see {self.stderr.name}")
        return self.previous_process


class ArchiveCLI:
    def __init__(self, hirsel, scratch, endpoint, store_id):
        self.hirsel = hirsel
        self.scratch = scratch
        descriptor = scratch / "archive.json"
        atomic_json(descriptor, {"endpoint": endpoint, "region": "us-east-1", "bucket": "drill",
                                 "prefix": "soak/", "parent": store_id})
        self.source = "@" + str(descriptor)

    def run(self, *argv, stdin=None, expiry=False):
        env = {k: v for k, v in os.environ.items() if not k.startswith("HIRSEL_ARCHIVE_")}
        env.update(TMPDIR=str(self.scratch), XDG_STATE_HOME=str(self.scratch / "state"),
                   HIRSEL_ARCHIVE_READ_ACCESS_KEY_ID="reader",
                   HIRSEL_ARCHIVE_READ_SECRET_ACCESS_KEY=KEYS["reader"].decode())
        if expiry:
            env.update(HIRSEL_ARCHIVE_EXPIRY_ACCESS_KEY_ID="expiry",
                       HIRSEL_ARCHIVE_EXPIRY_SECRET_ACCESS_KEY=KEYS["expiry"].decode())
        proc = subprocess.run([self.hirsel, *argv, "--json"], env=env, cwd=self.scratch,
                              input=stdin, capture_output=True, text=True, timeout=IO_DEADLINE)
        if proc.returncode:
            raise ValueError(f"hirsel {' '.join(argv)} returned {proc.returncode}: {proc.stdout} {proc.stderr}")
        reply = json.loads(proc.stdout)
        if reply["exit"] != 0 or reply["v"] != 1:
            raise ValueError(f"invalid archive CLI response: {reply}")
        return reply

    def verify(self):
        result = self.run("store", "archive", "verify", self.source, "--deep")["result"]
        if result["verification"] != "store_graph" or result["open_gap"]:
            raise ValueError(f"archive failed deep verification: {result}")
        return result

    def expire(self, keep_seconds):
        cutoff = max(0, int((time.time() - keep_seconds) * 1000))
        plan = self.run("store", "archive", "expire", self.source, "--at", str(cutoff))
        details = plan["result"]["plan"]
        if int(details["proposed_base"]) <= int(details["previous_base"]):
            raise ValueError(f"archive expiry did not propose advancing the floor: {details}")
        vote = 'host_v1.Decision(plan_digest: "{}", verdict: Approve, entries: [])'.format(
            plan["result"]["plan_digest"])
        result = self.run("op", "decide", plan["operation"]["id"], "--decision", "-",
                          stdin=vote, expiry=True)
        if result["outcome"]["effect"] != "expired":
            raise ValueError(f"archive expiry had no effect: {result}")
        return {"before": details["previous_base"], "after": details["proposed_base"],
                "applied": result["result"]}


def recover(a, b, hirsel, scratch, kind, data):
    directory = scratch / f"{kind}-{b.generation:03}"
    shutil.copytree(b.store, directory)
    previous_sizes = {entry.name: entry.stat().st_size for entry in directory.iterdir()}
    verify = subprocess.run([hirsel, "store", "verify", directory], cwd=scratch, text=True,
                            env=dict(os.environ, TMPDIR=str(scratch)), capture_output=True,
                            timeout=IO_DEADLINE)
    verify_output = verify.stdout + verify.stderr
    (scratch / f"{directory.name}.verify").write_text(verify_output)
    if verify.returncode != 0:
        raise ValueError(f"Place B preserved copy verification failed: {verify_output}")
    old_pid = b.previous_process
    b.open()
    if b.process.pid == old_pid:
        raise ValueError("Place B did not acquire a new PID")
    witness = b.state()
    debris = b.measure({})["debris"]
    if debris:
        raise ValueError(f"Place B opener left {debris} unreachable files after reclaim")
    remaining = {entry.name: entry.stat().st_size for entry in b.store.iterdir()}
    removed = sorted(name for name in previous_sizes.keys() - remaining.keys()
                     if name == "MANIFEST.tmp" or PACK.fullmatch(name) or JOURNAL.fullmatch(name))
    repaired = sorted(name for name, size in previous_sizes.items()
                      if JOURNAL.fullmatch(name) and size == 0 and remaining.get(name, 0) > 0)
    a.write()
    return dict(event=kind, before_pid=old_pid, after_pid=b.process.pid, preserved=str(directory),
                verified_exit=verify.returncode, verifier_output=verify_output.strip(),
                debris_after_reclaim=debris, removed_store_files=removed, repaired_journals=repaired,
                witness=witness, place_a_after=a.state(), **data)


def crash(a, b, hirsel, scratch):
    pending = b.write(crash=True)
    b.reap(expected=-signal.SIGKILL, kill=True)
    return recover(a, b, hirsel, scratch, "sigkill", {"inflight_op": pending})


def pressure(a, b, hirsel, scratch, volume):
    b.write()
    fill = volume / "pressure.fill"
    reserve = volume / "pressure.reserve"
    # Leave room for a new journal and END so the cut can reach a pack write.
    # Both files consume real blocks in Place B's isolated filesystem.
    try:
        with reserve.open("xb") as handle:
            handle.write(b"\0" * (24 * 1024))
        written = fill_until_enospc(fill)
        fs = os.statvfs(volume)
        if written == 0 or fs.f_bavail != 0:
            raise ValueError("the isolated fill did not reach ENOSPC")
        reserve.unlink()
        fs = os.statvfs(volume)
        headroom = fs.f_bavail * fs.f_frsize
        if not 0 < headroom <= 24 * 1024:
            raise ValueError(f"unexpected cut headroom after releasing pressure reserve: {headroom}")
        b.send(5)
        b.expect("ENOSPC 28")
        b.reap(expected=28)
    finally:
        reserve.unlink(missing_ok=True)
        fill.unlink(missing_ok=True)
    return recover(a, b, hirsel, scratch, "enospc",
                   {"fill_bytes": written, "cut_headroom_bytes": headroom,
                    "kernel_errno": errno.ENOSPC, "store_errno": 28})


def file_sha(path):
    with path.open("rb") as handle:
        return file_digest(handle, "sha256").hexdigest()


def profile(mode, interval, crash_at):
    if mode == "dry":
        return {
            "mode": mode, "duration_s": 120, "interval_s": interval, "write_s": 0.5,
            "crash_s": [crash_at], "pressure_s": [55], "verify_s": [30, 100], "expire_s": [85],
            "keep_s": 40,
            "required_events": {"sigkill": 1, "enospc": 1, "archive_verify": 2, "archive_expire": 1},
        }
    hours = 24 if mode == "control" else 72
    return {
        "mode": mode, "duration_s": hours * HOUR, "interval_s": 120, "write_s": 2,
        "crash_s": list(range(2 * HOUR, hours * HOUR + 1, 2 * HOUR)),
        "pressure_s": list(range(12 * HOUR, hours * HOUR, 24 * HOUR)),
        "verify_s": list(range(HOUR, hours * HOUR + 1, HOUR)),
        "expire_s": list(range(13 * HOUR, hours * HOUR, 24 * HOUR)),
        "keep_s": 6 * HOUR,
        "required_events": {"sigkill": hours // 2, "enospc": hours // 24,
                            "archive_verify": hours, "archive_expire": hours // 24},
    }


def run(args, book, groups, sink):
    profile_spec = profile(args.mode, args.dry_interval, args.dry_crash)
    scratch = args.run / "evidence"
    scratch.mkdir()
    store_a = args.run / "place-a"
    store_b = args.pressure_volume / "place-b"
    store_a.mkdir()
    store_b.mkdir()
    if (not args.pressure_volume.is_mount() or store_a.stat().st_dev == store_b.stat().st_dev
            or args.run.is_relative_to(args.pressure_volume)):
        raise ValueError("Place B requires a separately mounted pressure volume")
    capacity = os.statvfs(args.pressure_volume)
    if not 32 * MIB <= capacity.f_blocks * capacity.f_frsize <= 256 * MIB:
        raise ValueError("Place B pressure volume must be between 32 and 256 MiB")
    a = Place("A", args.place, groups["A"], store_a, scratch, sink.endpoint)
    b = Place("B", args.place, groups["B"], store_b, scratch, sink.endpoint)
    try:
        a.open()
        b.open()
        if a.store_id == b.store_id:
            raise ValueError("the two Places have the same Store ID")
        for _ in range(80):
            a.write()
            b.write()
        archive = ArchiveCLI(args.hirsel, scratch, sink.endpoint, a.store_id)
        started = time.monotonic()
        wall_start = datetime.now(timezone.utc)
        identity = {"place_sha256": file_sha(args.place), "hirsel_sha256": file_sha(args.hirsel),
                    "driver": args.revision, "host": os.uname().nodename, "kernel": os.uname().release,
                    "glibc": os.confstr("CS_GNU_LIBC_VERSION"), "volume_bytes": capacity.f_blocks * capacity.f_frsize,
                    "write_s": profile_spec["write_s"], "interval_s": profile_spec["interval_s"],
                    "keep_s": profile_spec["keep_s"]}
        book.emit("start", profile=profile_spec, identity=identity, place_a=a.process.pid,
                  place_b=b.process.pid, supervisor=os.getpid(), positive_proof=check_positive(args.positive),
                  completion_eta=(wall_start + timedelta(seconds=profile_spec["duration_s"])).isoformat())
        frozen = json.loads(args.envelopes.read_text()) if args.envelopes else None
        samples = []
        baseline = None
        next_write = started
        timeline = sorted({*range(0, profile_spec["duration_s"] + 1, profile_spec["interval_s"]),
                           profile_spec["duration_s"], *(offset for name in ("crash_s", "pressure_s", "verify_s", "expire_s")
                                                         for offset in profile_spec[name])})
        for offset in timeline:
            deadline = started + offset
            while time.monotonic() < deadline:
                wait_until(min(deadline, next_write))
                if time.monotonic() >= deadline:
                    break
                a.write()
                b.write()
                next_write = time.monotonic() + profile_spec["write_s"]
            events = []
            if offset in profile_spec["crash_s"]:
                events.append(crash(a, b, args.hirsel, scratch))
            if offset in profile_spec["pressure_s"]:
                events.append(pressure(a, b, args.hirsel, scratch, args.pressure_volume))
            a.cycle()
            b.cycle()
            state_a = a.state()
            state_b = b.state()
            if offset in profile_spec["expire_s"]:
                events.append(dict(event="archive_expire", outcome=archive.expire(profile_spec["keep_s"])))
            if offset in profile_spec["verify_s"]:
                events.append(dict(event="archive_verify", outcome=archive.verify(), witness=state_a))
            archive_metrics = sink.metrics()
            places = {"A": a.measure(archive_metrics), "B": b.measure(archive_metrics)}
            exact(places, baseline)
            if baseline is None:
                baseline = {name: result["fds"] for name, result in places.items()}
            row = book.emit("sample", elapsed_s=time.monotonic() - started, scheduled_s=offset,
                            hour=offset // HOUR if offset in profile_spec["verify_s"] else 0,
                            places=places, witness={"A": state_a, "B": state_b},
                            archive=archive_metrics, events=events)
            if frozen is not None:
                samples.append(row)
                measured({"identity": identity}, samples, frozen)
        if frozen is not None:
            measured({"identity": identity}, samples, frozen, complete=True)
    except Exception as failure:
        for place in (b, a):
            try:
                place.reap()
            except Exception as cleanup:
                failure.add_note(f"Place {place.name} shutdown: {cleanup}")
        raise
    else:
        try:
            b.reap(expected=0)
        finally:
            a.reap(expected=0)
    book.emit("complete", finished=utc_now())


def main():
    if len(sys.argv) > 1 and sys.argv[1] == "_enter":
        _, _, group, executable, *argv = sys.argv
        (Path(group) / "cgroup.procs").write_text(str(os.getpid()))
        os.execv(executable, [executable, *argv])

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("dry", "control", "measured"))
    for name in ("run", "pressure-volume", "place", "hirsel", "positive"):
        parser.add_argument("--" + name, type=Path, required=True)
    parser.add_argument("--envelopes", type=Path)
    parser.add_argument("--revision", required=True)
    parser.add_argument("--dry-interval", type=int, default=20)
    parser.add_argument("--dry-crash", type=int, default=35)
    args = parser.parse_args()
    if (args.mode == "measured") != (args.envelopes is not None):
        parser.error("measured mode requires --envelopes")
    if args.mode == "dry" and (not 5 <= args.dry_interval <= 60 or not 10 <= args.dry_crash <= 110):
        parser.error("dry interval must be 5–60s and dry crash must be within 10–110s")
    for name in ("run", "pressure_volume", "place", "hirsel", "positive"):
        setattr(args, name, getattr(args, name).resolve())
    args.run.mkdir()
    proof = check_positive(args.positive)
    book = Receipts(args.run / "receipts")
    try:
        groups = child_groups()
        with Sink(args.run / "archive.sqlite") as sink:
            print(f"seeded leak rejected: {proof}", flush=True)
            run(args, book, groups, sink)
    except Exception as error:
        book.emit("failed", error=str(error))
        raise


if __name__ == "__main__":
    main()
