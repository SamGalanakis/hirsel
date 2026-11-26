#!/usr/bin/env python3
"""Build and publish the board's Saturday capacity measurements."""
import argparse
from collections import Counter, defaultdict
from datetime import datetime, time, timedelta, timezone
import json
import math
import os
from pathlib import Path
import shlex
import signal
from statistics import median
import subprocess
import sys
import tempfile

import awfy_measure
import board
import build
import receipt
from bench.multicore.build import GAME_REV, GAME_ROWS
from bench.units.contract import WEEKLY_REGIMES
from timing_shield import BOARD

ROOT = build.ROOT
TOOLS = ROOT / "tools"
GO_VERSION = "go1.26.4"
BUILD_CPUS = "8-11"
AA_ROWS = tuple(f"awfy/{row}" for row in awfy_measure.EXPECTED["AWFY original"])
STAGES = ("objstore", "http", "aa-first", "aa-second", "aa-compare", "game", "fair", "units")


def python(script, *options):
    return [sys.executable, ROOT / "bench" / script, *options]


def commands(out, peers, go):
    tools = out / "toolchain.json"
    objstore = out / "objstore"
    aa_options = ("--pairs", "7", "--rows", ",".join(AA_ROWS), "--artifacts", tools,
                  "--cpu", "16", "--lock", BOARD, "--upstream", peers / "awfy",
                  "--awfy-build", out / "awfy")
    return {
        "objstore": python("objstore/run.py", "time", "--rust", objstore / "objstore-ceiling",
                           "--hirsel", objstore / "client/hirsel", "--cert", objstore / "cert",
                           "--output", objstore / "time.jsonl"),
        "http": python("http/run.py", "--stage", "services", "--knee-only", "--shared-timing-locks",
                       "--server-cpu", "16", "--client-cpu", "17", "--output", out / "http"),
        "aa-first": python("awfy/receipt.py", "sample", out / "aa-first", *aa_options),
        "aa-second": python("awfy/receipt.py", "sample", out / "aa-second", *aa_options),
        "aa-compare": python("awfy/receipt.py", "compare", out / "aa-first/pairs.json",
                             out / "aa-second/pairs.json", "--aa"),
        "game": python("multicore/run.py", "--case", *GAME_ROWS, "--workers", *map(str, range(1, 17)),
                       "--repeats", "3", "--allow-smt", "--build-cpus", BUILD_CPUS,
                       "--measure-cpus", "16-23,0-7", "--lock", BOARD,
                       "--bins", out / "game-bins", "--out", out / "game"),
        "fair": ["taskset", "-c", "26-27", *python("units/fair.py", "--weekly", "--cells",
                 "a1,regimes", "--regimes", ",".join(WEEKLY_REGIMES), "--repeats", "3", "--out", out / "fair")],
        "units": python("awfy/board.py", out / "comparison", "--comparison", "--comparison-family", "units",
                        "--weekly-units", "--units-receipt", out / "fair/fair.json",
                        "--build-cpus", BUILD_CPUS, "--measure-cpus", "16-19"),
        "game-build": python("multicore/build.py", "--out", out / "game-bins",
                             "--game", peers / "benchmarks-game", "--go", go,
                             "--goose", out / "goose-build/goose"),
    }


def show_plan(base, go):
    scripts = commands(base / "weekly-runs/RUN", base / "peers", go)
    for name in STAGES:
        print(f"\n{name}\n", flush=True)
        if name in ("http", "objstore", "game", "fair"):
            command = scripts[name]
            subprocess.run([*(command[3:] if name == "fair" else command), "--plan"], check=True)
        elif name in ("aa-first", "aa-second"):
            for row in AA_ROWS:
                print(f"{row} (7 pairs)")
        elif name == "units":
            print("Import every fair cell into the comparison board.")
        else:
            print("Compare both complete A/A runs with identical images and CPU.")
        print(shlex.join(map(str, scripts[name])))


def cutoff(now):
    today = now.date()
    if now.weekday() == 5 and now.time() >= time(22):
        today += timedelta(days=1)
    elif now.weekday() != 6 or now.time() >= time(2, 45):
        raise ValueError("weekly measurement window is Saturday 22:00 UTC to Sunday 02:45 UTC")
    return datetime.combine(today, time(2, 45), tzinfo=timezone.utc)


def json_lines(path):
    with path.open() as stream:
        for line in stream:
            if line.endswith("\n"):
                yield json.loads(line)


def hold_counts(out, stage):
    if stage == "fair":
        holds = [receipt.read(path)["hold"]["clean"]
                 for path in (out / "fair/regime-samples").glob("*/pair.json")]
        holds.extend(not row["discarded"] for path in (out / "fair").glob("*-matched.jsonl")
                     for row in json_lines(path) if row.get("hold_summary"))
        return {"attempted": len(holds), "admitted": sum(holds)}
    log = out / f"{stage}.holds"
    counts = Counter(log.read_text().splitlines()) if log.is_file() else Counter()
    return {kind: counts[kind] for kind in ("attempted", "admitted")}


def metrics(out, steps):
    result = {}
    if steps.get("units") == 0:
        for row in receipt.read(out / "comparison/comparison.json")["rows"]:
            for arm, sample in row["samples"].items():
                for metric in row["metrics"]:
                    if sample.get(metric) is not None:
                        result[f"{row['goal']}/{row['row']}/{arm}/{metric}"] = sample[metric]
    if steps.get("game") == 0:
        samples = defaultdict(list)
        for row in receipt.read(out / "game/samples.json"):
            point = f"Game/{row['case']}/{row['language']}/w{row['width']}"
            for field in ("seconds", "kernel_seconds", "instructions", "max_rss_kib"):
                if field in row:
                    samples[f"{point}/{field}"].append(row[field])
        result.update({key: median(values) for key, values in samples.items()})
    if steps.get("http") == 0:
        for key, row in receipt.read(out / "http/knees.json").items():
            result[f"HTTP/{key}/knee_rps"] = row["knee"]["Rate"]
            for level, tail in row["tails"].items():
                if not tail.get("Censored"):
                    result[f"HTTP/{key}/p99_ns_at_{level}pct"] = tail["P99NS"]
    if steps.get("objstore") == 0:
        samples = defaultdict(list)
        for row in json_lines(out / "objstore/time.jsonl"):
            key = f"Objstore/{row['mode']}/{row['method']}"
            samples[f"{key}/ns_per_byte"].append(row["ns"] / row["bytes"])
            samples[f"{key}/cycles_per_byte"].append(row["event_count"] / row["bytes"])
        result.update({key: median(values) for key, values in samples.items()})
    if steps.get("aa-compare") == 0:
        for name in ("aa-first", "aa-second"):
            for key, row in receipt.read(out / name / "pairs.json")["summary"].items():
                for event in ("instructions", "cycles"):
                    spread = float(row[event]["max_percent"]) - float(row[event]["min_percent"])
                    result[f"A/A/{name}/{key}/{event}_spread_pct"] = spread
    return result


def write_report(out, report):
    report["holds"] = {stage: hold_counts(out, stage) for stage in STAGES}
    receipt.write(out / "weekly.json", report)
    lines = ["# Weekly capacity board", "", f"Commit `{report['commit']}`", "",
             "| Run | Status | Holds attempted | Holds admitted |", "| --- | --- | ---: | ---: |"]
    steps = report["steps"]
    for stage in STAGES:
        counts = report["holds"][stage]
        status = steps.get(stage)
        show = "pending" if status is None else "qualified" if status == 0 else f"exit {status}"
        lines.append(f"| {stage} | [{show}]({stage}.log) | {counts['attempted']} | {counts['admitted']} |")
    lines += ["", "Evidence: [units](comparison/comparison.md), [Game](game.log), "
              "[HTTP](http/knees.json), [objstore](objstore/time.jsonl), [A/A](aa-compare.log)."]
    if changes := report.get("change_points"):
        lines += ["", "## Change points", "", "| Metric | Older median | Recent median |", "| --- | ---: | ---: |"]
        lines.extend(f"| {row['row']}/{row['metric']} | {row['before']:.6g} | {row['after']:.6g} |"
                     for row in changes)
    if error := report.get("history_error"):
        lines.extend(["", f"History publication failed. {error}"])
    (out / "weekly.md").write_text("\n".join(lines) + "\n")


class WeeklyRun:
    def __init__(self, base, go):
        self.base = base
        self.peers = base / "peers"
        self.go = go
        runs = base / "weekly-runs"
        runs.mkdir(parents=True, exist_ok=True)
        now = datetime.now(timezone.utc)
        self.out = Path(tempfile.mkdtemp(prefix=now.strftime("%Y%m%dT%H%M%SZ-"), dir=runs))
        self.scripts = commands(self.out, self.peers, go)
        self.env = os.environ | {"PATH": f"{go.parent}:{os.environ['PATH']}", "GOTOOLCHAIN": "local"}
        self.build_env = self.env | {"HIRSEL_MEMORY_LIMIT": "8G", "HIRSEL_JOBS": "4"}
        self.report = {"commit": board.git(ROOT, "rev-parse", "HEAD"), "started": now.isoformat(), "steps": {}}
        self.report["contract"] = {"version": 1, "host": os.uname().nodename, "game": GAME_REV,
                                   "go": GO_VERSION, "awfy": board.PINS["awfy"], "goose": board.PINS["goose"]}
        self.stopping = False
        signal.signal(signal.SIGTERM, self.stop)
        print(self.out, flush=True)

    def stop(self, _signal, _frame):
        self.stopping = True

    def run(self, name, prepare=None):
        if self.stopping:
            return
        with (self.out / f"{name}.log").open("w") as log:
            try:
                if prepare:
                    prepare(log)
                status = 143 if self.stopping else subprocess.run(
                    self.scripts[name], cwd=ROOT, stdout=log, stderr=subprocess.STDOUT,
                    env=self.env | {"HIRSEL_BOARD_HOLD_LOG": "" if name == "fair" else
                                    str(self.out / f"{name}.holds")},
                ).returncode
            except (OSError, ValueError, RuntimeError, LookupError, subprocess.SubprocessError) as error:
                print(error, file=log)
                status = 1
        self.report["steps"][name] = status
        write_report(self.out, self.report)

    def setup(self):
        dirty = board.git(ROOT, "status", "--porcelain")
        if dirty:
            raise ValueError(f"weekly checkout is dirty:\n{dirty}")
        version = subprocess.check_output([self.go, "version"], env=self.env, text=True).split()
        if version[2:] != [GO_VERSION, "linux/amd64"]:
            raise ValueError(f"weekly Go requires {GO_VERSION} linux/amd64")
        subprocess.run([sys.executable, TOOLS / "buck2/prepare.py"], cwd=ROOT, env=self.build_env, check=True)

    def objstore(self, log):
        directory = self.out / "objstore"
        directory.mkdir()
        cargo = subprocess.run([TOOLS / "capped", "cargo", "build", "--manifest-path",
                                ROOT / "bench/objstore/Cargo.toml", "--workspace", "--all-targets",
                                "--release", "--locked", "--message-format=json"],
                               cwd=ROOT, env=self.build_env, stdout=subprocess.PIPE, stderr=log,
                               text=True, check=True)
        binary, = [row["executable"] for line in cargo.stdout.splitlines() if line.startswith("{")
                   if (row := json.loads(line)).get("executable")
                   and row["target"]["kind"] == ["bin"] and not row["profile"]["test"]]
        # Keep the executable's basename for the runner's process ownership check.
        (directory / "objstore-ceiling").symlink_to(binary)
        subprocess.run([binary, "cert", directory / "cert"], cwd=ROOT, stdout=log, stderr=log, check=True)
        subprocess.run(python("objstore/build_client.py", directory / "cert", directory / "client"),
                       cwd=ROOT, env=self.build_env, stdout=log, stderr=log, check=True)

    def toolchain(self, log):
        path = self.out / "toolchain.json"
        if not path.is_file():
            result = subprocess.run([sys.executable, ROOT / "core/tools/measurement_artifacts.py",
                                     "--threads", "4"], cwd=ROOT, env=self.build_env, stdout=subprocess.PIPE,
                                    stderr=log, text=True, check=True)
            receipt.write(path, json.loads(result.stdout))
        return receipt.read(path)

    def aa(self, log):
        upstream = self.peers / "awfy"
        if board.git(upstream, "rev-parse", "HEAD") != board.PINS["awfy"] or board.git(
                upstream, "status", "--porcelain", "--untracked-files=no"):
            raise ValueError(f"{upstream} must have clean tracked sources at {board.PINS['awfy']}")
        tools = self.out / "toolchain.json"
        self.toolchain(log)
        directory = self.out / "awfy"
        directory.mkdir()
        build.hirsel(argparse.Namespace(out=directory, artifacts=tools, upstream=upstream,
                                        env=self.build_env, hirsel_entries=("harness",)))

    def game(self, log):
        game = self.peers / "benchmarks-game"
        board.git(game, "rev-parse", "--verify", f"{GAME_REV}^{{commit}}")
        goose = self.peers / "goose"
        if board.git(goose, "rev-parse", "HEAD") != board.PINS["goose"] or board.git(
                goose, "status", "--porcelain", "--untracked-files=no"):
            raise ValueError(f"{goose} must have clean tracked sources at {board.PINS['goose']}")
        tools = self.toolchain(log)
        for command in (["cmake", "-S", goose, "-B", self.out / "goose-build", "-DCMAKE_BUILD_TYPE=Release"],
                        ["cmake", "--build", self.out / "goose-build", "--parallel", "4", "--target", "goose"],
                        [*self.scripts["game-build"], "--hirsel", tools["compiler"], "--runtime", tools["runtime"]]):
            subprocess.run(command, cwd=ROOT, env=self.build_env, stdout=log, stderr=log, check=True)

    def publish_history(self):
        script = [sys.executable, TOOLS / "board-history.py"]
        with (self.out / "history.log").open("w") as log:
            try:
                subprocess.run([*script, "append", "--base", self.base, self.out],
                               stdout=log, stderr=subprocess.STDOUT, check=True)
                result = subprocess.check_output([*script, "detect", "--base", self.base,
                                                  "--run", self.out.name], stderr=log, text=True)
                self.report["change_points"] = [json.loads(line) for line in result.splitlines()]
            except (OSError, ValueError, subprocess.SubprocessError) as error:
                self.report["history_error"] = str(error)

    def measure(self):
        write_report(self.out, self.report)
        try:
            self.setup()
            self.run("objstore", self.objstore)
            self.run("http")
            self.run("aa-first", self.aa)
            self.run("aa-second")
            self.run("aa-compare")
            self.run("game", self.game)
            self.run("fair")
            self.run("units")
        finally:
            self.report["finished"] = datetime.now(timezone.utc).isoformat()
            self.report["complete"] = not self.stopping and all(
                self.report["steps"].get(name) == 0 for name in STAGES)
            self.report["metrics"] = metrics(self.out, self.report["steps"])
            write_report(self.out, self.report)
            self.publish_history()
            write_report(self.out, self.report)
        return int(not self.report["complete"] or "history_error" in self.report)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", type=Path, default=Path.home() / ".local/share/hirsel-board")
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--plan", action="store_true")
    mode.add_argument("--inside-window", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args()
    base = args.base.resolve()
    go = Path(os.environ.get("HIRSEL_BOARD_GO", base / f"peers/{GO_VERSION}/bin/go")).resolve()
    if args.plan:
        show_plan(base, go)
        return 0
    try:
        now = datetime.now(timezone.utc)
        end = cutoff(now)
        if not args.inside_window:
            seconds = math.floor((end - now).total_seconds())
            if seconds < 1:
                raise ValueError("weekly window has ended")
            os.execvp("timeout", ["timeout", "--kill-after=30", str(seconds), sys.executable, __file__,
                                  "--inside-window", "--base", str(base)])
        base.mkdir(parents=True, exist_ok=True)
        with receipt.measurement_slot(base / "weekly.lock", blocking=False):
            return WeeklyRun(base, go).measure()
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        print(error, file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
