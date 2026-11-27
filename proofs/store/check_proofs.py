#!/usr/bin/env python3
"""Runner for the Store retention models (opt-in, not pre-push).

Checks, in order:
  1. TLC baselines: each module under the current STORE-SPEC (retention v2.164,
     catch-up v2.161 as amended by v2.167; and the retention model
     under 3157's rejected count+restart rule), every invariant, no error
     (`--deep` adds the fork, export-lease and anchor baselines, 5-25 minutes each).
  2. Known holes: STORE-SPEC v2.157 as written must produce each listed
     counterexample (the Tiered-idle partial-probe hole, export catch-up F1).
  3. Mutants: every line of mutants.txt must be caught by its named invariant.
     The retention mutants include review 3177 F1's broken idle reading.
  4. Reachability witnesses: each behaviour the v2.164 fold adds (the idle
     step, lineage cells under a fork row, the optional drain) must occur.
  5. The Python crash and stream models STORE-SPEC §3.9 cites (python/).

A check passes only on the expected outcome: "pass" needs TLC's "No error has
been found", a hole or mutant needs "Invariant <name> is violated". State counts
are printed, never asserted. `--save-traces` writes each known-hole
counterexample to traces/. TLC lives under ~/.local/opt/fleet-formal (never in
the repo), as for proofs/fleet.
"""

import argparse
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import time

HERE = Path(__file__).resolve().parent
TOOLS = Path(os.environ.get("FORMAL_TOOLS", Path.home() / ".local" / "opt" / "fleet-formal"))
WORKERS = str(min(8, os.cpu_count() or 1))

BOUNDS = {
    "retention": {
        "NKeys": "2", "MaxStamp": "3", "MaxPack": "3", "MaxCrash": "1",
        "WithAnchors": "FALSE", "WithExport": "FALSE", "WithFork": "FALSE",
    },
    # The fork row and its lineage cells (review 3177 F3). One key and no crash
    # keep it tractable; the crash interleavings are the retention bound's.
    "retention_fork": {
        "NKeys": "1", "MaxStamp": "3", "MaxPack": "3", "MaxCrash": "0",
        "WithAnchors": "FALSE", "WithExport": "FALSE", "WithFork": "TRUE",
    },
    "retention_export": {
        "NKeys": "2", "MaxStamp": "3", "MaxPack": "3", "MaxCrash": "1",
        "WithAnchors": "FALSE", "WithExport": "TRUE", "WithFork": "FALSE",
    },
    "retention_anchor": {
        "NKeys": "1", "MaxStamp": "3", "MaxPack": "3", "MaxCrash": "1",
        "WithAnchors": "TRUE", "WithExport": "FALSE", "WithFork": "FALSE",
    },
    "catchup": {
        "Vals": '{"a", "b", "c"}', "MaxWrites": "4", "MaxDemote": "2", "MaxCrash": "1",
    },
    "janitor": {"MaxBatch": "5"},
}

INVARIANTS = {
    "StoreRetention": ["RetainReachable", "SnapReadable", "ImageComplete", "AckedDurable",
                       "IdleReachesNoFrame", "ProbeClaim", "Progress", "CellsListed", "IdleProgress"],
    "StoreCatchUp": ["NoFalseCorrupt", "InstallExact", "BackfillNoneEmpty", "HolderNoCorrupt",
                     "RecoveredPrefix", "SupersedePremise"],
    "StoreJanitor": ["HeadBatchesKept"],
}

# (name, module, rules, bounds, deep). Every invariant must hold. Deep baselines
# (about 5 minutes each on 8 workers) run only with --deep.
BASELINES = [
    ("retention-v2164", "StoreRetention", "v2164", "retention", False),
    ("retention-v2164-fork", "StoreRetention", "v2164", "retention_fork", True),
    ("retention-count3157", "StoreRetention", "count3157", "retention", False),
    ("catchup-v2161", "StoreCatchUp", "v2161", "catchup", False),
    ("janitor", "StoreJanitor", None, "janitor", False),
    ("retention-v2164-export", "StoreRetention", "v2164", "retention_export", True),
    ("retention-v2164-anchor", "StoreRetention", "v2164", "retention_anchor", True),
]

# (trace name, module, rules, bounds, invariant): v2.157 as written must violate it.
HOLES = [
    ("tiered-idle-v2157", "StoreRetention", "v2157", "retention", "RetainReachable"),
    ("tiered-idle-v2157-restart", "StoreRetention", "v2157_restart", "retention", "Progress"),
    ("catchup-f1-premise", "StoreCatchUp", "v2157", "catchup", "SupersedePremise"),
    ("catchup-f1-corrupt", "StoreCatchUp", "v2157", "catchup", "NoFalseCorrupt"),
    ("catchup-f1-stale-index", "StoreCatchUp", "v2157", "catchup", "BackfillNoneEmpty"),
]

# (witness invariant, bounds): each must be violated under the current rules, with
# Track on, so every behaviour the v2.164 fold adds is reachable (not vacuous).
COVERS = [
    ("NoIdleRetire", "retention_fork"), ("NoIdleUnselected", "retention_fork"),
    ("NoIdleCursor", "retention_fork"), ("NoIdleCells", "retention_fork"),
    ("NoIdleHolderCells", "retention_fork"), ("NoIdleDropped", "retention_fork"),
    ("NoDrainNotIdle", "retention_fork"), ("NoDrainSkipped", "retention_fork"),
    ("NoUnforkQueued", "retention_fork"), ("NoCellFiled", "retention_fork"),
    ("NoCellMoved", "retention_fork"),
]

PYTHON_MODELS = [
    "model_v2.py", "model_v25_packs.py", "model_v29_refusal.py", "model_v23_streams.py",
    "model_v24_streams.py", "model_v221_streams.py", "model_v221_scope.py",
]


def classpath():
    jar = TOOLS / "tla2tools-nightly.jar"
    comm = TOOLS / "CommunityModules-deps.jar"
    if not jar.is_file() or not comm.is_file():
        raise SystemExit(f"Missing TLC jars under {TOOLS} (tla2tools-nightly.jar, CommunityModules-deps.jar)")
    return f"{jar}:{comm}"


def write_cfg(path, module, rules, mutant, bounds, invariants, track):
    lines = ["SPECIFICATION Spec", "CONSTANTS"]
    lines += [f"    {k} = {v}" for k, v in BOUNDS[bounds].items()]
    if module == "StoreRetention":
        lines.append(f"    Track = {'TRUE' if track else 'FALSE'}")
    if rules is not None:
        lines.append(f'    Rules = "{rules}"')
    lines.append(f'    Mutant = "{mutant}"')
    lines.append("INVARIANTS")
    lines += [f"    {i}" for i in invariants]
    path.write_text("\n".join(lines) + "\n")


def tlc(module, rules, mutant, bounds, invariants, tmp, track=False):
    tag = f"{module}-{rules}-{mutant}-{bounds}-{'-'.join(invariants) if len(invariants) == 1 else 'all'}"
    cfg = Path(tmp) / f"{tag}.cfg"
    write_cfg(cfg, module, rules, mutant, bounds, invariants, track)
    cmd = ["java", "-Xmx2g", "-XX:+UseParallelGC", "-cp", classpath(), "tlc2.TLC", "-noGenerateSpecTE",
           "-deadlock", "-workers", WORKERS, "-metadir", str(Path(tmp) / f"meta-{tag}"),
           "-config", str(cfg), f"{module}.tla"]
    t0 = time.monotonic()
    res = subprocess.run(cmd, cwd=HERE, capture_output=True, text=True)
    out = res.stdout + res.stderr
    secs = time.monotonic() - t0
    m = re.search(r"([\d,]+) states generated, ([\d,]+) distinct states found", out)
    d = re.search(r"depth of the complete state graph search is (\d+)", out)
    stats = (f"{m.group(1)} generated, {m.group(2)} distinct" if m else "no state count") + \
            (f", depth {d.group(1)}" if d else "") + f", {secs:.1f}s"
    violated = re.findall(r"Invariant (\w+) is violated", out)
    ok = "No error has been found" in out
    return ok, violated, stats, out


def trace_text(out):
    lines = out.splitlines()
    start = next((i for i, l in enumerate(lines) if "is violated" in l), None)
    if start is None:
        return out
    end = next((i for i in range(start, len(lines)) if re.match(r"[\d,]+ states generated", lines[i])), len(lines))
    return "\n".join(lines[start:end]) + "\n"


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--save-traces", action="store_true", help="write known-hole counterexamples to traces/")
    ap.add_argument("--deep", action="store_true", help="also run the export-lease and anchor baselines")
    ap.add_argument("--skip-python", action="store_true", help="skip the §3.9 Python models")
    args = ap.parse_args()
    failures = []
    started = time.monotonic()

    with tempfile.TemporaryDirectory(prefix="tlc-store-") as tmp:
        print("== baselines (every invariant holds)", flush=True)
        for name, module, rules, bounds, deep in BASELINES:
            if deep and not args.deep:
                print(f"  skip  {name} (--deep)", flush=True)
                continue
            ok, violated, stats, out = tlc(module, rules, "none", bounds, INVARIANTS[module], tmp)
            print(f"  {'PASS' if ok else 'FAIL'}  {name}: {stats}", flush=True)
            if not ok:
                failures.append(name)
                print(trace_text(out), file=sys.stderr)

        print("== known holes (v2.157 as written must fail)", flush=True)
        for name, module, rules, bounds, inv in HOLES:
            ok, violated, stats, out = tlc(module, rules, "none", bounds, [inv], tmp)
            hit = inv in violated
            print(f"  {'PASS' if hit else 'FAIL'}  {name}: {inv} {'violated' if hit else 'NOT violated'}; {stats}", flush=True)
            if not hit:
                failures.append(name)
            elif args.save_traces:
                (HERE / "traces" / f"{name}.txt").write_text(
                    f"# {module} Rules={rules} bounds={bounds}: {inv}\n" + trace_text(out))

        print("== mutants (each caught by its invariant)", flush=True)
        seeded = caught = 0
        for raw in (HERE / "mutants.txt").read_text().splitlines():
            if not raw.strip() or raw.startswith("#"):
                continue
            module, rules, mutant, bounds, inv = raw.split()[:5]
            rules = None if rules == "-" else rules
            seeded += 1
            ok, violated, stats, out = tlc(module, rules, mutant, bounds, [inv], tmp)
            hit = inv in violated
            caught += hit
            print(f"  {'CAUGHT' if hit else 'SURVIVED'}  {module}/{mutant} by {inv}; {stats}", flush=True)
            if not hit:
                failures.append(f"{module}/{mutant}")
        print(f"  {caught}/{seeded} mutants caught", flush=True)

        print("== reachability witnesses (each behaviour of the v2.164 fold occurs)", flush=True)
        for inv, bounds in COVERS:
            ok, violated, stats, out = tlc("StoreRetention", "v2164", "none", bounds, [inv], tmp, track=True)
            hit = inv in violated
            print(f"  {'REACHED' if hit else 'UNREACHED'}  {inv}; {stats}", flush=True)
            if not hit:
                failures.append(f"cover/{inv}")

    if not args.skip_python:
        print("== STORE-SPEC §3.9 Python models", flush=True)
        for f in PYTHON_MODELS:
            t0 = time.monotonic()
            res = subprocess.run([sys.executable, "-I", f], cwd=HERE / "python", capture_output=True, text=True)
            secs = time.monotonic() - t0
            tail = (res.stdout.strip().splitlines() or [""])[0]
            print(f"  {'PASS' if res.returncode == 0 else 'FAIL'}  {f} ({secs:.1f}s): {tail[:100]}", flush=True)
            if res.returncode != 0:
                failures.append(f)
                print(res.stdout[-4000:] + res.stderr[-4000:], file=sys.stderr)

    print(f"== {'FAILED: ' + ', '.join(failures) if failures else 'all checks passed'} "
          f"({time.monotonic() - started:.0f}s)", flush=True)
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
