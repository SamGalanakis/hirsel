#!/usr/bin/env python3
"""Build, run and record the matched Hirsel and Tokio comparison."""
import argparse
from contextlib import contextmanager
from datetime import datetime, timedelta
from functools import partial
import hashlib
from itertools import count
import json
import os
from pathlib import Path
import signal
from statistics import median
import subprocess
import sys
import threading
import time

import scale
from light import pick_ports

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(ROOT / "core/tools"))
sys.path.insert(0, str(ROOT / "bench/awfy"))
from receipt import measurement_slot, read, release_tools, write
from bench_cpus import cpu_list

from bench.multicore import build, regimes, run

CPU_DEFAULTS = {
    "server": (0, 8, 1, 9, 2, 10, 3, 11),
    "load": (16, 17, 18, 19),
    "helper": (24, 25),
    "controller": (26, 27),
}
MEASURE_CPUS = set().union(*CPU_DEFAULTS.values())
BUILD_CPUS = "8-11"
MATCHED_OFFER = 3000
CONNECTIONS = (256, 2048, 8192)
QUICK_REGIMES = ("fanin", "pipeline")
ROUNDS = dict(zip(regimes.GRAINS, (0, 64, 640, 6400, 64000), strict=True))
REGIMES = tuple(name for name in regimes.WORKLOADS if name != "calibrate")
VERSION = 1
ARMS = ("hirsel", "tokio")
A1_SIDES = ("untied", "tokiow8")
A1_BINARIES = ("a1/bin/session_u", "a1/bin/twin", "a1/bin/loadgen",
               "a1/bin/toolsrv", "a1/software/twin")
REGIME_BINARIES = ("regimes/hirsel/regimes", "regimes/rust/regimes")
CHECKBOX_BINARIES = ("regimes/checkbox/checkbox", "regimes/checkbox/wants.bin",
                     "regimes/wire/release/wire-reference", "regimes/wire/release/wire-load")
SCOPE = count()


def now():
    return datetime.now().astimezone()


def sha256(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def hashes(cache, names):
    return {name: sha256(cache / name) for name in names}


def mask(cpus):
    return ",".join(map(str, cpus))


def build_command(arguments, *, local=False, env=None, **kwargs):
    env = os.environ | (env or {})
    if local:
        env.update(HIRSEL_JOBS="4", UNITS_BUILD_UNDER_CAP="1")
        arguments = ("taskset", "-c", BUILD_CPUS, ROOT / "tools/capped",
                     "flock", scale.REGIMES, "timeout", "--kill-after=5s", "220s", *arguments)
    return subprocess.run(list(map(str, arguments)), cwd=ROOT, env=env,
                          check=True, text=True, **kwargs)


def toolchain(cache):
    path = cache / "toolchain.json"
    if path.is_file():
        artifacts = read(path)
        if all(Path(artifacts[name]).is_file() for name in ("compiler", "runtime")):
            release_tools(artifacts)
            return artifacts
    build_command([sys.executable, "tools/buck2/prepare.py"], local=True)
    result = build_command(
        [".buck2/bin/buck2", "build", "--remote-only", "-j4", "--target-platforms",
         "//tools/buck2:release", "//:hirsel-core-driver-bin-hirsel",
         "//:hirsel-core-rt-archives", "--show-json-output"],
        stdout=subprocess.PIPE,
    )
    outputs = json.loads(result.stdout)
    compiler = ROOT / outputs["root//:hirsel-core-driver-bin-hirsel"]
    runtime = ROOT / outputs["root//:hirsel-core-rt-archives"] / "x86_64-unknown-linux-gnu/libhirsel_core_rt.a"
    result = build_command(
        [sys.executable, "core/tools/measurement_artifacts.py", "--compiler", compiler,
         "--runtime", runtime, "--compiler-profile", "release"], stdout=subprocess.PIPE,
    )
    artifacts = json.loads(result.stdout)
    release_tools(artifacts)
    write(path, artifacts)
    return artifacts


def verify_sha(cache):
    flags = [line.split() for line in Path("/proc/cpuinfo").read_text().splitlines()
             if line.startswith("flags")]
    if not flags or not all({"sha_ni", "ssse3", "sse4_1"}.issubset(cpu) for cpu in flags):
        raise ValueError("A1 requires x86 hardware SHA-256")
    for name, expected in (("a1/bin/twin", True), ("a1/software/twin", False)):
        assembly = subprocess.check_output(["objdump", "-d", cache / name], text=True)
        if ("sha256rnds2" in assembly) != expected:
            raise ValueError(f"{name} has the wrong SHA-256 backend")


def prepare(cache, commit, suites, cases):
    path = cache / "binaries.json"
    manifest = read(path) if path.is_file() else {"commit": commit, "binary_sha256": {}}
    if manifest["commit"] != commit:
        raise ValueError("cache commit mismatch")
    recorded = manifest["binary_sha256"]
    need = {}
    if "a1" in suites:
        need["a1"] = A1_BINARIES
    if "regimes" in suites:
        need["regimes"] = REGIME_BINARIES
        if "checkbox" in cases:
            need["checkbox"] = CHECKBOX_BINARIES

    def available(names):
        return all(name in recorded and (cache / name).is_file() for name in names)

    for family, names in need.items():
        if available(names):
            if hashes(cache, names) != {name: recorded[name] for name in names}:
                raise ValueError(f"{family} cached binaries have changed")
            continue
        artifacts = toolchain(cache)
        compiler, runtime = (Path(artifacts[key]) for key in ("compiler", "runtime"))
        if family == "a1":
            for stage in ("hardware", "software", "hirsel"):
                build_command([ROOT / "bench/units/build_scale.sh", cache / "a1",
                               compiler, runtime, "lang", stage], local=True,
                              env={"UNITS_CARGO_TARGET": str(cache.parent / "cargo-a1")})
        else:
            bins = cache / "regimes"
            bins.mkdir(exist_ok=True)
            if family == "checkbox":
                entries = ("b.build_wire(out, prefix=(), jobs=4)",
                           "b.build_checkbox(out, cc, rt, prefix=(), jobs=4, wire=False)")
            else:
                entries = ("b.build_rust(out, None, False, regimes=True, prefix=(), jobs=4)",
                           "sys.exit(not b.build_hirsel(out, cc, rt, 'llvm', regimes=True, prefix=(), jobs=4))")
            for entry in entries:
                build_command([sys.executable, "-c",
                               "import sys; from pathlib import Path; from bench.multicore import build as b; "
                               f"out, cc, rt = map(Path, sys.argv[1:]); {entry}",
                               bins, compiler, runtime], local=True)
        recorded.update(hashes(cache, names))
        write(path, manifest)
    if "a1" in need:
        verify_sha(cache)
    return {name: recorded[name] for names in need.values() for name in names}


def eligible():
    current = now()
    start = current.replace(hour=2, minute=45, second=0, microsecond=0)
    end = start.replace(hour=4, minute=0)
    if current >= end:
        start += timedelta(days=1)
    return os.getloadavg()[0] <= 8 and (start - current).total_seconds() >= 240


@contextmanager
def guarded_hold(hold):
    while not eligible():
        print(f"Waiting for the measurement window, one-minute load {os.getloadavg()[0]:.2f}",
              file=sys.stderr, flush=True)
        time.sleep(scale.HOLDS_GAP_S)
    with measurement_slot(Path(scale.REGIMES)), measurement_slot(Path(scale.BOARD)):
        started = time.monotonic()
        hold.update(started=now().isoformat(), load_start=os.getloadavg()[0],
                    cc1plus_start=scale.cc1plus(), board_window_open=eligible())
        done = threading.Event()

        def watch_load():
            while not done.wait(0.2):
                if (load := os.getloadavg()[0]) > 8:
                    hold["load_exceeded"] = load
                    return

        usable = hold["board_window_open"] and not hold["cc1plus_start"]
        watcher = threading.Thread(target=watch_load) if usable else None
        if watcher:
            watcher.start()
        try:
            yield usable
        finally:
            done.set()
            if watcher:
                watcher.join()
            hold.update(hold_s=time.monotonic() - started, finished=now().isoformat(),
                        load_end=os.getloadavg()[0], cc1plus_end=scale.cc1plus())


def clean_hold(hold):
    return (hold["board_window_open"] and hold["cc1plus_start"] == hold["cc1plus_end"] == []
            and hold["hold_s"] < 240 and hold["load_end"] <= 8 and "load_exceeded" not in hold)


def scope(cpus, command, *, limit=55):
    return ["taskset", "-c", cpus, "systemd-run", "--user", "--scope", "--quiet", "--collect",
            "--unit", f"hirsel-fair-{os.getpid()}-{next(SCOPE)}.scope", "-p", "MemoryMax=3G",
            "-p", "MemorySwapMax=0", "-p", f"RuntimeMaxSec={limit}s", "-p", "TimeoutStopSec=2s",
            "--", *map(str, command)]


def measure_regime(directory, cpus, command, profile=False, cycles=False, *, controller):
    events = "instructions:u,duration_time,cycles:u"
    launcher = ["taskset", "-c", mask(cpus), "/usr/bin/time", "-f", "%U %S\n%M",
                "-o", directory / "rss.txt", *command]
    if profile:
        return scope(mask(controller), ["perf", "record", "-N", "--clockid", "mono", "--sample-cpu",
                             "-e", "cycles:u,cycles:k,cpu-clock", "-F", "499",
                             "--call-graph", "dwarf,8192", "-o", directory / "perf.data", "--", *launcher])
    return scope(mask(controller), ["perf", "stat", "-x", ";", "-e", events,
                          "-o", directory / "perf.csv", "--", *launcher])


def regime_pair(out, bins, case, grain, width, index, reverse, cpu_sets, profile=False):
    work = regimes.WORKLOADS[case]
    rounds = ROUNDS[grain]
    wanted = ("%d %d\n" % regimes.receipt(case, work.size, rounds)).encode()
    cpus = cpu_sets["server"][:width]
    parent = out / ("regime-profiles" if profile else "regime-samples")
    parent.mkdir(exist_ok=True)
    order = ARMS[::-1] if (index + reverse) % 2 else ARMS
    for attempt in count(1):
        point = parent / f"{case}-{grain}ns-w{width}-p{index}-h{attempt}"
        point.mkdir()
        hold = {"case": case, "grain_ns": grain, "width": width, "order": order}
        samples = {}
        with guarded_hold(hold) as usable:
            if usable:
                for arm in order:
                    if "load_exceeded" in hold:
                        break
                    path, row = run.observe_regime(
                        bins, point, case, arm.capitalize(), cpus, cpu_sets["load"], work.size, rounds,
                        wanted, profile=profile, launch_loader=scope,
                        launch_measurement=partial(measure_regime, controller=cpu_sets["controller"]))
                    cpu = None
                    if (path / "rss.txt").is_file():
                        lines = (path / "rss.txt").read_text().splitlines()
                        if len(lines) >= 2 and row.get("iterations"):
                            cpu = sum(map(float, lines[-2].split())) * 1e6 / row["iterations"]
                    sample = row | {"cpu_us_per_turn": cpu, "path": str(path.relative_to(out))}
                    if row["status"] == "ok" and not profile:
                        sample["work_per_s"] = regimes.rate(row)
                    samples[arm] = sample
        hold["clean"] = clean_hold(hold) and len(samples) == 2
        write(point / "pair.json", {"hold": hold, "samples": samples})
        time.sleep(scale.HOLDS_GAP_S)
        if hold["clean"]:
            return samples


@contextmanager
def journal(path, filesystem):
    def fstype():
        return subprocess.check_output(["findmnt", "-T", path, "-no", "FSTYPE"], text=True).strip()

    if fstype() != "ext4":
        raise ValueError(f"{path} must start on ext4")
    if filesystem == "ext4":
        yield
        return
    options = f"size=1G,nodev,nosuid,noexec,mode=0700,uid={os.getuid()},gid={os.getgid()}"
    subprocess.run(["sudo", "-n", "mount", "-t", "tmpfs", "-o", options, "tmpfs", path], check=True)
    try:
        if fstype() != filesystem:
            raise ValueError(f"expected {filesystem} journal at {path}")
        yield
    finally:
        subprocess.run(["sudo", "-n", "umount", path], check=True)


class A1Runner(scale.Runner):
    def profile_setup(self, mode):
        return 2_000_000_000, ["-e", "cycles:u,cycles:k,cpu-clock", "-F", "499",
                              "--call-graph", "dwarf,8192", "--sample-cpu"], "3"


def a1_row(source, side, summary=None):
    if source is None:
        return {"side": side, "status": "no-passing-offer"}
    load = source["load"] or {}
    status = "ok" if source["passed"] else "failed"
    if summary and summary.get("incomplete"):
        status = "incomplete"
    return {
        "side": side, "status": status, "offer_turns_per_s": source["rate"],
        "intended_arrival_p99_ns": load.get("e2e_p99_ns"),
        "server_added_p99_ns": load.get("p99_ns"),
        "cpu_us_per_turn": source["server_measured_cpu_us_per_turn"],
        "sync_groups_per_turn": source["sync_groups_per_turn"],
        "sha_backend": "software" if source["cell"]["software_sha"] else "hardware",
        "completed": load.get("completed"),
        "goodput_turns_per_s": (load["completed_after_warm_in_run"] * 1e9 /
                                (load["end_ns"] - load["warm_ns"])) if load else None,
        "trial": source["trial"], "hold": source["hold"],
        "profile": source.get("profile"),
    }


def search_samples(path):
    rows = [json.loads(line) for line in path.read_text().splitlines()]
    samples = {}
    for side, arm in zip(A1_SIDES, ARMS, strict=True):
        summaries = [row for row in rows if row.get("summary") and row["side"] == side]
        summary = next((row for row in summaries if row["max_rate"] is not None), None)
        trial = next((row for row in rows if summary and row.get("trial") and row["side"] == side
                      and not row["discarded"] and row["passed"]
                      and row["rate"] == round(summary["max_rate"])), None)
        samples[arm] = a1_row(trial, arm, summary)
    return samples


def ratio(samples, metric):
    h = samples.get("hirsel", {}).get(metric)
    t = samples.get("tokio", {}).get(metric)
    return h / t if h is not None and t else None


def expected_rows(suites, cases, full):
    labels = []
    if "a1" in suites:
        for storage in ("ext4", "tmpfs") if full else ("ext4",):
            for clients in CONNECTIONS if full else (256, 8192):
                cell = f"{storage}/{clients}/hardware"
                labels.append(("A1", cell))
                if full and storage == "ext4":
                    labels.append(("A1", cell + "/matched"))
        if full:
            labels.extend((("A1", "ext4/8192/software"), ("A1", "ext4/8192/software/matched")))
    if "regimes" in suites:
        for case in cases:
            for grain in regime_grains(case, full):
                for width in regime_widths(full):
                    labels.append(("Regimes", f"{case}/{grain}ns/w{width}"))
    return labels


def regime_grains(case, full):
    if case == "checkbox":
        return (0,)
    return regimes.GRAINS if full else (100_000,)


def regime_widths(full):
    return (1, 4, 8) if full else (1, 8)


class Report:
    def __init__(self, out, commit, config, expected):
        self.out = out
        self.expected = expected
        self.data = {"version": VERSION, "commit": commit, "config": config,
                     "started": now().isoformat(), "host": os.uname().nodename,
                     "host_load_start": os.getloadavg(), "binary_sha256": {}, "rows": [],
                     "retention_8192_over_256": {}, "failures": {}, "complete": False}
        self.save()

    def save(self):
        statuses = {(row["goal"], row["row"]): row["status"] for row in self.data["rows"]}
        self.data.update(updated=now().isoformat(), host_load_last=os.getloadavg(),
                         expected_rows=[{"goal": goal, "row": name, "status": statuses.get((goal, name), "missing")}
                                        for goal, name in self.expected])
        write(self.out / "fair.json", self.data)
        lines = ["# Fair Hirsel and Tokio benchmark", "", f"Commit `{self.data['commit']}`", "",
                 "Complete" if self.data["complete"] else "Incomplete", "",
                 "| Cell | Status | Metric | Hirsel | Tokio | Hirsel / Tokio |",
                 "|---|---|---|---:|---:|---:|"]
        for row in self.data["rows"]:
            for metric, value in row["ratios"]["tokio"].items():
                h = row["samples"]["hirsel"].get(metric)
                t = row["samples"]["tokio"].get(metric)
                show = lambda observed: f"{observed:,.4g}" if observed is not None else ""
                lines.append(f"| {row['row']} | {row['status']} | {metric} | {show(h)} | {show(t)} | {show(value)} |")
        for storage, values in self.data["retention_8192_over_256"].items():
            lines.append(f"\n{storage} 8,192/256 retention ({values['metric']}) {values['hirsel']:.4f}×")
        if self.data["failures"]:
            lines.extend(["", "## Incomplete cells", ""])
            lines.extend(f"- {cell}. {reason}" for cell, reason in self.data["failures"].items())
        (self.out / "SUMMARY.md").write_text("\n".join(lines) + "\n")

    def row(self, goal, label, samples, fields, **context):
        ratios = {field: ratio(samples, field) for field in fields}
        ok = all(s["status"] == "ok" for s in samples.values()) and all(r is not None for r in ratios.values())
        if context.get("sync_groups_match") is False:
            ok = False
        self.data["rows"].append({"goal": goal, "row": label, "commit": self.data["commit"],
                                  "status": "ok" if ok else "failed", "metrics": fields,
                                  "samples": samples, "ratios": {"tokio": ratios}, **context})
        if not ok:
            self.data["failures"][label] = "A result, required metric, or sync grouping missed its criterion"
        self.save()

    def retention(self, filesystem, metric, low, high):
        values = {}
        for arm in ARMS:
            a, b = low[arm].get(metric), high[arm].get(metric)
            if a and b:
                values[arm] = b / a
        if "hirsel" in values:
            self.data["retention_8192_over_256"][filesystem] = {"metric": metric, **values}
            self.save()


def matched_a1(runner, raw, cell_name, cell, offer, repeats, phase, profile=None):
    samples = {arm: [] for arm in ARMS}
    for index in range(repeats):
        order = A1_SIDES[::-1] if (index + phase) % 2 else A1_SIDES
        rows = runner.run_set(raw, "matched", cell_name, cell, 0,
                              [(side, offer, 12345) for side in order], profile)
        for side, arm in zip(A1_SIDES, ARMS, strict=True):
            trial = next(row for row in rows if row["side"] == side)
            sample = a1_row(trial, arm)
            if profile and trial["profile"]["rc"] != 0:
                sample["status"] = "profile-failed"
            samples[arm].append(sample)
    result = {}
    for arm, rows in samples.items():
        summary = rows[0].copy()
        summary["status"] = next((row["status"] for row in rows if row["status"] != "ok"), "ok")
        for field in ("intended_arrival_p99_ns", "server_added_p99_ns", "cpu_us_per_turn",
                      "sync_groups_per_turn", "goodput_turns_per_s"):
            values = [row[field] for row in rows if row[field] is not None]
            summary[field] = median(values) if values else None
        if len(rows) > 1:
            summary["arms"] = rows
        result[arm] = summary
    return result


def run_a1(report, cache, full, repeats, reverse, cpu_sets):
    scale.PORT, scale.TOOL_PORT = pick_ports()
    runner = A1Runner(cache / "a1", report.out, hold_guard=guarded_hold,
                      cpu_sets={role: mask(cpus) for role, cpus in cpu_sets.items()})
    for storage, software in ([("ext4", False), ("tmpfs", False), ("ext4", True)]
                              if full else [("ext4", False)]):
        found = {}
        populations = (8192,) if software else CONNECTIONS if full else (256, 8192)
        with journal(runner.store, storage):
            for connections in populations:
                cell_name = f"A1-{connections}"
                cell = scale.SCALE_CELLS[cell_name] | {"journal": storage, "software_sha": software}
                label = f"{storage}/{connections}/{'software' if software else 'hardware'}"
                metrics = ("offer_turns_per_s",) if full else ("cpu_us_per_turn",)
                path = report.out / f"{label.replace('/', '-')}-{'search' if full else 'matched'}.jsonl"
                with path.open("w") as raw:
                    if full:
                        runner.search(raw, cell_name, cell, A1_SIDES, [2 if reverse else 1], {})
                        samples = search_samples(path)
                    else:
                        samples = matched_a1(runner, raw, cell_name, cell, MATCHED_OFFER,
                                             repeats, reverse + len(found) * repeats)
                sync = ratio(samples, "sync_groups_per_turn")
                report.row("A1", label, samples, metrics, filesystem=storage, connections=connections,
                           kind="ceiling" if full else "matched", raw=path.name,
                           sync_groups_match=(sync is not None and abs(sync - 1) <= .05) if not full else None)
                found[connections] = samples
                if full and storage == "ext4":
                    raw_path = report.out / f"{label.replace('/', '-')}-matched.jsonl"
                    offer = min((value["offer_turns_per_s"] for value in samples.values()
                                 if value.get("offer_turns_per_s")), default=MATCHED_OFFER)
                    offer = min(MATCHED_OFFER, int(offer * .8))
                    with raw_path.open("w") as raw:
                        paired = matched_a1(runner, raw, cell_name, cell, offer, 1,
                                            reverse + len(found), profile="cpu")
                    group_ratio = ratio(paired, "sync_groups_per_turn")
                    report.row("A1", label + "/matched", paired, ("cpu_us_per_turn",),
                               filesystem=storage, connections=connections, kind="matched", raw=raw_path.name,
                               sync_groups_match=group_ratio is not None and abs(group_ratio - 1) <= .05)
        if not software and 256 in found and 8192 in found:
            report.retention(storage, "offer_turns_per_s" if full else "goodput_turns_per_s",
                             found[256], found[8192])


def run_regimes(report, cache, cases, full, repeats, reverse, cpu_sets):
    bins = cache / "regimes"
    index = 0
    for case in cases:
        for grain in regime_grains(case, full):
            baseline = {}
            for width in regime_widths(full):
                samples = {arm: [] for arm in ARMS}
                for _ in range(repeats):
                    pair = regime_pair(report.out, bins, case, grain, width, index, reverse, cpu_sets)
                    index += 1
                    for arm, value in pair.items():
                        samples[arm].append(value)
                reduced = {}
                for arm, values in samples.items():
                    status = "ok" if all(value["status"] == "ok" for value in values) else "failed"
                    rates = [value["work_per_s"] for value in values if "work_per_s" in value]
                    cpu = [value["cpu_us_per_turn"] for value in values if value["cpu_us_per_turn"] is not None]
                    rate = median(rates) if rates else None
                    if width == 1:
                        baseline[arm] = rate
                    reduced[arm] = {"side": arm, "status": status, "work_per_s": rate,
                                    "speedup_vs_w1": rate / baseline[arm] if baseline[arm] and rate else None,
                                    "cpu_us_per_turn": median(cpu) if cpu else None, "arms": values}
                report.row("Regimes", f"{case}/{grain}ns/w{width}", reduced, ("work_per_s", "speedup_vs_w1"),
                           case=case, grain_ns=grain, width=width, kind="work_rate")
        if full:
            profile = regime_pair(report.out, bins, case, 0 if case == "checkbox" else 10_000,
                                  8, index, reverse, cpu_sets, profile=True)
            index += 1
            report.data.setdefault("regime_profiles", {})[case] = profile
            report.save()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--quick", action="store_true")
    mode.add_argument("--full", action="store_true")
    parser.add_argument("--cells", default="a1,regimes", help="comma-separated families")
    parser.add_argument("--regimes", help="comma-separated workloads from the multicore suite")
    parser.add_argument("--out", type=Path)
    parser.add_argument("--cache", type=Path)
    for role, default in CPU_DEFAULTS.items():
        parser.add_argument(f"--{role}-cpus", type=cpu_list, default=default,
                            help=f"CPU list for {role}, default {mask(default)}")
    parser.add_argument("--repeats", type=int)
    parser.add_argument("--reverse", action="store_true", help="reverse the first paired arm")
    parser.add_argument("--deadline-seconds", type=int, help="limit time waiting and measuring after builds")
    args = parser.parse_args()
    suites = args.cells.split(",")
    cases = args.regimes.split(",") if args.regimes else REGIMES if args.full else QUICK_REGIMES
    if set(suites) - {"a1", "regimes"} or len(set(suites)) != len(suites):
        parser.error("select a1, regimes or a1,regimes")
    if set(cases) - set(REGIMES) or len(set(cases)) != len(cases) or (args.regimes and "regimes" not in suites):
        parser.error("select distinct regime workloads and include the regimes family")
    if (args.repeats is not None and args.repeats < 1
            or args.deadline_seconds is not None and args.deadline_seconds < 1):
        parser.error("repeats and deadline must be positive")
    cpu_sets = {role: getattr(args, f"{role}_cpus") for role in CPU_DEFAULTS}
    assigned = [cpu for cpus in cpu_sets.values() for cpu in cpus]
    if set(assigned) - MEASURE_CPUS or len(assigned) != len(set(assigned)):
        parser.error("CPU sets must be disjoint and use only 0-3, 8-11, 16-19 and 24-27")
    if len(cpu_sets["server"]) != 8:
        parser.error("the server CPU set must contain eight CPUs")
    controller = set(cpu_sets["controller"])
    if not controller.issubset(os.sched_getaffinity(0)):
        parser.error("pin the shell to the chosen controller CPUs")
    os.sched_setaffinity(0, controller)
    commit = subprocess.check_output(["git", "-C", ROOT, "rev-parse", "HEAD"], text=True).strip()
    if subprocess.check_output(["git", "-C", ROOT, "status", "--porcelain"]):
        parser.error("use a clean checkout to attribute the binaries to a commit")
    out = (args.out or ROOT.parent / "fair-results" / now().strftime("%Y%m%d-%H%M%S")).resolve()
    cache = (args.cache or out.parent / "fair-bin-cache").resolve() / commit
    if any(path.is_relative_to(ROOT) for path in (out, cache)):
        parser.error("output and cache must be outside the checkout")
    out.mkdir(parents=True)
    cache.mkdir(parents=True, exist_ok=True)
    repeats = args.repeats or (1 if args.full else 3)
    config = {"mode": "full" if args.full else "quick", "families": suites,
              "regimes": list(cases) if "regimes" in suites else [], "repeats": repeats,
              "reverse": args.reverse,
              **{f"{role}_cpus": cpus for role, cpus in cpu_sets.items()}}
    expected = expected_rows(suites, cases, args.full)
    report = Report(out, commit, config, expected)
    try:
        with measurement_slot(cache / "run.lock"):
            report.data["binary_sha256"] = prepare(cache, commit, suites, cases)
            report.data["toolchain"] = read(cache / "toolchain.json")
            report.save()
            signal.signal(signal.SIGALRM, scale.stop_at_deadline)
            signal.setitimer(signal.ITIMER_REAL, args.deadline_seconds or (14400 if args.full else 1800))
            try:
                if "a1" in suites:
                    run_a1(report, cache, args.full, repeats, args.reverse, cpu_sets)
                if "regimes" in suites:
                    run_regimes(report, cache, cases, args.full, repeats, args.reverse, cpu_sets)
            finally:
                signal.setitimer(signal.ITIMER_REAL, 0)
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError, scale.Deadline) as error:
        report.data["failures"]["run"] = str(error) or type(error).__name__
    report.data["complete"] = len(report.data["rows"]) == len(expected) and not report.data["failures"]
    report.save()
    return 0 if report.data["complete"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
