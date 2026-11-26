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
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

HERE = Path(__file__).resolve().parent
TOOLS = Path.home() / ".local" / "opt" / "fleet-formal"
VEIL_DIR = Path(os.environ.get("VEIL_DIR", "/tmp/ref-veil"))


def run(cmd, cwd, env=None):
    merged = dict(os.environ)
    if env:
        merged.update(env)
    print(f"+ {' '.join(str(c) for c in cmd)}", flush=True)
    res = subprocess.run(cmd, cwd=cwd, env=merged)
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


def check_tlc(trace_file):
    cp = tlc_classpath()
    with tempfile.TemporaryDirectory(prefix="tlc-fleet-") as meta:
        run(
            ["java", "-XX:+UseParallelGC", "-cp", cp, "tlc2.TLC",
             "-noGenerateSpecTE", "-deadlock", "-workers", "4", "-metadir", f"{meta}/mc",
             "-config", "FleetMove.cfg", "FleetMove.tla"],
            cwd=HERE,
        )
        run(
            ["java", "-XX:+UseParallelGC", "-cp", cp, "tlc2.TLC",
             "-noGenerateSpecTE", "-workers", "1", "-metadir", f"{meta}/trace",
             "-config", "TraceFleetMove.cfg", "TraceFleetMove.tla"],
            cwd=HERE,
            env={"TRACE_FILE": str(trace_file)},
        )


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
    parser.add_argument("--only", choices=("tlc", "quint", "veil", "all"), default="all")
    parser.add_argument("--trace", default="sample_trace.ndjson", help="NDJSON trace for TraceFleetMove")
    args = parser.parse_args()
    if args.only in ("tlc", "all"):
        check_tlc(args.trace)
    if args.only in ("quint", "all"):
        check_quint()
    if args.only in ("veil", "all"):
        check_veil()
    print("ALL FLEET PROOFS PASSED", flush=True)


if __name__ == "__main__":
    main()
