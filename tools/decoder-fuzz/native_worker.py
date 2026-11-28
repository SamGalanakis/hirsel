#!/usr/bin/env python3
"""Run one HTTP/1 input per fresh covered native child (docs/coverage.md, "The raw file").

`prepare` builds std's http1 contract module with std given as source and
`hirsel test --coverage`, once per build. It keeps the test executable and its
`.covmap`. `run` starts one child for one input. The child maps its flags over
a raw file that no other run names. Only sites of the decoder module count.
"""
import hashlib
import json
import os
from pathlib import Path
import resource
import signal
import struct
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "core/tools"))
sys.path.insert(0, str(Path(__file__).resolve().parent))
from contract_exec import source_manifest
from fixtures import partition

TARGET = "http1"
SCOPE = "std@1/http1/"
ENTRY = "std@1/tests/http1/native_contract"
BODY = "std@1/tests/http1/discovered_request#e"
INPUT_LIMIT = 4096
STATUSES = ("complete", "partial", "bad", "too-large")
ACCEPTED = "complete"
CONTROLS = ("control-get", "control-get-leading-crlf")
VARIABLES = ("HIRSEL_DECODER_INPUT", "HIRSEL_DECODER_ENDS", "HIRSEL_DECODER_EXPECT")
RAW_MAGIC = b"HIRSELCV"
RAW_END = 0x45444F4E
RECORD_MAGIC = 0x56434948

# The owner predicates are the module's own: resume_parity (every two-way split
# and the byte drip) and same_head. An absent expectation checks them alone.
ADAPTER = '''
fn discovery_bytes(e: Env, name: String) -> Option[Bytes] {
    match env.get(e, name) {
        Ok(Some(text)) => hex.decode(string.as_bytes(text)),
        _ => None
    }
}

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

fn discovered_request(raw: Bytes, ends: Bytes, expect: Option[Bytes], mut line: String) -> bool {
    let mut whole = http1.head()
    let progress = http1.request(raw, mut whole)
    string.push(mut line, "observed ")
    string.push(mut line, discovery_name(progress))
    string.push(mut line, " end=")
    string.push_u64(mut line, whole.end)
    string.push(mut line, " fields=")
    string.push_u64(mut line, whole.count)
    string.push(mut line, "\\n")
    if let Some(name) = expect {
        if name != string.as_bytes(discovery_name(progress)) {
            string.push(mut line, "mismatch expectation\\n")
            return false
        }
    }
    if !resume_parity(raw, None) {
        string.push(mut line, "mismatch resume_parity\\n")
        return false
    }
    if !discovered_partition(raw, ends, progress, whole) {
        string.push(mut line, "mismatch partition\\n")
        return false
    }
    true
}

@test
pub fn native_contract(native_env: Env, native_out: Stdout) -> Result[(), ()] waits {
    let Some(raw) = discovery_bytes(native_env, "HIRSEL_DECODER_INPUT") else {
        return Error(())
    }
    let Some(ends) = discovery_bytes(native_env, "HIRSEL_DECODER_ENDS") else {
        return Error(())
    }
    let expect = discovery_bytes(native_env, "HIRSEL_DECODER_EXPECT")
    let mut line = string.new()
    let passed = discovered_request(raw, ends, expect, mut line)
    result.unwrap(stdout.write(native_out, line))
    if passed {
        Ok(())
    } else {
        Error(())
    }
}
'''


def child_cpu():
    used = resource.getrusage(resource.RUSAGE_CHILDREN)
    return used.ru_utime + used.ru_stime


def prepare(compiler, runtime, work, backend="llvm"):
    """Build the covered image once. Returns the build's executable, map and decoder sites."""
    work.mkdir(parents=True, exist_ok=True)
    source = work / "http1.hirsel"
    source.write_text((ROOT / "core/std/tests/http1.hirsel").read_text() + ADAPTER)
    inputs = work / "inputs.json"
    inputs.write_text(json.dumps(source_manifest(ROOT, "std", ["tests", "http1"], source)))
    programs = work / "programs"
    control = case_environment(*reviewed_control(CONTROLS[0]))
    started = child_cpu()
    built = subprocess.run(
        [str(compiler), "test", "--inputs", str(inputs), "--source-core", "--source-std", "--backend", backend,
         "--runtime", str(runtime), "--store", str(work / "store"), "--json", "--limit", str(64 << 20),
         "--coverage", "--rerun", "--exact", ENTRY, "--grant", "Env=" + ",".join(VARIABLES), "--grant", "Stdout",
         "--test-timeout", "60", "-o", str(programs)],
        capture_output=True, text=True, env={**os.environ, **control}, cwd=work)
    try:
        tests = json.loads(built.stdout)["tests"]
    except (ValueError, KeyError, TypeError):
        raise ValueError(f"preparation exited {built.returncode}: {built.stdout[-2000:]} {built.stderr[-2000:]}")
    if [row.get("definition") for row in tests] != [ENTRY] or not tests[0].get("passed"):
        raise ValueError(f"preparation must execute exactly {ENTRY} and pass its control: {tests}")
    # hirsel test names an executable by the framed hash of its definition.
    parts = (b"hirsel.text.v1", ENTRY.encode())
    executable = programs / hashlib.sha256(
        b"".join(len(part).to_bytes(8, "little") + part for part in parts)).hexdigest()
    if not executable.is_file():
        raise ValueError(f"preparation produced no executable at {executable}")
    image = {"target": TARGET, "executable": str(executable), "map": f"{executable}.covmap", "backend": backend,
             "prepare_cpu": child_cpu() - started}
    sites = site_map(Path(image["map"]))
    if BODY not in sites["all"] or not sites["decoder"]:
        raise ValueError("coverage map has no decoder site or no discovery body")
    image["build"] = sites["build"]
    image["decoder_sites"] = len(sites["decoder"])
    return image


def site_map(path):
    """The map's records by id. A missing or foreign map is refused here, before any run."""
    document = json.loads(path.read_text())
    if document.get("hirsel_covmap") != 2 or document.get("test") != ENTRY:
        raise ValueError(f"{path}: not the format 2 site map of {ENTRY}")
    records = {bytes.fromhex(record["id"]): [site["site"] for site in record["sites"]]
               for record in document["records"]}
    every = {site for sites in records.values() for site in sites}
    return {"records": records, "all": every, "build": document["build"],
            "decoder": sorted(site for site in every if site.startswith(SCOPE))}


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


def reviewed_control(case_id):
    bundle = json.loads((ROOT / "core/tests/decoder-corpus/http1.json").read_text())
    case = next(case for case in bundle["cases"] if case["id"] == case_id)
    raw = bytes.fromhex(bundle["objects"][case["digest"]])
    return raw, partition(case["schedules"][0], len(raw))


def schedule(raw, seed):
    """A generated legal partition: the reviewed bundle's generator over this input."""
    return partition({"seed": seed & 0xffffffff, "max_chunk": max(1, min(len(raw), 1 + seed % 7))}, len(raw))


def case_environment(raw, ends, expect=None):
    if len(raw) > INPUT_LIMIT or not ends or ends[-1] != len(raw):
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
    before = child_cpu()
    try:
        child = subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                 env=environment, cwd=cwd, start_new_session=True)
    except OSError as error:
        return False, None, b"", str(error).encode(), False, 0.0
    if fault == "kill":
        os.killpg(child.pid, signal.SIGKILL)
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


def run(image, sites, case, raw_file, *, timeout=20, cwd=None, fault=None):
    """Run one input in a fresh child. `raw_file` must not exist: the runtime creates it."""
    raw, ends = case["input"], case["schedule"]
    environment = {**case_environment(raw, ends, case.get("expect")), "HIRSEL_COVERAGE": str(raw_file)}
    if fault == "zero-bodies":
        del environment["HIRSEL_DECODER_INPUT"]
    row = new_row()
    row["started"], status, stdout, stderr, timed_out, row["cpu"] = supervise(
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
    row["entered"] = BODY in reached
    row["reached"] = sorted(site for site in reached if site.startswith(SCOPE))
    if stop:
        return row
    if not row["entered"] or outcome not in (1, 2):
        row["incomplete"] = "zero bodies" if not row["entered"] else "run recorded no outcome"
        return row
    row["completed"] = True
    lines = stdout.decode(errors="replace").splitlines()
    observed = lines[0].split() if lines else []
    if len(observed) == 4 and observed[0] == "observed" and observed[1] in STATUSES:
        row["status"], row["observation"] = observed[1], lines[0]
    if outcome == 2 or status != 0:
        law = next((line for line in lines[1:] if line.startswith("mismatch ")), None)
        trap = stderr.decode(errors="replace").strip().splitlines()
        row["failure"] = f"{law}: observed {row['status']}" if law else trap[0] if trap else f"exit {status}"
        row["detail"] = "\n".join(lines + trap)
    elif row["status"] is None:
        row["incomplete"] = "passing run printed no observation"
        row["completed"] = False
        return row
    row["comparable"] = True
    return row
