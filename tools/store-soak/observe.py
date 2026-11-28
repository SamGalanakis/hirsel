"""Durable receipts and Linux process measurements for the soak."""
from collections import Counter
from datetime import datetime, timezone
from hashlib import sha256
import json
import os
from pathlib import Path
import stat
import time

MIB = 1024**2


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), allow_nan=False).encode()


def digest(value):
    return sha256(canonical(value)).hexdigest()


def utc_now():
    return datetime.now(timezone.utc).isoformat(timespec="milliseconds")


def atomic_json(path, value):
    temp = path.with_name("." + path.name)
    with temp.open("xb") as handle:
        handle.write(canonical(value) + b"\n")
        handle.flush()
        os.fsync(handle.fileno())
    temp.replace(path)
    fd = os.open(path.parent, os.O_DIRECTORY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


class Receipts:
    def __init__(self, path):
        self.path = path
        self.path.mkdir()
        self.seq = 0
        self.last = None

    def emit(self, kind, **fields):
        self.seq += 1
        row = dict(seq=self.seq, previous_sha256=self.last, at=utc_now(), kind=kind, **fields)
        self.last = digest(row)
        filename = self.path / f"{self.seq:08}.json"
        atomic_json(filename, dict(row, sha256=self.last))
        print(f"{filename.name} {kind} {self.last}", flush=True)
        return row


def receipts(path):
    result = []
    previous = None
    for seq, file in enumerate(sorted(path.glob("*.json")), 1):
        row = json.loads(file.read_bytes())
        checksum = row.pop("sha256")
        if (file.name != f"{seq:08}.json" or row["seq"] != seq
                or row["previous_sha256"] != previous or digest(row) != checksum):
            raise ValueError(f"receipt chain failed at {file.name}")
        previous = checksum
        result.append(row)
    if not result:
        raise ValueError(f"no receipts in {path}")
    return result


def memory(pid):
    with open(f"/proc/{pid}/smaps_rollup") as handle:
        stats = {}
        for line in handle:
            name, _, fields = line.partition(":")
            if name in {"Pss", "Rss", "Pss_Anon", "Private_Clean", "Private_Dirty", "Private_Hugetlb"}:
                amount, units = fields.split()
                if units != "kB":
                    raise ValueError(f"unexpected smaps_rollup unit {units}")
                stats[name] = int(amount) * 1024
    return {
        "pss_bytes": stats["Pss"],
        "rss_bytes": stats["Rss"],
        "private_pss_bytes": stats["Pss_Anon"],
        "private_rss_bytes": stats["Private_Clean"] + stats["Private_Dirty"] + stats["Private_Hugetlb"],
    }


def descriptors(proc, store):
    counts = Counter()
    closed = []
    for fd in (proc / "fd").iterdir():
        target = fd.readlink().as_posix()
        if target.endswith(" (deleted)"):
            closed.append(target)
        if target.startswith(("socket:[", "pipe:[", "anon_inode:")):
            kind = target.split(":", 1)[0]
            if kind == "anon_inode":
                kind = target
        else:
            mode = fd.stat().st_mode
            if stat.S_ISDIR(mode):
                kind = "directory"
            elif stat.S_ISCHR(mode):
                kind = "character"
            elif target.startswith(str(store) + "/"):
                kind = "store:" + Path(target).name.split("-")[0].removesuffix(" (deleted)")
            else:
                kind = "file"
        counts[kind] += 1
    return dict(counts), sorted(closed)


def resources(pid, group, store):
    proc = Path(f"/proc/{pid}")
    # Field 22 of /proc/PID/stat, after the command in parentheses.
    start_tick = int((proc / "stat").read_text().rsplit(")", 1)[1].split()[19])
    with (proc / "maps").open() as maps:
        vmas = sum(1 for _ in maps)
    fds, closed = descriptors(proc, store)
    uptime = float(Path("/proc/uptime").read_text().split()[0])
    events = (group / "memory.events").read_text().splitlines()
    return dict(memory(pid), pid=pid, start_tick=start_tick,
                uptime_s=round(uptime - start_tick / os.sysconf("SC_CLK_TCK"), 3),
                vmas=vmas, fds=fds, closed_fds=closed,
                cgroup=str(group.relative_to("/sys/fs/cgroup")),
                cgroup_bytes=int((group / "memory.current").read_text()),
                cgroup_peak_bytes=int((group / "memory.peak").read_text()),
                cgroup_events={name: int(n) for name, n in map(str.split, events)})


class History:
    """The external write-ahead ledger and bounded logical model for one Place."""

    def __init__(self, ledger):
        self.ledger = ledger
        self.sent = self.last = self.chain = self.next_event = self.acked = 0
        self.pending = None
        self.rows = {}
        self.blobs = {}
        self.events = {}

    def record(self, kind, op, **fields):
        with self.ledger.open("ab") as handle:
            handle.write(canonical(dict(kind=kind, op=op, at=utc_now(), **fields)) + b"\n")
            handle.flush()
            os.fsync(handle.fileno())

    def begin(self):
        if self.pending is not None:
            raise ValueError("previous operation is unresolved")
        self.sent += 1
        self.pending = self.sent
        self.record("send", self.pending)
        return self.pending

    def plan(self, op):
        if op != self.pending:
            raise ValueError(f"unexpected operation {op}, pending {self.pending}")
        chain = (self.chain * 1099511628211 + op) % (1 << 64)
        rows = dict(self.rows)
        rows[op % 64] = op * 3 + 1
        if op % 4 == 0:
            rows.pop((op + 17) % 64, None)
        payload = bytes((op + offset * 17) % 251 for offset in range(12288))
        blobs = dict(self.blobs)
        blobs[op % 8] = sha256(payload).hexdigest()
        events = dict(self.events)
        events[self.next_event] = op
        events.pop(self.next_event - 32, None)
        return self.state(op, chain, rows, blobs, events)

    @staticmethod
    def state(op, chain, rows, blobs, events):
        return {"last": op, "chain": chain, "rows": rows, "blobs": blobs, "events": events}

    def install(self, state):
        self.last, self.chain = state["last"], state["chain"]
        self.rows, self.blobs, self.events = state["rows"], state["blobs"], state["events"]
        self.pending = None
        self.next_event += 1

    def acknowledge(self, op, chain):
        expected = self.plan(op)
        if chain != expected["chain"]:
            raise ValueError(f"durable acknowledgement {op} has a different commit chain")
        self.record("durable_ack", op, chain=chain)
        self.install(expected)
        self.acked += 1

    def verify(self, observed):
        pending = self.pending
        expected = self.state(self.last, self.chain, self.rows, self.blobs, self.events)
        recovered_pending = pending is not None and observed["last"] == pending
        if recovered_pending:
            expected = self.plan(pending)
        if expected != observed:
            raise ValueError(f"history mismatch after {self.acked} durable acks: "
                             f"expected {digest(expected)}, observed {digest(observed)}; "
                             f"expected last op {expected['last']}, observed {observed['last']}")
        if pending is not None:
            self.record("recovered" if recovered_pending else "lost_unacknowledged", pending)
            if recovered_pending:
                self.install(expected)
            self.pending = None
        return {"last_op": self.last, "acked": self.acked, "state_sha256": digest(observed),
                "acked_loss": 0, "recovered_unacknowledged": recovered_pending}


def wait_until(deadline):
    time.sleep(max(0, deadline - time.monotonic()))
