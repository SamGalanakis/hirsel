"""Unshielded, Hirsel-only rows for the nightly board."""
import csv
from decimal import Decimal
import io
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile

import board_host
import build
import receipt
from measurement_artifacts import build_outputs
from measure import sample
import store_bench
import store_disk_bench

ROOT = build.ROOT
sys.path.insert(0, str(ROOT))
from bench.multicore import build as multicore_build, run as multicore
from bench.srtp import cost as srtp
from bench.turn import cost as turn

CAP = ROOT / "tools/capped"
JOBS = 4
BUNDLE = "//:hirsel-toolchain"
NOTES_ORIGIN = "https://github.com/SamGalanakis/lash-hirsel.git"
NOTES_REV = "f8d230c3165f2caa17ed74eab812404e493e60b8"
NOTES_CONFIG = Path(__file__).resolve().with_name("notes-config")
NOTES_EDIT = ('emit(out, "you: ", prompt)', 'emit(out, "You: ", prompt)')
# The default allocator arenas hit the 2 GiB admission limit at four jobs.
LOW_MEMORY_ALLOCATOR = {"MIMALLOC_DISALLOW_ARENA_ALLOC": "1", "MIMALLOC_PURGE_DELAY": "0", "MALLOC_ARENA_MAX": "2"}
ALLOCATIONS = "allocations_per_packet"
INSTRUCTIONS = "instructions_per_packet"
CHURN = "Place churn"
CHURN_ROW = "1,000 wake/refuse cycles"
RESOURCES = tuple(f"{process}_{metric}" for process in ("launcher", "platform")
                  for metric in ("fds", "threads"))
EXPECTED = {
    "RTP": ("parse/rewrite",),
    "SRTP": tuple(f"{profile}/{operation}" for profile in srtp.PROFILES
                  for operation in (srtp.ALL_OPERATIONS if profile == "gcm128" else srtp.OPERATIONS)),
    "TURN": tuple(f"{op}/{live} live" for op in turn.OPERATIONS for live in (1, turn.ALLOCATIONS_WIDE)),
    "Memory Store": tuple(f"{profile}/{phase}" for profile in store_bench.PROFILES for phase in store_bench.EXPECTED),
    "Disk Store": tuple(f"{profile}/{phase}/i{index}" for profile in store_disk_bench.PROFILES
                        for phase in store_disk_bench.PHASES for index in store_disk_bench.phase_indexes(phase)),
    "Multicore S0": tuple(multicore.S0_SIZES),
    "Notes release": ("cold", "edit"),
    CHURN: (CHURN_ROW,),
}
NOTE = ("The unshielded tier gates instructions at +2%, Notes peak RSS at +5%, packet allocations at zero, "
        "and launcher and platform FDs and threads at their starting counts. Notes patch work and wall time are trends.")


def fields_for(goal, row, values):
    if goal == "RTP" or (goal == "SRTP" and row.split("/")[1] in srtp.OPERATIONS) or (
            goal == "TURN" and row.endswith("/1 live")):
        return (ALLOCATIONS, INSTRUCTIONS)
    if goal in ("SRTP", "TURN"):
        return (INSTRUCTIONS,)
    if goal in ("Memory Store", "Disk Store", "Multicore S0"):
        return ("instructions", "seconds")
    if goal == "Notes release":
        return ("instructions", "peak_rss_kib", "seconds",
                *sorted(name for name in values if name.startswith("work/")))
    return RESOURCES


def metric_valid(name, value):
    return (type(value) in (float, int) and 0 <= value < float("inf")
            and (value > 0 or name == ALLOCATIONS or name.startswith("work/")))


def add(board, goal, row, values):
    fields = fields_for(goal, row, values)
    board.row(goal, row, {"hirsel": {"status": "ok", **values}}, fields)


def limit_for(metric, sample, previous):
    if metric == ALLOCATIONS:
        return 0
    if metric in RESOURCES:
        return sample["starting"][metric]
    if metric in (INSTRUCTIONS, "instructions", "peak_rss_kib") and previous is not None:
        increase = Decimal("1.05") if metric == "peak_rss_kib" else Decimal("1.02")
        return Decimal(str(previous[metric])) * increase
    return None


def compare(report, baseline=None):
    prior = {}
    current = [row for row in report["rows"] if row["goal"] in EXPECTED]
    if baseline is not None:
        prior = {(row["goal"], row["row"]): row for row in baseline["rows"] if row["goal"] in EXPECTED}
        # An existing nightly baseline can acquire the whole tier on its next clean run.
        if prior and (prior.keys() != {(goal, name) for goal, names in EXPECTED.items() for name in names}
                      or baseline["counter_identity"] != report["counter_identity"]):
            raise ValueError("nightly counter baseline has a different workload or host identity")
    gates = []
    for row in current:
        if row["status"] != "ok":
            continue
        goal, name = row["goal"], row["row"]
        sample = row["samples"]["hirsel"]
        previous = prior.get((goal, name))
        if previous:
            if previous["status"] != "ok":
                raise ValueError(f"{goal}/{name}: incomplete nightly counter baseline")
            previous = previous["samples"]["hirsel"]
        for metric in row["metrics"]:
            if metric == "seconds" or metric.startswith("work/"):
                continue
            relative = metric in (INSTRUCTIONS, "instructions", "peak_rss_kib")
            if relative and previous is not None and not metric_valid(metric, previous[metric]):
                raise ValueError(f"{goal}/{name}/{metric}: invalid nightly counter baseline")
            limit = limit_for(metric, sample, previous)
            value = sample[metric]
            if limit is None:
                status = "baseline-needed"
            else:
                status = "fail" if Decimal(str(value)) > limit else "pass"
            gates.append(dict(goal=goal, row=name, metric=metric, value=value,
                              limit=float(limit) if limit is not None else None, status=status))
    return gates


def identity():
    return {"cpu_model": board_host.facts(())["cpu_model"], "jobs": JOBS, "notes_revision": NOTES_REV,
            "notes_allocator": LOW_MEMORY_ALLOCATOR, "notes_edit": list(NOTES_EDIT), "version": 1}


def run_script(path, out, env, *args, allow_gate_fail=False):
    out.parent.mkdir(parents=True, exist_ok=True)
    with out.with_suffix(".log").open("w") as log:
        result = subprocess.run([sys.executable, ROOT / path, *args], cwd=ROOT, env=env,
                                stdout=subprocess.PIPE, stderr=log, text=True)
    out.with_suffix(".stdout").write_text(result.stdout)
    if result.returncode not in ((0, 1) if allow_gate_fail else (0,)):
        raise RuntimeError(f"{path}: exit {result.returncode}. See {out.with_suffix('.log')}")
    return result.stdout


def rtp_row(board, out, env):
    script = "bench/rtp/cost.py"
    run_script(script, out, env, out, "--build-only")
    values = {}
    for mode, metric in (("allocations", ALLOCATIONS), ("instructions", INSTRUCTIONS)):
        output = run_script(script, out.with_name(f"rtp-{mode}"), env,
                            out, "--skip-build", "--mode", mode, allow_gate_fail=mode == "allocations")
        key, value = output.strip().split()
        if key != metric:
            raise ValueError(f"RTP/{mode}: missing {metric}")
        values[key] = float(value)
    add(board, "RTP", EXPECTED["RTP"][0], values)


def media_rows(board, out, env, goal, *options):
    name = goal.lower()
    script = f"bench/{name}/cost.py"
    run_script(script, out.with_name(f"{name}-build"), env, out, *options, "--build-only")
    output = run_script(script, out, env, out, *options, "--skip-build", allow_gate_fail=True)
    for row in csv.DictReader(io.StringIO(output)):
        label = (f"{row['profile']}/{row['operation']}" if goal == "SRTP"
                 else f"{row['operation']}/{row['allocations_live']} live")
        add(board, goal, label, {key: float(row[key]) for key in (ALLOCATIONS, INSTRUCTIONS) if row[key]})


def store_rows(board, out, env, artifacts, goal, name, *options):
    # The stores are phase fixtures, with no requirement to measure real disk latency.
    with tempfile.TemporaryDirectory(prefix="hirsel-board-store-", dir="/dev/shm") as scratch:
        work = Path(scratch)
        script = f"core/tools/{name}.py"
        flags = ("--out", work, "--artifacts", artifacts, "--unshielded", *options)
        run_script(script, out.with_name(f"{out.name}-build"), env, *flags, "--prepare")
        run_script(script, out, env, *flags, "--measure", allow_gate_fail=True)
        samples = receipt.read(work / "samples.json")
        receipt.write(out.with_suffix(".json"), samples)
    for row in samples:
        label = f"{row['profile']}/{row['workload']}"
        if goal == "Disk Store":
            label += f"/i{row['indexes']}"
        if not row["complete"] or row["instruction_active_percent"] != 100:
            board.row(goal, label, {"hirsel": {"status": row["censored"] or "inactive-counter"}},
                      fields_for(goal, label, {}))
        else:
            add(board, goal, label, {"instructions": row["instructions"], "seconds": row["wall_ns"] / 1e9})


def multicore_rows(board, out, env, artifacts):
    bins = out / "bins"
    bins.mkdir(parents=True)
    if not multicore_build.build_hirsel(bins, artifacts["compiler"], artifacts["runtime"],
                                       "llvm", s0=True, jobs=JOBS):
        raise RuntimeError(f"Multicore S0 build failed. See {bins}")
    run_script("bench/multicore/run.py", out, env, "--bins", bins, "--out", out / "samples",
               "--s0", "--languages", "Hirsel", "--unshielded", allow_gate_fail=True)
    for row in receipt.read(out / "samples/samples.json"):
        if row["status"] == "ok" and row["instruction_active_percent"] == 100:
            add(board, "Multicore S0", row["case"], {"instructions": row["instructions"], "seconds": row["seconds"]})
        else:
            status = "inactive-counter" if row["status"] == "ok" else row["status"]
            board.row("Multicore S0", row["case"], {"hirsel": {"status": status}}, ("instructions", "seconds"))


def notes_source(out):
    with tempfile.TemporaryDirectory(prefix="ref-lash-hirsel-") as checkout:
        subprocess.run(["git", "clone", "--quiet", "--no-checkout", NOTES_ORIGIN, checkout], check=True)
        archive = subprocess.check_output(["git", "-C", checkout, "archive", NOTES_REV])
    with tarfile.open(fileobj=io.BytesIO(archive)) as tree:
        tree.extractall(out, filter="data")
    # This revision predates typed package manifests. Declare the Notes target
    # and its harness library, without the separate operator publisher and tools.
    shutil.copytree(NOTES_CONFIG, out, dirs_exist_ok=True)
    (out / "hirsel.lock").unlink()
    # The pinned app predates Store's explicit limits. None retains the defaults.
    source = out / "notes/main.hirsel"
    edit_notes(source, "pager_task_bytes: None, readers: None)",
               "pager_task_bytes: None, readers: None, limits: None)")
    edit_notes(out / "harness/gateway.hirsel", "platform.local.Config(root: root, options:",
               "platform.local.Config(root: root, backing: DiskBacking, options:")
    edit_notes(out / "harness/gateway.hirsel", "net: net, watch: take watch)",
               "net: net, watch: take watch, journal: None)")


def edit_notes(source, old, new):
    before = source.read_text()
    if before.count(old) != 1:
        raise ValueError(f"Notes fixture edit no longer matches: {old}")
    source.write_text(before.replace(old, new))


def notes_phase(board, out, env, toolchain, phase, cold_parts=None):
    source = out / "source"
    notes_env = env | LOW_MEMORY_ALLOCATOR | {"XDG_CACHE_HOME": str(out / "cache"), "XDG_DATA_HOME": str(out / "xdg"),
                                         "HIRSEL_DAEMON": "off"}
    image = out / "notes"
    metrics = out / f"{phase}-metrics.json"
    target = out / phase
    command = [toolchain / "hirsel", "build", "notes", "--release", "--json",
               "--limit", "268435456", "--dir", out / "data", "--grant", "Stdout",
               "--grant", "Clock", "--grant", "Random", "--grant", "Tasks",
               "--grant", "Net=Listen 127.0.0.0/8 0-65535;Connect 127.0.0.0/8 0-65535",
               "--metrics", metrics, "--store", out / "work", "-o", image]
    result = sample(command, target, compiler_only=True, env=notes_env, cwd=source,
                    pmu_times=True, unshielded=True)
    if result["status"] != "ok":
        raise ValueError(f"Notes {phase}: {result['status']}. See {target}")
    if not receipt.counter_ready(result, "instructions:u"):
        raise ValueError(f"Notes {phase}: inactive instruction counter. See {target}")
    build_result = build.hirsel_build_evidence(target / "stdout", False, None, image, source)
    if not build_result["rebuilt"] or build_result["release_std_lowered"] != 0:
        raise ValueError(f"Notes {phase}: expected a fresh link, compiled parts and shipped std. See {target}")
    counts = receipt.read(metrics)["counters"]
    if not counts or any(type(value) is not int or value < 0 for value in counts.values()):
        raise ValueError(f"Notes {phase}: invalid work counters")
    parts = build_result["compiled_parts"]
    values = {"instructions": result["instructions"], "peak_rss_kib": result["max_rss_kib"],
              "seconds": float(result["seconds"]), "work/compiled_parts": parts,
              **{f"work/{key}": value for key, value in counts.items()}}
    if cold_parts is not None:
        values["work/edit_parts_pct_of_cold"] = 100 * parts / cold_parts
    add(board, "Notes release", phase, values)
    return parts


def notes_rows(board, out, env, toolchain):
    source = out / "source"
    notes_source(source)
    (out / "data").mkdir()
    cold_parts = notes_phase(board, out, env, toolchain, "cold")
    edit_notes(source / "notes/main.hirsel", *NOTES_EDIT)
    (out / "notes").unlink()
    notes_phase(board, out, env, toolchain, "edit", cold_parts)


def churn_row(board, out, env, toolchain):
    result = out.with_suffix(".json")
    # Genesis assembles the place inside this same 2 GiB scope.
    env = env | LOW_MEMORY_ALLOCATOR
    with out.with_suffix(".log").open("w") as log:
        subprocess.run([CAP, sys.executable, ROOT / "bench/place-density/run.py",
                        "--hirsel", toolchain / "hirsel", "--churn", result], cwd=ROOT,
                       env=env, stdout=log, stderr=subprocess.STDOUT, check=True)
    raw = receipt.read(result)
    if raw["cycles"] != 1000:
        raise ValueError("Place churn did not complete 1,000 cycles")
    start, end = ({f"{process}_{metric}": value
                   for process, counts in zip(("launcher", "platform"), raw[tag], strict=True)
                   for metric, value in counts.items()} for tag in ("before", "after"))
    if start.keys() != set(RESOURCES) or not all(type(value) is int and value > 0 for value in start.values()):
        raise ValueError("Place churn missing valid resource starting counts")
    add(board, CHURN, CHURN_ROW, end | {"starting": start, "cycles": raw["cycles"]})


def collect(board, artifact_file, artifacts):
    out = board.out / "counters"
    out.mkdir()
    env = build.capped_env(out, "2G", str(JOBS)) | {"LC_ALL": "C"}
    board.report["counter_identity"] = identity()
    board.attempt("RTP counters", rtp_row, board, out / "rtp", env)
    board.attempt("SRTP counters", media_rows, board, out / "srtp", env, "SRTP", "--mode", "both")
    board.attempt("TURN counters", media_rows, board, out / "turn", env, "TURN", "--counters-only")
    board.attempt("Memory Store counters", store_rows, board, out / "store", env,
                  artifact_file, "Memory Store", "store_bench", "--only-language", "hirsel")
    board.attempt("Disk Store counters", store_rows, board, out / "store-disk", env,
                  artifact_file, "Disk Store", "store_disk_bench")
    board.attempt("Multicore S0 counters", multicore_rows, board, out / "s0", env, artifacts)
    toolchain = board.attempt("Counter toolchain build", lambda: build_outputs([BUNDLE], threads=JOBS)[BUNDLE])
    if toolchain:
        board.attempt("Notes release counters", notes_rows, board, out / "notes", env, toolchain)
        board.attempt("Place churn counters", churn_row, board, out / "place-churn", env, toolchain)
