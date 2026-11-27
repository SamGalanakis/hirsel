#!/usr/bin/env python3
"""Standalone runner for Fleet T1/T2 formal specs and proofs (opt-in, not pre-push).

Checks:
  1. TLC bounded model check of proofs/fleet/FleetMove.tla (N=3, MaxEpoch=4).
  2. TLC NDJSON trace validation of proofs/fleet/TraceFleetMove.tla against SM1 traces.
  3. Quint typecheck, randomized invariant check, and ITF MBT trace generation on fleet_move.qnt.
  4. Veil (Lean 4) all-N inductive fencing proof in proofs/fleet/FleetMoveVeil.lean.

External toolchains live under ~/.local/opt/fleet-formal and /tmp/ref-veil (never in the repo).
"""

import argparse
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile

HERE = Path(__file__).resolve().parent
TOOLS = Path(os.environ.get("FORMAL_TOOLS", Path.home() / ".local" / "opt" / "fleet-formal"))
VEIL_DIR = Path(os.environ.get("VEIL_DIR", "/tmp/ref-veil"))
TRACE_TEST = "fleet@1/laws/sim_sm1_trace_export_ndjson_and_itf"


def run(cmd, cwd):
    print(f"+ {' '.join(str(c) for c in cmd)}", flush=True)
    res = subprocess.run(cmd, cwd=cwd)
    if res.returncode != 0:
        raise SystemExit(res.returncode)


def tlc_classpath():
    tla_nightly = TOOLS / "tla2tools-nightly.jar"
    tla_release = TOOLS / "tla2tools.jar"
    jar = tla_nightly if tla_nightly.is_file() else tla_release
    comm = TOOLS / "CommunityModules-deps.jar"
    if not jar.is_file() or not comm.is_file():
        raise SystemExit(f"Missing TLC jars under {TOOLS}; install tla2tools.jar and CommunityModules-deps.jar")
    return f"{jar}:{comm}"


def check_tlc():
    with tempfile.TemporaryDirectory(prefix="tlc-fleet-") as meta:
        run(
            ["java", "-XX:+UseParallelGC", "-cp", tlc_classpath(), "tlc2.TLC",
             "-noGenerateSpecTE", "-deadlock", "-workers", "4", "-metadir", f"{meta}/mc",
             "-config", "FleetMove.cfg", "FleetMove.tla"],
            cwd=HERE,
        )
    print("PASS Fleet bounded exploration", flush=True)


def trace_from_tests(report):
    tests = json.loads(report.read_text())["tests"]
    rows = [row for row in tests if row.get("definition") == TRACE_TEST]
    if len(rows) != 1:
        raise ValueError(f"expected one result for {TRACE_TEST}")
    row = rows[0]
    if row.get("passed") is not True or row.get("source") != "executed":
        raise ValueError(f"{TRACE_TEST} must pass in a fresh execution")
    return row["stdout"]


def u64(value):
    if type(value) is not int or not 0 <= value < (1 << 64):
        raise ValueError(f"expected a u64, got {value!r}")
    return value


def normalize_trace(trace):
    places = {}
    holders = {}
    seed = None
    for index, line in enumerate(trace.splitlines()):
        try:
            row = json.loads(line)
            if u64(row["index"]) != index:
                raise ValueError(f"expected contiguous index {index}, got {row['index']}")
            if index == 0:
                seed = u64(row["seed"])
            if u64(row["seed"]) != seed:
                raise ValueError(f"expected seed {seed}, got {row['seed']}")
            state = row["state"]
            place = u64(state["place_id"])
            names = holders.setdefault(place, {})

            def holder_name(holder):
                key = (u64(holder["lo"]), u64(holder["hi"]))
                return names.setdefault(key, len(names) + 1)

            state["holder"] = holder_name(state["holder"])
            event = row["event"]
            if event["tag"] in ("Sm1AuthPrimary", "Sm1AuthRecovery"):
                claim = event["value"][1]
            elif event["tag"] == "Sm1SpawnContinue":
                claim = event["value"]
            elif event["tag"] == "Sm1ExecCas":
                claim = event["value"][0]
            else:
                claim = None
            if claim is not None:
                expect = claim["expect"]
                expect["holder"] = holder_name(expect["holder"])
            places.setdefault(place, []).append(row)
        except (KeyError, IndexError, TypeError, ValueError) as error:
            raise ValueError(f"SM1 step {index}: {error}") from error
    if not places:
        raise ValueError("empty SM1 capture")
    return places


def replay_place(place, rows, cp, work):
    path = work / f"place-{place}.ndjson"
    path.write_text("".join(json.dumps(row) + "\n" for row in rows))
    result = subprocess.run(
        ["java", "-Xmx1g", "-XX:+UseParallelGC", "-cp", cp, "tlc2.TLC",
         "-noGenerateSpecTE", "-workers", "1", "-metadir", str(work / f"meta-{place}"),
         "-config", "TraceFleetMove.cfg", "TraceFleetMove.tla"],
        cwd=HERE, env={**os.environ, "TRACE_FILE": str(path)}, capture_output=True, text=True,
    )
    output = result.stdout + result.stderr
    depth = re.search(r"depth of the complete state graph search is (\d+)", output)
    if result.returncode == 0 and depth and int(depth[1]) == len(rows) + 1:
        return None
    cursors = re.findall(r"^(?:/\\ )?l = (\d+)$", output, re.MULTILINE)
    invariant = re.search(r"Invariant (\w+) is violated", output)
    if cursors and (invariant or "Deadlock reached" in output):
        # A deadlock leaves l at the unconsumed row. An invariant failure
        # happens in the state reached after consuming the offending row.
        cursor = int(cursors[-1])
        rejected = cursor - 1 if invariant else cursor
        if 1 <= rejected <= len(rows):
            row = rows[rejected - 1]
            reason = f"invariant {invariant[1]}" if invariant else "no matching model transition"
            return row["index"], f"Place {place}, {row['event']['tag']}: {reason}"
    print(output, file=sys.stderr)
    raise ValueError(f"Place {place}: TLC did not verify consumption of all {len(rows)} rows")


def check_trace(trace, report):
    try:
        places = normalize_trace(trace_from_tests(report) if report else trace.read_text())
        cp = tlc_classpath()
        with tempfile.TemporaryDirectory(prefix="tlc-fleet-trace-") as work:
            failures = [replay_place(place, rows, cp, Path(work)) for place, rows in places.items()]
        first = min((failure for failure in failures if failure is not None), default=None)
        if first:
            index, reason = first
            raise ValueError(f"first rejected step {index} (zero-based), {reason}")
    except (IndexError, KeyError, OSError, TypeError, ValueError) as error:
        raise SystemExit(f"Fleet law red (SM1 trace): {error}") from error
    print(f"PASS Fleet SM1 trace: {sum(map(len, places.values()))} rows across {len(places)} Place(s)", flush=True)


def check_quint():
    quint = TOOLS / "node_modules" / ".bin" / "quint"
    if not quint.is_file():
        found = shutil.which("quint")
        if not found:
            raise SystemExit(f"Missing quint CLI under {TOOLS} or PATH")
        quint = Path(found)
    run([quint, "typecheck", "fleet_move.qnt"], cwd=HERE)
    run(
        [quint, "run", "fleet_move.qnt",
         "--invariant=all_invariants", "--max-samples=500", "--max-steps=25"],
        cwd=HERE,
    )
    with tempfile.TemporaryDirectory(prefix="quint-fleet-") as out:
        itf = Path(out) / "fleet_mbt.itf.json"
        run(
            [quint, "run", "fleet_move.qnt",
             "--mbt", "--max-samples=10", "--max-steps=12", f"--out-itf={itf}"],
            cwd=HERE,
        )


def check_veil():
    if not VEIL_DIR.is_dir():
        raise SystemExit(f"Missing Veil checkout at {VEIL_DIR}; clone verse-lab/veil there or set VEIL_DIR")
    target = VEIL_DIR / "Examples" / "FleetMoveVeil.lean"
    shutil.copy2(HERE / "FleetMoveVeil.lean", target)
    run(["lake", "build", "Examples.FleetMoveVeil"], cwd=VEIL_DIR)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--only", choices=("tlc", "trace", "quint", "veil", "all"), default="all")
    trace = parser.add_mutually_exclusive_group()
    trace.add_argument("--trace", type=Path, help="NDJSON trace for TraceFleetMove")
    trace.add_argument("--trace-from-tests", type=Path, help="JSON report of a fresh three-host SM1 law run")
    args = parser.parse_args()
    if (args.trace or args.trace_from_tests) and args.only not in ("trace", "all"):
        parser.error("trace inputs require --only trace or --only all")
    if args.only in ("tlc", "all"):
        check_tlc()
    if args.only in ("trace", "all"):
        check_trace(args.trace or HERE / "sample_trace.ndjson", args.trace_from_tests)
    if args.only in ("quint", "all"):
        check_quint()
    if args.only in ("veil", "all"):
        check_veil()
    if args.only == "all":
        print("ALL FLEET PROOFS PASSED", flush=True)


if __name__ == "__main__":
    main()
