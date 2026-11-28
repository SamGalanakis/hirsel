#!/usr/bin/env python3
"""Nightly decoder discovery (the board's "Decoder discovery" row).

Two workers, one input per fresh child: the Rust libFuzzer host of the package
archive reader and the covered native image of std's HTTP/1 head parser. Each
run has its own raw coverage file, read against the map of the build that
wrote it, and only decoder sites count. The driver saves the case before it
starts a worker, saves a failure before it shrinks it, confirms the shrunk
case in a fresh worker, and accounts search and reduction child CPU apart.

    discover.py nightly --host H --compiler C --runtime R --out DIR --alert FILE
    discover.py qualify ...   # the same row, then its one-time qualification checks

Exit status: 0 green, 1 a confirmed failure, 2 incomplete, 75 deferred (no spare CPU).
"""
import argparse
import bisect
import hashlib
import json
import os
from pathlib import Path
import random
import resource
import struct
import subprocess
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parent))
import native_worker
from native_worker import ROOT, child_cpu, new_row, stopped, supervise

DEFERRED = 75
QUEUE_LIMIT = 256
FAULTS = ("missing-map", "kill", "timeout", "zero-bodies", "partial")


def digest(data):
    return hashlib.sha256(data).hexdigest()


def commit(path, document):
    """Replace `path` atomically with a document that is already on disk."""
    pending = path.with_name(path.name + ".pending")
    with pending.open("w") as stream:
        json.dump(document, stream, indent=1, sort_keys=True)
        stream.write("\n")
        stream.flush()
        os.fsync(stream.fileno())
    pending.replace(path)


def reviewed(bundle, target):
    document = json.loads((ROOT / "core/tests/decoder-corpus" / bundle).read_text())
    return {case["id"]: (bytes.fromhex(document["objects"][case["digest"]]), case)
            for case in document["cases"] if case["target"] == target}


class Native:
    """std's HTTP/1 request-head parser: `hirsel test --coverage` with std as source."""
    name = "http1"
    accepted = ("complete",)
    limit = 512
    tokens = (b"\r\n", b"\n", b"\r\n\r\n", b"GET ", b"POST ", b" HTTP/1.1", b" HTTP/1.0", b": ", b"Host", b" ", b"\t",
              b"/", b"?", b"\x00", b"\x7f", b"\xff", b":", b"HTTP/")
    mismatch = ("control-get", "partial")

    def __init__(self, args):
        self.compiler, self.runtime = args.compiler, args.runtime

    def cases(self):
        return {name: {"input": raw, "schedule": native_worker.partition(case["schedules"][0], len(raw))}
                for name, (raw, case) in reviewed("http1.json", "http1").items()}

    def controls(self):
        return native_worker.CONTROLS

    def schedule(self, raw, seed):
        return native_worker.schedule(raw, seed)

    def prepare(self, work):
        return native_worker.prepare(self.compiler, self.runtime, work)

    def sites(self, image):
        return native_worker.site_map(Path(image["map"]))

    def run(self, image, sites, case, payload, raw_file, **options):
        return native_worker.run(image, sites, case, raw_file, **options)

    def replay(self, image, case, payload):
        environment = native_worker.case_environment(case["input"], case["schedule"], case.get("expect"))
        return " ".join(f"{key}={value}" for key, value in environment.items()) + " " + image["executable"]


class Host:
    """The package archive frame reader: the libFuzzer host in the driver's test configuration."""
    name = "archive"
    accepted = ("decoded",)
    limit = 1024
    tokens = (b"\x28\xb5\x2f\xfd", b"\x00", b"\xff", b"\x01\x00\x00", b"\x20", b"\x60", b"\xe0", b"\x80")
    mismatch = ("block-rle", "refused")
    test = "package_content::archive::host_target::libfuzzer"
    source = "crates/hirsel-core-driver/src/package_content/archive.rs"

    def __init__(self, args):
        self.executable, self.llvm = args.host, args.llvm

    def cases(self):
        return {name: {"input": raw, "schedule": case["schedules"][1]["max_read"]}
                for name, (raw, case) in reviewed("archive.json", "archive").items()}

    def controls(self):
        return ("block-rle", "truncated-before-block")

    def schedule(self, raw, seed):
        return 1 + seed % 7

    def command(self):
        return [str(self.executable), "--exact", self.test, "--nocapture", "--test-threads", "1"]

    def prepare(self, work):
        """Name each decoder counter of this build once: its innermost source line in the reader."""
        work.mkdir(parents=True, exist_ok=True)
        started = child_cpu()
        described = work / "pcs"
        described.unlink(missing_ok=True)
        ran = subprocess.run(self.command(), env={"HIRSEL_DECODER_DESCRIBE": str(described)}, capture_output=True,
                             text=True, cwd=work)
        if ran.returncode or not described.is_file():
            raise ValueError(f"host did not describe its counters: {ran.stdout[-1000:]} {ran.stderr[-1000:]}")
        data = described.read_bytes()
        header, _, table = data.partition(b"\n")
        magic, version, build, count = header.decode().split()
        if (magic, version) != ("hirsel-decoder-pcs", "1") or len(table) != 8 * int(count):
            raise ValueError("unreadable counter description")
        addresses = struct.unpack(f"<{count}Q", table)
        # Reader code is in symbols of the archive module, or inlined into them.
        symbols = subprocess.run([str(self.llvm / "llvm-nm"), "--defined-only", "-n", "-S", str(self.executable)],
                                 capture_output=True, text=True, check=True).stdout
        ranges = sorted((int(start, 16), int(start, 16) + int(size, 16)) for start, size, kind, name in
                        (line.split() for line in symbols.splitlines() if line.count(" ") == 3)
                        if kind in "tT" and "15package_content7archive" in name)
        starts = [start for start, _ in ranges]
        candidates = [index for index, address in enumerate(addresses) if address and (
            at := bisect.bisect_right(starts, address) - 1) >= 0 and address < ranges[at][1]]
        placed = subprocess.run(
            [str(self.llvm / "llvm-symbolizer"), "--obj=" + str(self.executable), "--output-style=JSON"],
            input="".join(f"{addresses[index]:#x}\n" for index in candidates), capture_output=True, text=True,
            check=True).stdout
        # Production code precedes the module's first test item.
        lines = (ROOT / self.source).read_text().splitlines()
        tests = next(number for number, line in enumerate(lines, 1) if line.startswith(("#[test]", "#[cfg(test)]")))
        sites, names, seen = [], [], {}
        for index, row in zip(candidates, map(json.loads, placed.splitlines())):
            frame = (row.get("Symbol") or [{}])[0]
            if not frame.get("FileName", "").endswith(self.source) or not 0 < frame.get("Line", 0) < tests:
                continue
            function = frame["FunctionName"].split("::<")[0].rsplit("::", 1)[-1]
            key = f"archive.rs:{frame['Line']}:{frame['Column']} {function}"
            ordinal = seen[key, frame["FunctionName"]] = seen.get((key, frame["FunctionName"]), -1) + 1
            sites.append(index)
            names.append(f"{key}#{ordinal}")
        if not sites:
            raise ValueError("no decoder counter found in the host")
        (work / "sites.bin").write_bytes(struct.pack(f"<{len(sites)}I", *sites))
        commit(work / "sites.json", {"build": build, "executable": str(self.executable), "sites": names})
        return {"target": self.name, "executable": str(self.executable), "map": str(work / "sites.json"),
                "listed": str(work / "sites.bin"), "build": build, "decoder_sites": len(set(names)),
                "counters": int(count), "prepare_cpu": child_cpu() - started}

    def sites(self, image):
        document = json.loads(Path(image["map"]).read_text())
        if document.get("executable") != image["executable"] or not document.get("sites"):
            raise ValueError(f"{image['map']}: not the site map of {image['executable']}")
        return document

    def environment(self, image, case, payload, raw_file, timeout=20):
        arguments = ["-rss_limit_mb=2048", f"-timeout={timeout}", str(payload)]
        environment = {"HIRSEL_DECODER_FUZZ_ARGS": "\n".join(arguments), "HIRSEL_DECODER_RAW": str(raw_file),
                       "HIRSEL_DECODER_SITES": image["listed"], "HIRSEL_DECODER_MAX_READ": str(case["schedule"])}
        if case.get("expect"):
            environment["HIRSEL_DECODER_EXPECT"] = case["expect"]
        return environment

    def run(self, image, sites, case, payload, raw_file, *, timeout=20, cwd=None, fault=None):
        environment = self.environment(image, case, payload, raw_file, timeout)
        if fault == "zero-bodies":
            del environment["HIRSEL_DECODER_FUZZ_ARGS"]
        row = new_row()
        # libFuzzer's own alarm reports a slow input first. The driver's bound is the backstop.
        row["started"], status, stdout, stderr, timed_out, row["cpu"] = supervise(
            self.command(), environment, timeout + 10, cwd, fault)
        if not row["started"]:
            row["incomplete"] = "worker did not start: " + stderr.decode(errors="replace")
            return row
        text = stderr.decode(errors="replace")
        stop = stopped(row, status, timed_out, timeout + 10)
        if fault == "partial":
            native_worker.truncate(Path(raw_file))
        if (error := text.find("ERROR: libFuzzer: ")) >= 0 and "deadly signal" not in text[error:error + 40]:
            row["failure"] = "resource: " + text[error + 18:].split("\n")[0].split(" (")[0].split(" after")[0]
            row["detail"], stop = text[error:error + 400], True
        try:
            lines = Path(raw_file).read_text(errors="replace").splitlines()
        except FileNotFoundError:
            lines = []
        header = lines[0].split() if lines else []
        if not lines:
            unobserved = "no raw file"
        elif header[:2] != ["hirsel-decoder-raw", "1"] or len(header) != 4:
            unobserved = "raw file has no header"
        elif header[2] != sites["build"] or int(header[3]) != len(sites["sites"]):
            unobserved = "raw file matches no record of the site map"
        elif lines[-1] != "end" or len(lines) != 5 or len(lines[3]) != 6 + len(sites["sites"]):
            unobserved = "raw file has no end mark"
        else:
            unobserved = None
        row["entered"] = len(lines) > 1 and lines[1] == "entered" and unobserved in (None, "raw file has no end mark")
        if unobserved:
            if not stop:
                row["incomplete"] = "zero bodies" if unobserved == "no raw file" and status == 0 else unobserved
            return row
        row["reached"] = sorted({name for name, flag in zip(sites["sites"], lines[3][6:]) if flag == "1"})
        kind, _, rest = lines[2].partition(" ")
        if kind == "failed":
            # The assertion's site names it. Its message holds the values of this input.
            site, _, message = rest.partition(" ")
            row["failure"], row["detail"], stop = f"panic at {site}", message, False
        if stop:
            return row
        row["completed"] = True
        if kind == "observed":
            row["status"], row["observation"] = rest.split(" ")[0], lines[2]
            if status != 0:
                row["failure"], row["detail"] = f"exit {status}", text[-400:]
        row["comparable"] = True
        return row

    def replay(self, image, case, payload):
        environment = self.environment(image, case, payload, "RAW")
        del environment["HIRSEL_DECODER_RAW"], environment["HIRSEL_DECODER_SITES"]
        return " ".join(f"{key}={value!r}" for key, value in environment.items()) + " " + " ".join(self.command())


def mutate(data, rng, corpus, tokens, limit):
    data = bytearray(data)
    for _ in range(rng.randint(1, 4)):
        at = rng.randint(0, len(data))
        choice = rng.randrange(8)
        if choice == 0 and data:
            data[rng.randrange(len(data))] ^= 1 << rng.randrange(8)
        elif choice == 1 and data:
            data[rng.randrange(len(data))] = rng.randrange(256)
        elif choice == 2:
            data[at:at] = bytes(rng.randrange(256) for _ in range(rng.randint(1, 4)))
        elif choice == 3 and data:
            del data[at:at + rng.randint(1, max(1, len(data) // 4))]
        elif choice == 4 and data:
            start = rng.randrange(len(data))
            data[at:at] = data[start:start + rng.randint(1, 16)]
        elif choice == 5:
            data[at:at] = rng.choice(tokens)
        elif choice == 6:
            other = rng.choice(corpus)["input"]
            data[at:] = other[rng.randint(0, len(other)):]
        else:
            del data[at:]
    return bytes(data[:limit])


class Row:
    """One target's row: its store, counters and the two CPU accounts."""

    def __init__(self, target, image, out, args, alert):
        self.target, self.image, self.out, self.args, self.alert = target, image, out, args, alert
        self.sites = target.sites(image)
        for directory in ("objects", "raw", "failures", "work"):
            (out / directory).mkdir(parents=True, exist_ok=True)
        self.counts = dict.fromkeys(("selected", "started", "body_entered", "completed", "comparable", "accepted",
                                     "partial_refused", "unobserved", "red", "confirmed_replays", "kept"), 0)
        self.cpu = {"search": 0.0, "reduction": 0.0}
        self.reaches, self.queue, self.failures, self.incomplete, self.signatures = set(), [], [], [], set()
        self.runs = 0

    def publish(self, data):
        path = self.out / "objects" / digest(data)
        if not path.is_file():
            pending = path.with_name(path.name + ".pending")
            with pending.open("wb") as stream:
                stream.write(data)
                stream.flush()
                os.fsync(stream.fileno())
            pending.replace(path)
        return path

    def document(self, case):
        return {"target": self.target.name, "digest": digest(case["input"]), "bytes": len(case["input"]),
                "schedule": case["schedule"], "expect": case.get("expect"), "provenance": case["provenance"]}

    def execute(self, case, phase, fault=None):
        """Publish the payload, save the in-flight case, then start one fresh worker for it."""
        payload = self.publish(case["input"])
        commit(self.out / "inflight.json", self.document(case) | {"phase": phase, "run": self.runs})
        raw_file = self.out / "raw" / f"{self.runs:06d}-{payload.name[:12]}.raw"
        self.runs += 1
        row = self.target.run(self.image, self.sites, case, payload, raw_file, cwd=self.out / "work",
                              **({"fault": fault} if fault else {}))
        self.cpu[phase] += row["cpu"]
        self.counts["selected"] += 1
        for key, counted in (("started", "started"), ("entered", "body_entered"), ("completed", "completed"),
                             ("comparable", "comparable")):
            self.counts[counted] += bool(row[key])
        if row["completed"] and row["status"] is not None:
            self.counts["accepted" if row["status"] in self.target.accepted else "partial_refused"] += 1
        if row["reached"] is None:
            self.counts["unobserved"] += 1
        if row["incomplete"]:
            self.incomplete.append(f"run {self.runs - 1} ({payload.name[:12]}): {row['incomplete']}")
        if not row["failure"]:
            raw_file.unlink(missing_ok=True)
        (self.out / "inflight.json").unlink()
        return row

    def reduce(self, case, signature, budget):
        """Bounded shrinking: drop ever smaller spans while a fresh worker shows the same failure."""
        data, steps, size = case["input"], 0, len(case["input"])
        # Stop before a step that would cross the budget: leave room for the confirmation.
        started, reserve = self.cpu["reduction"], 0.01
        while size >= 1:
            at = 0
            while at < len(data):
                if self.cpu["reduction"] + 2 * reserve >= budget:
                    return data, steps, "budget"
                smaller = data[:at] + data[at + size:]
                trial = self.case(smaller, case["provenance"] | {"reduces": digest(case["input"])}, case.get("expect"),
                                  case["seed"])
                steps += 1
                failed = self.execute(trial, "reduction")["failure"]
                reserve = max(reserve, (self.cpu["reduction"] - started) / steps)
                if failed == signature:
                    data = smaller
                else:
                    at += size
            size = min(size // 2, len(data))
        return data, steps, "minimal"

    def case(self, data, provenance, expect=None, seed=0):
        return {"input": data, "schedule": self.target.schedule(data, seed), "expect": expect, "seed": seed,
                "provenance": provenance}

    def fail(self, case, row, seeded=False):
        """Keep the original, then shrink it, then confirm the shrunk case in a fresh worker."""
        signature = row["failure"]
        if not seeded:
            if signature in self.signatures:
                return
            self.signatures.add(signature)
            self.counts["red"] += 1
        name = f"{'seeded-' if seeded else ''}{len(self.failures):03d}-{digest(case['input'])[:12]}"
        record = {"signature": signature, "detail": row["detail"], "seeded": seeded, "build": self.image["build"],
                  "original": self.document(case), "saved_before_reduction": time.time()}
        commit(self.out / "failures" / f"{name}.json", record)
        # A failure that a fresh worker does not repeat on the original input is not shrunk.
        again = self.execute(case, "reduction")
        if again["failure"] != signature:
            record |= {"confirmed": False, "confirmation": again["failure"], "reduction": None}
            commit(self.out / "failures" / f"{name}.json", record)
            self.failures.append(record | {"file": f"failures/{name}.json"})
            self.incomplete.append(f"{name}: {signature} did not repeat in a fresh worker")
            return
        data, steps, stopped_by = self.reduce(case, signature, self.args.reduction_cpu_seconds)
        reduced = dict(case, input=data, schedule=self.target.schedule(data, case["seed"]),
                       provenance=case["provenance"] | {"reduces": digest(case["input"])})
        confirmation = self.execute(reduced, "reduction")
        confirmed = confirmation["failure"] == signature
        payload = self.publish(data)
        record |= {"reduced": self.document(reduced), "reduction": {"steps": steps, "stopped": stopped_by},
                   "confirmed": confirmed, "confirmation": confirmation["failure"],
                   "confirmation_detail": confirmation["detail"],
                   "replay": self.target.replay(self.image, reduced, payload)}
        commit(self.out / "failures" / f"{name}.json", record)
        self.failures.append(record | {"file": f"failures/{name}.json"})
        if not confirmed:
            self.incomplete.append(f"{name}: {signature} did not reproduce in a fresh worker")
            return
        if seeded:
            self.counts["seeded_confirmed"] = self.counts.get("seeded_confirmed", 0) + 1
        else:
            self.counts["confirmed_replays"] += 1
        with self.alert.open("a") as stream:
            stream.write(f"{time.strftime('%Y-%m-%d %H:%M:%S UTC', time.gmtime())} Decoder discovery "
                         f"{'seeded ' if seeded else ''}red in {self.target.name}: {signature}. Case "
                         f"{payload} ({len(data)} bytes, from {len(case['input'])}). Replay: {record['replay']}\n")

    def observe(self, case, row):
        """Keep an input that reached a decoder site no kept input reached."""
        if row["failure"]:
            self.fail(case, row)
            return 0
        if not row["comparable"]:
            return 0
        novel = set(row["reached"]) - self.reaches
        if novel:
            self.reaches |= novel
            self.counts["kept"] += 1
            self.queue.append(self.document(case) | {"novel_sites": len(novel)})
            # The nightly queue is a cache: evict the oldest candidates, keep the payloads' manifest whole.
            del self.queue[:-QUEUE_LIMIT]
            commit(self.out / "queue.json", self.queue)
        return len(novel)

    def parity(self, cases):
        """A/B/A and B/A/B: equal target-only signals for one input, different ones for the two."""
        names = self.target.controls()
        evidence, signals = [], {name: set() for name in names}
        for order in ((0, 1, 0), (1, 0, 1)):
            for index in order:
                name = names[index]
                case = dict(cases[name], seed=0, provenance={"control": name})
                row = self.execute(case, "search")
                signal = digest("\n".join(row["reached"]).encode())[:16] if row["reached"] is not None else None
                signals[name].add((signal, row["observation"]))
                evidence.append({"input": name, "sites": len(row["reached"] or ()), "signal": signal,
                                 "observation": row["observation"], "failure": row["failure"]})
                self.observe(case, row)
        a, b = (signals[name] for name in names)
        passed = len(a) == 1 and len(b) == 1 and a != b and next(iter(a))[0] and next(iter(b))[0] and (
            next(iter(a))[0] != next(iter(b))[0])
        if not passed:
            self.incomplete.append("parity: the controls' target-only signals are not stable and distinct")
        return {"passed": bool(passed), "sequence": evidence}

    def search(self, cases, rng):
        corpus = []
        seen = set()
        for name, case in sorted(cases.items()):
            case = dict(case, seed=0, provenance={"reviewed": name})
            seen.add(digest(case["input"]))
            row = self.execute(case, "search")
            self.observe(case, row)
            if row["comparable"] and not row["failure"]:
                corpus.append(case)
        skipped = 0
        while corpus and self.cpu["search"] < self.args.search_cpu_seconds and skipped < 10000 and (
                self.args.max_runs is None or self.runs < self.args.max_runs):
            parent = rng.choice(corpus)
            seed = rng.getrandbits(32)
            data = mutate(parent["input"], rng, corpus, self.target.tokens, self.target.limit)
            if digest(data) in seen:
                skipped += 1
                continue
            seen.add(digest(data))
            case = self.case(data, {"seed": self.args.seed, "parent": digest(parent["input"]), "run": self.runs},
                             seed=seed)
            row = self.execute(case, "search")
            if self.observe(case, row):
                corpus.append(case)

    def report(self, **extra):
        status = ("failed" if self.counts["confirmed_replays"] else "incomplete" if self.incomplete
                  or not self.counts["completed"] or self.counts["completed"] != self.counts["started"]
                  or self.counts["comparable"] != self.counts["completed"] else "green")
        return {"status": status, **self.counts, "named_reaches": len(self.reaches),
                "reached_sites": sorted(self.reaches), "decoder_sites": self.image["decoder_sites"],
                "child_cpu_seconds": {key: round(value, 3) for key, value in self.cpu.items()},
                "prepare_cpu_seconds": round(self.image["prepare_cpu"], 3), "build": self.image["build"],
                "incomplete": self.incomplete[:20], "failures": self.failures, **extra}


def spare_cpus(cpus, window):
    """Idle CPUs of the allocation over `window` seconds, from /proc/stat."""
    def sample():
        rows = {}
        for line in Path("/proc/stat").read_text().splitlines():
            name, *fields = line.split()
            if name[:3] == "cpu" and name[3:].isdigit() and int(name[3:]) in cpus:
                values = [int(field) for field in fields[:8]]
                rows[int(name[3:])] = (values[3] + values[4], sum(values))
        return rows
    before = sample()
    time.sleep(window)
    after = sample()
    return sum((after[cpu][0] - before[cpu][0]) / max(1, after[cpu][1] - before[cpu][1]) for cpu in cpus)


def qualify(target, image, out, args, row, cases):
    """The one-time checks of the two workers. None of them counts in the row above."""
    checks = {}
    names = target.controls()
    control = dict(cases[names[0]], seed=0, provenance={"control": names[0]})
    # No selection and no input: the driver adds no reach of its own.
    empty = Row(target, image, out / "empty", args, out / "empty/alert.txt")
    before = set(empty.reaches)
    worker = empty.execute(dict(control, provenance={"qualification": "empty"}), "search", fault="zero-bodies")
    checks["cpu_outside_row"] = sum(empty.cpu.values())
    checks["empty"] = {"selected_without_cases": 0, "novel_sites": len(empty.reaches - before),
                       "worker_without_input": {"reached": worker["reached"], "entered": worker["entered"],
                                                "incomplete": worker["incomplete"]},
                       "passed": not empty.reaches and not worker["reached"] and not worker["entered"]}
    # A reviewed input with a wrong declared status, through saving, shrinking and confirmation.
    name, expect = target.mismatch
    seeded = dict(cases[name], seed=0, expect=expect, provenance={"qualification": "seeded oracle mismatch", "of": name})
    before = len(row.failures)
    first = row.execute(seeded, "search")
    if first["failure"]:
        row.fail(seeded, first, seeded=True)
    record = row.failures[before] if len(row.failures) > before else {}
    checks["seeded_mismatch"] = {
        "input": name, "declared": expect, "observed": first["observation"], "signature": first["failure"],
        "original_bytes": len(seeded["input"]), "reduced_bytes": record.get("reduced", {}).get("bytes"),
        "reduction": record.get("reduction"), "confirmed": record.get("confirmed", False), "file": record.get("file"),
        "passed": bool(record.get("confirmed")) and record["reduced"]["bytes"] <= len(seeded["input"])}
    # Each collection fault alone, on a control that passes without it.
    checks["negative"] = {}
    for fault in FAULTS:
        directory = out / f"fault-{fault}"
        try:
            broken = dict(image)
            if fault == "missing-map":
                broken["map"] = str(directory / "missing.covmap")
            faulted = Row(target, broken, directory, args, directory / "alert.txt")
            result = faulted.execute(dict(control, provenance={"qualification": fault}), "search",
                                     fault=None if fault == "missing-map" else fault)
            if result["failure"]:
                faulted.fail(control | {"provenance": {"qualification": fault}}, result)
            report = faulted.report()
            checks["cpu_outside_row"] += sum(faulted.cpu.values())
            checks["negative"][fault] = {"status": report["status"], "reason": (report["incomplete"] or [None])[0],
                                         "unobserved": report["unobserved"], "red": report["red"],
                                         "confirmed_replays": report["confirmed_replays"]}
        except (OSError, ValueError) as error:
            checks["negative"][fault] = {"status": "incomplete", "reason": f"map refused: {error}"}
        checks["negative"][fault]["passed"] = checks["negative"][fault]["status"] in ("incomplete", "failed")
    checks["cpu_outside_row"] = round(checks["cpu_outside_row"], 3)
    checks["passed"] = (checks["empty"]["passed"] and checks["seeded_mismatch"]["passed"]
                        and all(check["passed"] for check in checks["negative"].values()))
    return checks


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("mode", choices=("nightly", "qualify"))
    parser.add_argument("--host", type=Path, required=True, help="//:hirsel-core-driver-decoder-fuzz")
    parser.add_argument("--compiler", type=Path, required=True)
    parser.add_argument("--runtime", type=Path, required=True)
    parser.add_argument("--llvm", type=Path, default=ROOT / "tools/buck2/toolchains/llvm-files/bin")
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--alert", type=Path, required=True)
    parser.add_argument("--seed", type=int, default=0)
    parser.add_argument("--search-cpu-seconds", type=float, default=60)
    parser.add_argument("--reduction-cpu-seconds", type=float, default=10)
    parser.add_argument("--capacity-window", type=float, default=2)
    parser.add_argument("--max-runs", type=int, help=argparse.SUPPRESS)
    parser.add_argument("--targets", default="archive,http1", help=argparse.SUPPRESS)
    args = parser.parse_args()
    args.host, args.compiler, args.runtime = args.host.resolve(), args.compiler.resolve(), args.runtime.resolve()
    args.out.mkdir(parents=True, exist_ok=True)
    args.out, args.alert = args.out.resolve(), args.alert.resolve()
    started = time.time()
    targets = [kind(args) for kind in (Host, Native) if kind.name in args.targets.split(",")]
    # One worker runs at a time, so the row needs one idle CPU of its allocation.
    cpus = sorted(os.sched_getaffinity(0))
    idle = spare_cpus(cpus, args.capacity_window)
    cap = len(targets) * (args.search_cpu_seconds + args.reduction_cpu_seconds)
    receipt = {"row": "Decoder discovery", "mode": args.mode, "seed": args.seed, "started": started,
               "capacity": {"cpus": cpus, "window_seconds": args.capacity_window, "idle_cpus": round(idle, 2),
                            "required_idle_cpus": 1, "selected_cap_child_cpu_seconds": cap,
                            "per_target": {"search": args.search_cpu_seconds,
                                           "reduction": args.reduction_cpu_seconds}},
               "targets": {}}
    if idle < 1:
        receipt["status"] = "deferred"
        commit(args.out / "receipt.json", receipt)
        print(f"Decoder discovery deferred: {idle:.2f} idle CPUs of {len(cpus)} allocated, 1 required.")
        return DEFERRED
    rng = random.Random(args.seed)
    used = 0.0
    for target in targets:
        out = args.out / target.name
        try:
            image = target.prepare(out / "image")
            row = Row(target, image, out, args, args.alert)
            leftover = out / "inflight.json"
            recovered = json.loads(leftover.read_text()) if leftover.is_file() else None
            cases = target.cases()
            parity = row.parity(cases)
            row.search(cases, rng)
            report = row.report(parity=parity, interrupted_case=recovered)
            if args.mode == "qualify":
                report["qualification"] = qualify(target, image, out, args, row, cases)
                report |= {"child_cpu_seconds": {key: round(value, 3) for key, value in row.cpu.items()},
                           "seeded_confirmed": row.counts.get("seeded_confirmed", 0)}
                if not report["qualification"]["passed"] and report["status"] == "green":
                    report["status"] = "incomplete"
            used += sum(row.cpu.values())
        except (OSError, ValueError, subprocess.CalledProcessError) as error:
            report = {"status": "incomplete", "incomplete": [f"preparation: {error}"]}
        receipt["targets"][target.name] = report
        print(f"{target.name}: {report['status']} " + " ".join(
            f"{key}={report[key]}" for key in ("selected", "started", "body_entered", "completed", "comparable",
                                               "accepted", "partial_refused", "named_reaches", "unobserved", "red",
                                               "confirmed_replays", "child_cpu_seconds") if key in report), flush=True)
    statuses = {report["status"] for report in receipt["targets"].values()}
    receipt["status"] = "failed" if "failed" in statuses else "incomplete" if "incomplete" in statuses else "green"
    receipt["capacity"]["used_child_cpu_seconds"] = round(used, 3)
    driver = resource.getrusage(resource.RUSAGE_SELF)
    receipt["capacity"]["driver_cpu_seconds"] = round(driver.ru_utime + driver.ru_stime, 3)
    receipt["wall_seconds"] = round(time.time() - started, 1)
    commit(args.out / "receipt.json", receipt)
    print(f"Decoder discovery {receipt['status']}: {used:.1f} of {cap:.0f} child CPU-seconds. See "
          f"{args.out / 'receipt.json'}.")
    return {"green": 0, "failed": 1, "incomplete": 2}[receipt["status"]]


if __name__ == "__main__":
    sys.exit(main())
