#!/usr/bin/env python3
"""Covered native images of the native decoder laws, run warm or one input per fresh child.

`prepare_warm` builds one target of TARGETS with `hirsel test --coverage`, once
per build: the owner's law file with the target's adapter appended, std's given
as source, a package's in a copy of the package. It keeps the test executable
and its `.covmap`. `Warm` serves many inputs in one worker (hirsel-core-rt's
coverage::warm): it clears and reads back only the flags of the target's
records. `fresh` runs one input in a fresh child whose flags map a raw file
that no other run names (docs/coverage.md, "The raw file"). Only sites of the
target's scope count.
"""
import hashlib
import json
import os
from pathlib import Path
import re
import resource
import select
import signal
import struct
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "core/tools"))
sys.path.insert(0, str(Path(__file__).resolve().parent))
from contract_exec import source_manifest
from std_native import fixture_source
from fixtures import NATIVE, native_cases, partition

INPUT_LIMIT = 4096
VARIABLES = ("HIRSEL_DECODER_INPUT", "HIRSEL_DECODER_ENDS", "HIRSEL_DECODER_EXPECT")
RAW_MAGIC = b"HIRSELCV"
RAW_END = 0x45444F4E
RECORD_MAGIC = 0x56434948

# The laws (hirsel-core-rt's coverage::warm): serial, they do not wait, read
# one immutable input from the Env grant, write nothing and answer through the
# exit status. 0 passes; 10 + k a law held with result class k; 20 + k the
# declared expectation is not class k; 3 the input is missing. Each executes on
# fresh case state. An absent expectation checks the owner predicates alone.
# `{hex}` is std's hex module as the owner's source names it.
DISCOVERY = '''
fn discovery_bytes(e: Env, name: String) -> Option[Bytes] {
    match env.get(e, name) {
        Ok(Some(text)) => {hex}.decode(string.as_bytes(text)),
        _ => None
    }
}

fn discovery_exit(status: u8) -> Result[(), Exit] {
    if status == 0u8 {
        Ok(())
    } else {
        Error(Exit(status: status))
    }
}

@test
pub fn {entry}(native_env: Env) -> Result[(), Exit] {
    let Some(raw) = discovery_bytes(native_env, "HIRSEL_DECODER_INPUT") else {
        return Error(Exit(status: 3u8))
    }
    let Some(ends) = discovery_bytes(native_env, "HIRSEL_DECODER_ENDS") else {
        return Error(Exit(status: 3u8))
    }
    discovery_exit({entry}_law(take raw, ends, discovery_bytes(native_env, "HIRSEL_DECODER_EXPECT")))
}
'''

# HTTP/1's owner predicates are the module's own: resume_parity (every two-way
# split and the byte drip) and same_head. 30 + k and 40 + k: one of them broke.
HTTP1 = '''
fn discovery_name(p: http1.Progress) -> String {
    match p {
        Complete => "complete",
        Partial => "partial",
        Bad => "bad",
        TooLarge => "too-large"
    }
}

fn discovered_partition(raw: Bytes, ends: Bytes, expected: http1.Progress, whole: http1.Head) -> bool {
    let mut h = http1.head()
    let mut cur = http1.cursor()
    let mut progress: http1.Progress = Partial
    for i in 0u64 .. list.len(ends) / 2 {
        let high: u64 = int.widen(ends[2 * i])
        let low: u64 = int.widen(ends[2 * i + 1])
        let stop = high * 256 + low
        if stop > list.len(raw) {
            return false
        }
        progress = http1.resume(list.slice(raw, 0, stop), mut h, mut cur)
    }
    progress == expected && same_head(h, whole)
}

fn warm_progress(p: http1.Progress) -> u8 {
    match p {
        Complete => 0u8,
        Partial => 1u8,
        Bad => 2u8,
        TooLarge => 3u8
    }
}

fn warm_http1_law(take raw: Bytes, ends: Bytes, declared: Option[Bytes]) -> u8 {
    let mut whole = http1.head()
    let progress = http1.request(raw, mut whole)
    let k = warm_progress(progress)
    if let Some(name) = declared {
        if name != string.as_bytes(discovery_name(progress)) {
            return 20u8 + k
        }
    }
    if !resume_parity(raw, None) {
        return 30u8 + k
    }
    if !discovered_partition(raw, ends, progress, whole) {
        return 40u8 + k
    }
    if k == 0u8 {
        0u8
    } else {
        10u8 + k
    }
}
'''

# A reviewed cohort's law is its owner test's predicate (`{case}`), which runs
# the cohort's owner laws and answers class 9 when one broke: exit 30. `{name}`
# is the owner's name of a class, the status a declared expectation names.
REVIEWED = '''
fn {entry}_law(take raw: Bytes, ends: Bytes, declared: Option[Bytes]) -> u8 {
    {case}
    if class == 9u8 {
        return 30u8
    }
    if let Some(name) = declared {
        if name != string.as_bytes({name}(class)) {
            return 20u8 + class
        }
    }
    if class == 0u8 {
        0u8
    } else {
        10u8 + class
    }
}
'''


def child_cpu():
    used = resource.getrusage(resource.RUSAGE_CHILDREN)
    return used.ru_utime + used.ru_stime


def site_map(path, entry, scope):
    """The map's records by id. A missing or foreign map is refused here, before any run."""
    document = json.loads(path.read_text())
    if document.get("hirsel_covmap") != 2 or document.get("test") != entry:
        raise ValueError(f"{path}: not the format 2 site map of {entry}")
    records = {bytes.fromhex(record["id"]): [site["site"] for site in record["sites"]]
               for record in document["records"]}
    every = {site for sites in records.values() for site in sites}
    return {"records": records, "all": every, "build": document["build"],
            "decoder": sorted(site for site in every if site.startswith(scope))}


def read_raw(path, records):
    """One run's reached sites and header outcome, or the reason the file is unobserved."""
    try:
        data = path.read_bytes()
    except FileNotFoundError:
        return None, None, "no raw file"
    if len(data) < 52 or data[:8] != RAW_MAGIC:
        return None, None, "raw file has no header"
    version, _page, offset, length, _seeded, outcome = struct.unpack_from("<IIQQII", data, 8)
    if version != 1 or struct.unpack_from("<I", data, 48)[0] != RAW_END:
        return None, None, "raw file has no end mark"
    if offset + length > len(data):
        return None, None, "raw file is shorter than its section"
    reached = set()
    at, end = offset, offset + length
    while at + 8 <= end:
        magic, count = struct.unpack_from("<II", data, at)
        if magic == 0 and count == 0:
            at += 8
            continue
        identity = data[at + 8:at + 40]
        sites = records.get(identity)
        if magic != RECORD_MAGIC or sites is None or count != len(sites) or at + 40 + count > end:
            return None, None, "raw record matches no record of the site map"
        reached.update(site for site, flag in zip(sites, data[at + 40:at + 40 + count]) if flag)
        at += (40 + count + 7) & ~7
    return reached, outcome, None


def reviewed_cases(name):
    """A target's reviewed cases as the worker takes them: the input, with its surface's
    selector byte first when the cohort has several, and its first reviewed partition."""
    if name not in NATIVE:
        bundle = json.loads((ROOT / "core/tests/decoder-corpus" / f"{name}.json").read_text())
        cases = {}
        for case in bundle["cases"]:
            raw = bytes.fromhex(bundle["objects"][case["digest"]])
            cases[case["id"]] = {"input": raw, "schedule": partition(case["schedules"][0], len(raw))}
        return cases
    selected = len(NATIVE[name].get("selectors", ())) > 0
    return {case["id"]: {"input": bytes([case["selector"]] * selected) + case["input"],
                         "schedule": case["ends"][0] if case["ends"] else [len(case["input"])]}
            for case in native_cases(name)}


def schedule(name, raw, seed):
    """A generated legal partition of the bytes after the selector: the reviewed bundle's
    generator over this input. A decoder that takes its input whole has the one fragment."""
    size = max(0, len(raw) - TARGETS[name]["selector"])
    if not TARGETS[name]["streaming"]:
        return [size]
    return partition({"seed": seed & 0xffffffff, "max_chunk": max(1, min(size, 1 + seed % 7))}, size)


def case_environment(name, raw, ends, expect=None):
    if len(raw) > INPUT_LIMIT or not ends or ends[-1] != max(0, len(raw) - TARGETS[name]["selector"]):
        raise ValueError("input over the worker limit or partition does not cover it")
    environment = {"HIRSEL_DECODER_INPUT": raw.hex(),
                   "HIRSEL_DECODER_ENDS": b"".join(struct.pack(">H", end) for end in ends).hex()}
    if expect is not None:
        environment["HIRSEL_DECODER_EXPECT"] = expect.encode().hex()
    return environment


def supervise(command, environment, timeout, cwd, fault=None):
    """Start one child in its own session. Returns (started, exit status, stdout, stderr, timed out, CPU)."""
    if fault == "timeout":
        # The qualification's hung worker: it never reaches the image within the bound.
        command, timeout = ["sh", "-c", 'sleep 30; exec "$@"', "sh", *command], 1
    elif fault == "kill":
        # The qualification's killed worker. It signals itself: a signal from here could
        # land after a short law has finished.
        command = ["sh", "-c", 'kill -KILL $$; exec "$@"', "sh", *command]
    before = child_cpu()
    try:
        child = subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                 env=environment, cwd=cwd, start_new_session=True)
    except OSError as error:
        return False, None, b"", str(error).encode(), False, 0.0
    timed_out = False
    try:
        stdout, stderr = child.communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        timed_out = True
        os.killpg(child.pid, signal.SIGKILL)
        stdout, stderr = child.communicate()
    return True, child.returncode, stdout, stderr, timed_out, child_cpu() - before


def truncate(path):
    """The qualification's partial collection: the run's raw file loses its second half."""
    if path.is_file():
        os.truncate(path, path.stat().st_size // 2)


def new_row():
    return {"started": False, "entered": False, "completed": False, "comparable": False, "status": None,
            "observation": None, "reached": None, "failure": None, "detail": None, "incomplete": None, "cpu": 0.0}


def stopped(row, status, timed_out, timeout):
    """A worker the driver or a signal stopped: a resource violation to confirm, never a pass."""
    if timed_out:
        row["failure"] = "resource: timeout"
        row["detail"] = f"no result within {timeout}s"
    elif status is not None and status < 0:
        row["failure"] = f"resource: signal {-status}"
        row["detail"] = f"worker ended by signal {-status}"
    return row["failure"] is not None


def target(name, *, home, file, scope, control, case=None, law=None):
    """One native target: its owner law file with the adapter appended, the test entry,
    the sites that count (`scope`: definition path prefixes) and the law's body site,
    which marks an attributed case. `home` is ("std", test module) or ("package",
    package, the packages its copy needs beside it). `control` is the accepted reviewed
    case the preparation must pass. `laws` name what an exit's tens digit broke."""
    identifier = name.replace("-", "_")
    entry = f"warm_{identifier}"
    module = f"std@1/tests/{home[1]}/" if home[0] == "std" else f"{home[1]}@1/laws/"
    adapter = DISCOVERY.replace("{hex}", "hex" if home[0] == "std" else "std.hex").replace("{entry}", entry)
    if case is None:
        adapter += HTTP1
        statuses, laws = ("complete", "partial", "bad", "too-large"), {
            2: "mismatch expectation", 3: "mismatch resume_parity", 4: "mismatch partition"}
    else:
        adapter += REVIEWED.replace("{entry}", entry).replace("{case}", case).replace("{name}", law)
        statuses, laws = NATIVE[name]["classes"], {2: "mismatch expectation"}
    return {"home": home, "file": file, "entry": module + entry, "scope": scope, "body": f"{module}{entry}_law#e",
            "adapter": adapter, "statuses": statuses, "laws": laws, "control": control,
            "streaming": case is None or NATIVE[name].get("streaming", False),
            "selector": int(case is not None and "selectors" in NATIVE[name])}


TARGETS = {
    "http1": target("http1", home=("std", "http1"), file="core/std/tests/http1.hirsel", scope=("std@1/http1/",),
                    control="control-get"),
    "json": target("json", home=("std", "json"), file="core/std/tests/json.hirsel", scope=("std@1/json/",),
                   control="accepted-scalars", law="reviewed_json_name",
                   case="let (class, _) = reviewed_json_case(take raw)"),
    "jwt": target("jwt", home=("std", "jwt"), file="core/std/tests/jwt.hirsel", scope=("std@1/jwt/",),
                  control="accepted-rfc7515-a1", law="reviewed_jwt_name",
                  case="let (class, _) = reviewed_jwt_case(take raw)"),
    # The CRYPTO stream's reassembly and framing, then the handshake module's message and
    # ClientHello readers.
    "quic-tls": target("quic-tls", home=("package", "quic", ()), file="core/programs/quic/initial_laws.hirsel",
                       scope=("quic@1/handshake/", "quic@1/quic/receive_crypto", "quic@1/quic/tls_message"),
                       control="accepted-rfc9001-a2", law="reviewed_crypto_name",
                       case="let (class, _, _) = reviewed_crypto_case(raw, ends)"),
    # The first byte selects the surface (fixtures.NATIVE's selectors); without one the
    # law answers class 6.
    "qpack": target("qpack", home=("package", "webtransport", ("quic",)),
                    file="core/programs/webtransport/laws.hirsel", scope=("webtransport@1/qpack/",),
                    control="accepted-rfc9204-b1", law="reviewed_qpack_name",
                    case="""let (class, _, _) = if list.len(raw) == 0 {
        (6u8, b"", 0u64)
    } else {
        reviewed_qpack_case(raw[0], list.slice(raw, 1, list.len(raw)), ends)
    }"""),
}
WARM_GREETING = b"HIRSELWM"
WARM_CASE, WARM_SAMPLE, WARM_FINISH = 1, 2, 3
WARM_COUNTERS = ("heap_held", "heap_arena", "maps", "resident_pages", "descriptors", "threads")


def executable_name(entry):
    """hirsel test names an executable by the framed hash of its definition."""
    parts = (b"hirsel.text.v1", entry.encode())
    return hashlib.sha256(b"".join(len(part).to_bytes(8, "little") + part for part in parts)).hexdigest()


def stage(name, work):
    """Write the target's law file with its adapter under `work`. Returns what `hirsel test` reads."""
    target = TARGETS[name]
    kind, owner, *rest = target["home"]
    if kind == "std":
        source = fixture_source(ROOT / target["file"]) + target["adapter"]
        staged = work / f"{owner}.hirsel"
        staged.write_text(source)
        inputs = work / "inputs.json"
        inputs.write_text(json.dumps(source_manifest(ROOT, "std", ["tests", owner], staged)))
        return ["--inputs", str(inputs), "--source-core", "--source-std", "--limit", str(64 << 20),
                "--grant", "Env=" + ",".join(VARIABLES)]
    # A package's tests take their grants from its manifest: the copy's adds the Env grant.
    members = (owner, *rest[0])
    packages = work / "packages"
    for member in members:
        (packages / member).mkdir(parents=True, exist_ok=True)
        for path in (ROOT / "core/programs" / member).iterdir():
            if path.suffix == ".hirsel" or path.name == "hirsel.hcfg":
                (packages / member / path.name).write_bytes(path.read_bytes())
    (packages / "hirsel.hcfg").write_text(
        "pkg_v1.Workspace(\n    members: [\n" + "".join(f'        "{member}",\n' for member in members) + "    ],\n)\n")
    (packages / owner / Path(target["file"]).name).write_text((ROOT / target["file"]).read_text() + target["adapter"])
    manifest = packages / owner / "hirsel.hcfg"
    declared, tests, grants = manifest.read_text().partition("tests: Some(Tests(")
    before, found, after = grants.partition("grants: [")
    if not found:
        raise ValueError(f"{manifest}: the tests declare no grants")
    grant = "Env([" + ", ".join(f'"{variable}"' for variable in VARIABLES) + "]), "
    manifest.write_text(declared + tests + before + found + grant + after)
    return [str(packages / owner), "--libraries", str(ROOT / "core")]


def prepare_warm(compiler, runtime, work, name, backend="cranelift"):
    """Build the target's covered image. Returns it.
    Cranelift by default: its warm HTTP/1 search completes more cases per CPU-second than LLVM's."""
    target = TARGETS[name]
    work = work.resolve()
    work.mkdir(parents=True, exist_ok=True)
    programs = work / "programs"
    case = reviewed_cases(name)[target["control"]]
    control = case_environment(name, case["input"], case["schedule"])
    command = [str(compiler), "test", *stage(name, work), "--backend", backend, "--runtime", str(runtime),
               "--store", str(work / "store"), "--json", "--coverage", "--rerun", "--test-timeout", "60",
               "--exact", target["entry"], "-o", str(programs)]
    started, wall = child_cpu(), time.monotonic()
    built = subprocess.run(command, capture_output=True, text=True, env={**os.environ, **control}, cwd=work)
    cpu, wall = child_cpu() - started, time.monotonic() - wall
    try:
        tests = {row["definition"]: row for row in json.loads(built.stdout)["tests"]}
    except (ValueError, KeyError, TypeError):
        raise ValueError(f"preparation exited {built.returncode}: {built.stdout[-2000:]} {built.stderr[-2000:]}")
    if not tests.get(target["entry"], {}).get("passed"):
        raise ValueError(f"preparation must pass the {name} law's control: {tests}")
    executable = programs / executable_name(target["entry"])
    if not executable.is_file():
        raise ValueError(f"preparation produced no executable for {target['entry']}")
    image = {"target": name, "executable": str(executable), "map": f"{executable}.covmap", "backend": backend,
             "prepare_cpu": cpu, "prepare_wall": wall,
             **{key: target[key] for key in ("entry", "scope", "body", "statuses", "laws")}}
    sites = warm_sites(image)
    if target["body"] not in sites["all"] or not sites["decoder"]:
        raise ValueError(f"{name}: coverage map has no target site or no law body")
    image["build"], image["decoder_sites"] = sites["build"], len(sites["decoder"])
    return image


def warm_sites(image):
    return site_map(Path(image["map"]), image["entry"], image["scope"])


def warm_variables(image, case):
    return case_environment(image["target"], case["input"], case["schedule"], case.get("expect"))


def warm_status(image, status, row):
    """Fill a completed run's status and failure from the law's exit status."""
    names, laws = image["statuses"], image["laws"]
    tens, k = divmod(status, 10) if 0 <= status < 50 else (None, None)
    if status == 0:
        row["status"] = names[0]
    elif tens == 1 and k < len(names):
        row["status"] = names[k]
    elif tens in laws and k < len(names):
        row["status"] = names[k]
        row["failure"] = f"{laws[tens]}: observed {names[k]}"
    elif status == 30:
        row["status"], row["failure"] = "law", "mismatch owner law"
    else:
        row["failure"] = f"exit {status}"
    row["observation"] = f"observed {row['status']}"


def fresh(image, sites, case, raw_file, *, timeout=20, cwd=None, fault=None):
    """One input in a fresh child of a warm law's image: the comparison and the replay."""
    environment = {**warm_variables(image, case), "HIRSEL_COVERAGE": str(raw_file)}
    if fault == "zero-bodies":
        del environment["HIRSEL_DECODER_INPUT"]
    row = new_row()
    row["started"], status, _stdout, stderr, timed_out, row["cpu"] = supervise(
        [image["executable"]], environment, timeout, cwd, fault)
    if not row["started"]:
        row["incomplete"] = "worker did not start: " + stderr.decode(errors="replace")
        return row
    stop = stopped(row, status, timed_out, timeout)
    if fault == "partial":
        truncate(Path(raw_file))
    reached, outcome, unobserved = read_raw(Path(raw_file), sites["records"])
    if unobserved:
        row["incomplete"] = unobserved
        return row
    row["entered"] = image["body"] in reached
    row["reached"] = sorted(site for site in reached if site.startswith(image["scope"]))
    if stop:
        return row
    if not row["entered"] or outcome not in (1, 2):
        row["incomplete"] = "zero bodies" if not row["entered"] else "run recorded no outcome"
        return row
    row["completed"], row["exit"] = True, status
    warm_status(image, status, row)
    row["comparable"] = row["status"] is not None
    return row


class WorkerLost(Exception):
    """The warm worker ended or overran its bound before it answered a case."""


class Warm:
    """One warm worker: a covered image serving cases over two pipes.

    The worker maps its raw file once. Each case clears the flags of the
    records that hold a target site or the law's body, runs the law on fresh
    case state and answers with the exit status and those flags. A lost
    worker answers nothing for its case: the driver saved the case before it
    asked, replays it in a fresh child, and starts another worker.
    """

    def __init__(self, image, sites, raw_file, *, cwd=None):
        self.image, self.raw_file = image, Path(raw_file)
        self.total, self.charged = None, 0.0
        requests, self._request = os.pipe()
        self._reply, replies = os.pipe()
        environment = {"HIRSEL_COVERAGE": str(raw_file), "HIRSEL_COVERAGE_WARM": f"{requests},{replies}"}
        self.child = subprocess.Popen([image["executable"]], stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                      stderr=subprocess.DEVNULL, env=environment, cwd=cwd, pass_fds=(requests, replies),
                                      start_new_session=True)
        os.close(requests)
        os.close(replies)
        try:
            greeting = self._read(16, 20)
            if greeting[:8] != WARM_GREETING or struct.unpack_from("<I", greeting, 8)[0] != 1:
                raise WorkerLost("the worker's greeting is not warm feedback version 1")
            count = struct.unpack_from("<I", greeting, 12)[0]
            layout = self._read(40 * count, 20)
        except WorkerLost:
            self.kill()
            raise
        selected, self.flat = [], []
        for index in range(count):
            identity, _offset, sites_in = struct.unpack_from("<32sII", layout, 40 * index)
            record = sites["records"].get(identity)
            if record is None or len(record) != sites_in:
                self.kill()
                raise WorkerLost("a worker record matches no record of the site map")
            if any(site.startswith(image["scope"]) or site == image["body"] for site in record):
                selected.append(index)
                self.flat += record
        self.width = len(self.flat)
        self._write(struct.pack(f"<I{len(selected)}I", len(selected), *selected))

    def _write(self, data):
        try:
            os.write(self._request, data)
        except (BrokenPipeError, OSError) as error:
            raise WorkerLost(f"request refused: {error}")

    def _read(self, size, timeout):
        data = bytearray()
        deadline = time.monotonic() + timeout
        while len(data) < size:
            left = deadline - time.monotonic()
            if left <= 0 or not select.select([self._reply], [], [], left)[0]:
                self.kill()
                raise WorkerLost(f"no answer within {timeout}s")
            chunk = os.read(self._reply, size - len(data))
            if not chunk:
                raise WorkerLost(f"worker ended: {self.reap()}")
            data += chunk
        return bytes(data)

    def call(self, variables, timeout=20):
        """Run one case. Returns the law's exit status and the selected flags."""
        payload = b"".join(f"{key}={value}".encode() + b"\0" for key, value in variables.items())
        payload += b"".join(key.encode() + b"\0" for key in VARIABLES if key not in variables)
        self._write(struct.pack("<II", WARM_CASE, len(payload)) + payload)
        reply = self._read(8 + self.width, timeout)
        tag, status = struct.unpack_from("<Ii", reply)
        if tag != WARM_CASE:
            raise WorkerLost("the worker answered another request")
        return status, reply[8:]

    def reached(self, flags):
        return [self.flat[match.start()] for match in NONZERO.finditer(flags)]

    def run(self, case, timeout=20):
        """One case as a row of the fresh worker's shape. Raises WorkerLost."""
        row = new_row()
        row["started"] = True
        status, flags = self.call(warm_variables(self.image, case), timeout)
        # Two records can hold one site name (a generic's instances): the reach is a set, as a raw file's is.
        reached = set(self.reached(flags))
        row["entered"] = self.image["body"] in reached
        row["reached"] = sorted(site for site in reached if site.startswith(self.image["scope"]))
        if not row["entered"]:
            row["incomplete"] = "zero bodies"
            return row
        row["completed"], row["exit"] = True, status
        warm_status(self.image, status, row)
        row["comparable"] = row["status"] is not None
        return row

    def sample(self):
        self._write(struct.pack("<I", WARM_SAMPLE))
        tag, count = struct.unpack("<II", self._read(8, 20))
        return dict(zip(WARM_COUNTERS, struct.unpack(f"<{count}Q", self._read(8 * count, 20))))

    def cpu(self):
        """The worker's CPU seconds so far: its rusage once reaped."""
        if self.total is not None:
            return self.total
        try:
            fields = Path(f"/proc/{self.child.pid}/stat").read_text().rsplit(")", 1)[1].split()
        except OSError:
            return self.charged
        return (int(fields[11]) + int(fields[12])) / TICKS

    def spent(self):
        """The worker's CPU seconds since the last call."""
        now = max(self.cpu(), self.charged)
        spent, self.charged = now - self.charged, now
        return spent

    def finish(self):
        """End the worker cleanly; its raw file then holds every completed case's flags."""
        try:
            self._write(struct.pack("<I", WARM_FINISH))
        except WorkerLost:
            pass
        os.close(self._request)
        status = self.reap()
        os.close(self._reply)
        return status

    def reap(self):
        try:
            self._wait(20)
        except subprocess.TimeoutExpired:
            self.kill()
        return self.child.returncode

    def kill(self):
        if self.child.returncode is None:
            os.killpg(self.child.pid, signal.SIGKILL)
            self._wait(None)

    def _wait(self, timeout):
        # Only this wait reaps the worker: the children's rusage it adds is the worker's.
        before = child_cpu()
        self.child.wait(timeout=timeout)
        if self.total is None:
            self.total = child_cpu() - before


TICKS = os.sysconf("SC_CLK_TCK")

NONZERO = re.compile(rb"[^\x00]")
