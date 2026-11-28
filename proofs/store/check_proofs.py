#!/usr/bin/env python3
"""Runner for the Store retention models (opt-in, not pre-push).

Checks, in order:
  1. TLC baselines: each module under the current STORE-SPEC (retention v2.164 and the Format 15 direct/foreign pack projections,
     catch-up v2.161 as amended by v2.167; and the retention model
     under 3157's rejected count+restart rule), every invariant, no error
     (`--deep` adds the fork and export-lease baselines, 5-25 minutes each).
  2. Known holes: STORE-SPEC v2.157 as written must produce each listed
     counterexample (the Tiered-idle partial-probe hole, export catch-up F1).
  3. Mutants: every line of mutants.txt must be caught by its named invariant.
     The retention mutants include review 3177 F1's broken idle reading.
  4. Reachability witnesses: each behaviour the v2.164 fold adds (the idle
     step, lineage cells under a fork row, the optional drain) must occur.
  5. Preview holds (StorePreviewHold.tla, STORE-SPEC §15.7): removal of a hold
     at any time keeps every listed hold restorable, an operator clock further
     ahead than the grace breaks only PromiseKept, and each race the protocol
     must survive occurs.
  6. The Python crash and stream models STORE-SPEC §3.9 cites (python/).
  7. Retention traces (TraceStoreRetention.tla, seconds): an archive/gap/expire
     fixture conforms and publishing before a pack upload is refused. The crash sim's
     trace (traces/retention-trace-green.ndjson, or --trace FILE) conforms to
     StoreRetention; the hand-edited F1 trace (a cut that retires P while its
     queue still names P) is refused at its offending cut, and the
     idle_predicts_drain mutant model admits it. `--trace-only` runs only
     these (the nightly board's laws stage, with the trace the run printed).

A check passes only on the expected outcome: "pass" needs TLC's "No error has
been found", a hole or mutant needs "Invariant <name> is violated" (or "Action
property <name> is violated"). State counts
are printed, never asserted. `--save-traces` writes each known-hole
counterexample to traces/. TLC lives under ~/.local/opt/fleet-formal (never in
the repo), as for proofs/fleet.
"""

import argparse
import json
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
        "WithExport": "FALSE", "WithFork": "FALSE", "WithBuild": "FALSE", "WithTail": "FALSE",
    },
    # The fork row and its lineage cells (review 3177 F3). One key and no crash
    # keep it tractable; the crash interleavings are the retention bound's.
    "retention_fork": {
        "NKeys": "1", "MaxStamp": "3", "MaxPack": "3", "MaxCrash": "0",
        "WithExport": "FALSE", "WithFork": "TRUE", "WithBuild": "FALSE", "WithTail": "FALSE",
    },
    "retention_export": {
        "NKeys": "2", "MaxStamp": "3", "MaxPack": "3", "MaxCrash": "1",
        "WithExport": "TRUE", "WithFork": "FALSE", "WithBuild": "FALSE", "WithTail": "FALSE",
    },
    # The hot Tiered build pin (STORE-SPEC v2.169 as amended by v2.171): one key of
    # the built collection plus the untouched collection U.
    "retention_build": {
        "NKeys": "1", "MaxStamp": "4", "MaxPack": "3", "MaxCrash": "1",
        "WithExport": "FALSE", "WithFork": "FALSE", "WithBuild": "TRUE", "WithTail": "FALSE",
    },
    # A projector image as a capture's tail base (STORE-SPEC v2.177): a Memory
    # primary reading the image after demotion, the image released, expiry, reclaim
    # and reopen. One key and one crash.
    "retention_tail": {
        "NKeys": "1", "MaxStamp": "3", "MaxPack": "2", "MaxCrash": "1",
        "WithExport": "FALSE", "WithFork": "FALSE", "WithBuild": "FALSE",
        "WithTail": "TRUE",
    },
    "catchup": {
        "Vals": '{"a", "b", "c"}', "MaxWrites": "4", "MaxDemote": "2", "MaxCrash": "1",
    },
    "janitor": {"MaxBatch": "5"},
}

BOUNDS["archive"] = {**BOUNDS["retention"], "NKeys": "1", "MaxPack": "3"}
# Preview holds over ETag-only head writes (STORE-SPEC §15.7 "Preview holds"):
# two children, boundaries 2 and 3, five head writes of which one may erase and
# recreate the archive, an operator clock at most Skew ahead and a retirement
# grace equal to it (the tight case).
BOUNDS["preview"] = {
    "NKeys": "1", "MaxStamp": "3", "MaxPack": "2", "MaxCrash": "0",
    "WithExport": "FALSE", "WithFork": "FALSE", "WithBuild": "FALSE", "WithTail": "FALSE",
    "LastTime": "2", "MaxPut": "5", "MaxWipe": "1", "Skew": "1", "Grace": "1", "Watch": '"none"',
}
BOUNDS["direct"] = {**BOUNDS["archive"], "MaxPack": "4"}
BOUNDS["foreign"] = BOUNDS["preview"]
BOUNDS["direct_replay"] = {"MaxOps": "4"}
# Directory follower capture retention (STORE-SPEC §13.5 "Followers"): three
# publishes, three packs, one lost or kept unsynced rename, one erased pack.
BOUNDS["follower_capture"] = {"MaxPub": "3", "MaxPack": "3", "MaxCrash": "1", "MaxErase": "1"}
ARCHIVE_INVARIANTS = ["ArchiveComplete", "ArchiveIntervals", "ArchiveBounded", "ArchiveSourceReadable", "ArchiveStagedSafe"]

INVARIANTS = {
    "StoreRetention": ["RetainReachable", "SnapReadable", "ImageComplete", "AckedDurable",
                       "IdleReachesNoFrame", "ProbeClaim", "Progress", "CellsListed", "IdleProgress"],
    "StoreDirectReplay": ["StampDisjoint", "CounterCovers"],
    "StoreFollowerCapture": ["NoReclaimedRead", "NoFalseReclaim", "NoFalseAbsent",
                             "CaptureDurable", "FloorDurable"],
    "StoreDirectPack": [*ARCHIVE_INVARIANTS, "DirectReadable", "FollowerReadable"],
    "StorePreviewPack": ["ForeignBound", "ChildDeleteAuthority", "LocalReadable"],
    "StoreCatchUp": ["NoFalseCorrupt", "InstallExact", "BackfillNoneEmpty", "HolderNoCorrupt",
                     "RecoveredPrefix", "SupersedePremise"],
    "StoreJanitor": ["HeadBatchesKept"],
    "StorePreviewHold": ["ArchiveComplete", "HeldReadable", "NoResurrection"],
}
# Action properties a baseline also checks.
PROPERTIES = {"StorePreviewHold": ["PromiseKept", "HoldStable", "HeadNeverRepeats"]}
# Each must occur (`Watch` set, `NoWitness` violated): a retirement applied over
# an extension in flight and the reverse, an extension of a lapsed hold, an
# archiver write over other PUTs in flight, a child reporting expiry, an erase
# beside another child's live hold, a re-admission of a mounted child, a
# genesis published after its hold left, a child reporting expiry at a head
# that lists its stamp under another incarnation, and a recreated head meeting
# a PUT issued against the same rev of the erased one.
PREVIEW_WATCH = ["retireOverExtend", "extendOverRetire", "lapsedExtend", "bumpOverFlight",
                 "expiredSeen", "eraseBesideHold", "readmit", "staleMount",
                 "stampRepeated", "twinRefused", "oldEraser"]

# (name, module, rules, bounds, deep). Every invariant must hold. Deep baselines
# (about 5 minutes each on 8 workers) run only with --deep.
BASELINES = [
    ("direct-replay", "StoreDirectReplay", None, "direct_replay", False),
    ("follower-capture", "StoreFollowerCapture", None, "follower_capture", False),
    ("direct-pack", "StoreDirectPack", "v2164", "direct", False),
    ("foreign-pack", "StorePreviewPack", "v2164", "foreign", False),
    ("archive", "StoreRetention", "v2164", "archive", False),
    ("retention-v2164", "StoreRetention", "v2164", "retention", False),
    ("retention-v2164-fork", "StoreRetention", "v2164", "retention_fork", True),
    ("retention-count3157", "StoreRetention", "count3157", "retention", False),
    ("retention-v2164-build", "StoreRetention", "v2164", "retention_build", False),
    ("retention-v2164-tail", "StoreRetention", "v2164", "retention_tail", False),
    ("catchup-v2161", "StoreCatchUp", "v2161", "catchup", False),
    ("janitor", "StoreJanitor", None, "janitor", False),
    ("preview-hold", "StorePreviewHold", "v2164", "preview", False),
    ("retention-v2164-export", "StoreRetention", "v2164", "retention_export", True),
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
COVER_MODULE = {"direct_replay": "StoreDirectReplay", "direct": "StoreDirectPack", "foreign": "StorePreviewPack",
                "follower_capture": "StoreFollowerCapture"}
UNRULED = ("direct_replay", "follower_capture")
COVERS = [
    # One per §13.12 L66 follower-capture case (follower_laws, tiered_open_laws,
    # store_mixed_tail_laws via the warm hit, follower_crash_laws).
    ("NoWarmReclaim", "follower_capture"), ("NoColdReclaim", "follower_capture"),
    ("NoAbsent", "follower_capture"), ("NoRemovedReclaim", "follower_capture"),
    ("NoLostRenameRead", "follower_capture"), ("NoSyncedReclaim", "follower_capture"),
    ("NoReclaimAfterCatchUp", "follower_capture"), ("NoKeptRenameReclaim", "follower_capture"),
    ("NoWitIo", "follower_capture"),
    ("NoReplayRebase", "direct_replay"),
    ("NoDirectReopen", "direct"), ("NoDirectRestore", "direct"),
    ("NoDirectAbandon", "direct"), ("NoDirectAdopt", "direct"),
    ("NoForeignCopy", "foreign"), ("NoForeignLost", "foreign"), ("NoForeignExpired", "foreign"),
    ("NoIdleRetire", "retention_fork"), ("NoIdleUnselected", "retention_fork"),
    ("NoIdleCursor", "retention_fork"), ("NoIdleCells", "retention_fork"),
    ("NoIdleHolderCells", "retention_fork"), ("NoIdleDropped", "retention_fork"),
    ("NoDrainNotIdle", "retention_fork"), ("NoDrainSkipped", "retention_fork"),
    ("NoUnforkQueued", "retention_fork"), ("NoCellFiled", "retention_fork"),
    ("NoCellMoved", "retention_fork"),
    ("NoBuildMovesU", "retention_build"), ("NoGraftLeavesPinFrame", "retention_build"),
    ("NoOrphanDropped", "retention_build"),
    ("NoTailRetire", "retention_tail"), ("NoTailReclaim", "retention_tail"),
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


def write_cfg(path, module, rules, mutant, bounds, invariants, track, properties=(), extra=None):
    spec = {"archive": "ArchiveSpec", "preview": "PreviewSpec", "direct": "DirectSpec",
            "foreign": "ForeignSpec"}.get(bounds, "Spec")
    lines = [f"SPECIFICATION {spec}", "CONSTANTS"]
    lines += [f"    {k} = {v}" for k, v in {**BOUNDS[bounds], **(extra or {})}.items()]
    if module in ("StoreRetention", "StorePreviewHold", "StoreDirectPack", "StorePreviewPack"):
        lines.append(f"    Track = {'TRUE' if track else 'FALSE'}")
    if rules is not None:
        lines.append(f'    Rules = "{rules}"')
    lines.append(f'    Mutant = "{mutant}"')
    if invariants:
        lines.append("INVARIANTS")
        lines += [f"    {i}" for i in invariants]
    if properties:
        lines.append("PROPERTIES")
        lines += [f"    {i}" for i in properties]
    path.write_text("\n".join(lines) + "\n")


def tlc(module, rules, mutant, bounds, invariants, tmp, track=False, properties=(), extra=None):
    tag = f"{module}-{rules}-{mutant}-{bounds}-{'-'.join(invariants) if len(invariants) == 1 else 'all'}"
    tag += "".join(f"-{re.sub(r'[^A-Za-z0-9]', '', k + v)}" for k, v in (extra or {}).items())
    tag += "".join(f"-{p}" for p in properties)
    cfg = Path(tmp) / f"{tag}.cfg"
    write_cfg(cfg, module, rules, mutant, bounds, invariants, track, properties, extra)
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
    violated = re.findall(r"(?:Invariant|Action property) (\w+) is violated", out)
    ok = "No error has been found" in out
    return ok, violated, stats, out


# ---- retention traces ---------------------------------------------------------
# retention-trace-f1 is retention-trace-green with the generation-6 cut (the
# first after Tiered -> Memory) retiring P while its drain left P's rows queued
# (review 3177 F1), as the idle_predicts_drain reading does; the rows after it
# follow that store.
TRACE_GREEN = HERE / "traces" / "retention-trace-green.ndjson"
TRACE_F1 = HERE / "traces" / "retention-trace-f1.ndjson"
TRACE_F1_GENERATION = 6
PACK_FIELDS = ("listed", "queue", "files", "removed", "leased")


def load_trace(path):
    rows = [json.loads(line) for line in Path(path).read_text().splitlines() if line.strip()]
    if not rows:
        raise SystemExit(f"{path}: empty retention trace")
    return rows


def normalize(rows):
    """Renumber pack sequence numbers 1..n in sequence order (the model's nextPack order)."""
    seqs = set()
    for r in rows:
        if r["kind"] == "archive":
            continue
        for k in PACK_FIELDS:
            seqs.update(r.get(k, []))
        if "pack" in r:
            seqs.add(r["pack"])
    rank = {s: i + 1 for i, s in enumerate(sorted(seqs))}
    out = []
    for r in rows:
        if r["kind"] == "archive":
            out.append(r)
            continue
        n = dict(r)
        for k in PACK_FIELDS:
            if k in n:
                n[k] = sorted(rank[s] for s in n[k])
        if "pack" in n:
            n["pack"] = rank[n["pack"]]
        out.append(n)
    return out, len(seqs)


def trace_shape(rows):
    """What a conforming trace must exercise to say anything about F1. Returns the gaps."""
    gaps = []
    gens = [r["generation"] for r in rows if r["kind"] == "cut"]
    if any(b != a + 1 for a, b in zip(gens, gens[1:])):
        gaps.append(f"cut rows skip or merge generations {gens}")
    held, held_cut, delisted_held, removed_after_drop = set(), 0, False, False
    dropped = set()
    listed = set()
    for r in rows:
        if r["kind"] == "capture":
            held = set(r["leased"])
        elif r["kind"] == "drop":
            dropped |= held
            held = set()
        elif r["kind"] == "cut":
            if set(r["queue"]) & held:
                held_cut += 1
            if (listed - set(r["listed"])) & held:
                delisted_held = True
            listed = set(r["listed"])
        elif r["kind"] == "remove" and r["pack"] in dropped:
            removed_after_drop = True
    if held_cut < 2:
        gaps.append("fewer than two cuts drain a held pack's rows")
    if not delisted_held:
        gaps.append("no cut retires a pack a held Snapshot leases")
    if not removed_after_drop:
        gaps.append("no pack file is removed after its Snapshot drops")
    if not any(r["kind"] == "reopen" for r in rows):
        gaps.append("no reopen row")
    return gaps


def early_removal(rows):
    """The trace with each removal (its cut and remove rows) moved before the drop that allowed it."""
    out = list(rows)
    for i, r in enumerate(out):
        if r["kind"] != "drop":
            continue
        j = next((k for k in range(i + 1, len(out)) if out[k]["kind"] == "remove"), None)
        if j is None:
            return None
        out.insert(j, out.pop(i))
        return out
    return None


def trace_tlc(path, mutant, tmp, rows=None):
    """(conforms, stuck row index or None, rows, stats) for one raw trace under Mutant."""
    rows, packs = normalize(rows if rows is not None else load_trace(path))
    packs = max(packs, 1 + sum(r["kind"] == "archive" and r.get("op") == "cut" for r in rows))
    tag = f"trace-{Path(path).stem}-{mutant}"
    norm = Path(tmp) / f"{tag}.ndjson"
    norm.write_text("".join(json.dumps(r) + "\n" for r in rows))
    consts = {"NKeys": "1", "MaxStamp": "2", "MaxPack": str(max(1, packs)),
              "MaxCrash": str(sum(r["kind"] == "reopen" for r in rows)),
              "WithExport": "FALSE", "WithFork": "FALSE", "WithBuild": "FALSE",
              "WithTail": "FALSE", "Track": "FALSE",
              "Rules": '"v2164"', "Mutant": f'"{mutant}"'}
    cfg = Path(tmp) / f"{tag}.cfg"
    cfg.write_text("SPECIFICATION TraceSpec\nCONSTANTS\n" +
                   "".join(f"    {k} = {v}\n" for k, v in consts.items()) +
                   "INVARIANTS\n    TraceUnfinished\n")
    cmd = ["java", "-Xmx1g", "-XX:+UseParallelGC", "-cp", classpath(), "tlc2.TLC", "-noGenerateSpecTE",
           "-deadlock", "-workers", "1", "-metadir", str(Path(tmp) / f"meta-{tag}"),
           "-config", str(cfg), "TraceStoreRetention.tla"]
    t0 = time.monotonic()
    res = subprocess.run(cmd, cwd=HERE, capture_output=True, text=True,
                         env={**os.environ, "TRACE_FILE": str(norm)})
    out = res.stdout + res.stderr
    secs = time.monotonic() - t0
    if "Invariant TraceUnfinished is violated" in out:
        return True, None, rows, f"{len(rows)} rows, {secs:.1f}s"
    d = re.search(r"depth of the complete state graph search is (\d+)", out)
    if "No error has been found" not in out or not d:
        print(out[-4000:], file=sys.stderr)
        return False, None, rows, f"TLC did not finish, {secs:.1f}s"
    return False, int(d.group(1)), rows, f"{len(rows)} rows, {secs:.1f}s"


def row_name(rows, stuck):
    r = rows[stuck - 1]
    name = f"row {stuck} ({r['kind']}"
    if "step" in r:
        name += f" at sim step {r['step']}"
    if "generation" in r:
        name += f", generation {r['generation']}"
    return name + f"): {json.dumps({k: r[k] for k in r if k not in ('kind', 'step', 'generation')})}"


TRACE_TEST = "store@1/store/test_store_retention_trace"


def trace_from_tests(report, tmp):
    """The trace a fresh run of the trace law printed, from `hirsel test --json`."""
    try:
        tests = json.loads(Path(report).read_text()).get("tests", [])
    except (OSError, ValueError) as e:
        raise SystemExit(f"{report}: no test report ({e})")
    rows = [t for t in tests if t.get("definition") == TRACE_TEST]
    if len(rows) != 1:
        raise SystemExit(f"{report}: no {TRACE_TEST} row")
    row = rows[0]
    if not row.get("passed"):
        raise SystemExit(f"{TRACE_TEST} failed (the crash sim's oracles): {row.get('stderr', '')[-2000:]}")
    if row.get("source") != "executed" or not row.get("stdout", "").strip():
        raise SystemExit(f"{TRACE_TEST} did not run fresh (source {row.get('source')}): no trace")
    path = Path(tmp) / "retention-trace-run.ndjson"
    path.write_text(row["stdout"])
    return path


def green_trace(args, tmp):
    return trace_from_tests(args.trace_from_tests, tmp) if args.trace_from_tests else args.trace


def check_archive_trace(path, tmp, failures):
    ok, stuck, rows, stats = trace_tlc(path, "none", tmp)
    print(f"  {'PASS' if ok else 'FAIL'}  archive/gap/expire trace: {stats}", flush=True)
    if not ok:
        failures.append("trace/archive")
        if stuck:
            print(f"        no archive action admits {row_name(rows, stuck)}", flush=True)
    # From clean Init only the MANIFEST has been uploaded; publishing needs its pack too.
    broken = [{"kind": "archive", "op": "stage", "cursor": 1, "key": ["unit", 1]},
              {"kind": "archive", "op": "publish", "cursor": 1}]
    expected = len(broken)
    for mutant, admits in (("none", False), ("archive_publish_early", True)):
        ok, stuck, _, stats = trace_tlc("archive-before-pack", mutant, tmp, broken)
        hit = ok if admits else not ok and stuck == expected
        print(f"  {'PASS' if hit else 'FAIL'}  archive-before-pack ({mutant}): "
              f"{'admitted' if ok else f'refused at row {stuck}'}; {stats}", flush=True)
        if not hit:
            failures.append(f"trace/archive-before-pack/{mutant}")


def check_traces(green, tmp, failures, from_test=False):
    archive_path = HERE / "traces" / "archive.ndjson"
    if not from_test and all(r["kind"] in {"archive", "gap", "expire"} for r in load_trace(green)):
        archive_path, green = green, TRACE_GREEN
    check_archive_trace(archive_path, tmp, failures)
    print("== retention traces (TraceStoreRetention: the crash sim against StoreRetention)", flush=True)
    ok, stuck, rows, stats = trace_tlc(green, "none", tmp)
    gaps = trace_shape(rows)
    print(f"  {'PASS' if ok and not gaps else 'FAIL'}  {Path(green).name} conforms: {stats}", flush=True)
    if not ok:
        failures.append("trace/green")
        if stuck:
            print(f"        no StoreRetention action admits {row_name(rows, stuck)}", flush=True)
    for gap in gaps:
        failures.append("trace/green-shape")
        print(f"        vacuous trace: {gap}", flush=True)
    ok, stuck, rows, stats = trace_tlc(TRACE_F1, "none", tmp)
    expected = next(i + 1 for i, r in enumerate(rows)
                    if r["kind"] == "cut" and r["generation"] == TRACE_F1_GENERATION)
    hit = not ok and stuck == expected
    print(f"  {'PASS' if hit else 'FAIL'}  {TRACE_F1.name} refused at its F1 cut: {stats}", flush=True)
    if stuck:
        print(f"        no StoreRetention action admits {row_name(rows, stuck)}", flush=True)
    if not hit:
        failures.append("trace/f1")
    ok, stuck, rows, stats = trace_tlc(TRACE_F1, "idle_predicts_drain", tmp)
    print(f"  {'PASS' if ok else 'FAIL'}  {TRACE_F1.name} admitted by Mutant idle_predicts_drain: {stats}", flush=True)
    if not ok:
        failures.append("trace/f1-mutant")
    # Removal is matched apart from publication: the green trace with its
    # removal moved before the Snapshot's drop is refused at the remove row.
    early = early_removal(load_trace(green))
    if early is None:
        failures.append("trace/early-removal")
        print("  FAIL  no drop followed by a removal to reorder", flush=True)
        return
    ok, stuck, rows, stats = trace_tlc(Path(tmp) / "early-removal.ndjson", "none", tmp, early)
    hit = not ok and stuck is not None and rows[stuck - 1]["kind"] == "remove"
    print(f"  {'PASS' if hit else 'FAIL'}  removal before its drop refused at the remove row: {stats}", flush=True)
    if stuck:
        print(f"        no StoreRetention action admits {row_name(rows, stuck)}", flush=True)
    if not hit:
        failures.append("trace/early-removal")


def check_preview(tmp, failures):
    """Removing a hold at any time is safe for every listed hold; only the promise needs the clock."""
    print("== Preview holds (integrity without a clock, the clock assumption, witnesses)", flush=True)
    module, rules = "StorePreviewHold", "v2164"
    ahead = {"Skew": BOUNDS["preview"]["LastTime"], "Grace": "0"}
    ok, _, stats, out = tlc(module, rules, "none", "preview", INVARIANTS[module], tmp,
                            properties=["HoldStable"], extra=ahead)
    print(f"  {'PASS' if ok else 'FAIL'}  integrity with an unbounded operator clock: {stats}", flush=True)
    if not ok:
        failures.append("preview/clock-free")
        print(trace_text(out), file=sys.stderr)
    _, violated, stats, _ = tlc(module, rules, "none", "preview", [], tmp, properties=["PromiseKept"], extra=ahead)
    hit = "PromiseKept" in violated
    print(f"  {'PASS' if hit else 'FAIL'}  PromiseKept {'needs' if hit else 'does NOT need'} "
          f"Grace >= Skew; {stats}", flush=True)
    if not hit:
        failures.append("preview/promise-needs-clock")
    for watch in PREVIEW_WATCH:
        _, violated, stats, _ = tlc(module, rules, "none", "preview", ["NoWitness"], tmp,
                                    extra={"Watch": f'"{watch}"'})
        hit = "NoWitness" in violated
        print(f"  {'REACHED' if hit else 'UNREACHED'}  {watch}; {stats}", flush=True)
        if not hit:
            failures.append(f"preview/{watch}")


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
    ap.add_argument("--deep", action="store_true", help="also run the fork and export-lease baselines")
    ap.add_argument("--skip-python", action="store_true", help="skip the §3.9 Python models")
    ap.add_argument("--trace", default=str(TRACE_GREEN), help="retention or archive NDJSON trace that must conform")
    ap.add_argument("--trace-from-tests", metavar="JSON",
                    help=f"take the trace from a `hirsel test --json` report of {TRACE_TEST}")
    ap.add_argument("--trace-only", action="store_true", help="run only the retention trace checks")
    args = ap.parse_args()
    failures = []
    started = time.monotonic()

    if args.trace_only:
        with tempfile.TemporaryDirectory(prefix="tlc-store-trace-") as tmp:
            check_traces(green_trace(args, tmp), tmp, failures, from_test=bool(args.trace_from_tests))
        print(f"== {'FAILED: ' + ', '.join(failures) if failures else 'all checks passed'} "
              f"({time.monotonic() - started:.0f}s)", flush=True)
        return 1 if failures else 0

    with tempfile.TemporaryDirectory(prefix="tlc-store-") as tmp:
        check_traces(green_trace(args, tmp), tmp, failures, from_test=bool(args.trace_from_tests))

        print("== baselines (every invariant holds)", flush=True)
        for name, module, rules, bounds, deep in BASELINES:
            if deep and not args.deep:
                print(f"  skip  {name} (--deep)", flush=True)
                continue
            invariants = ARCHIVE_INVARIANTS if bounds == "archive" else INVARIANTS[module]
            ok, violated, stats, out = tlc(module, rules, "none", bounds, invariants, tmp,
                                           properties=PROPERTIES.get(module, ()))
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

        print("== reachability witnesses (retention, replay and Preview pack reads)", flush=True)
        for inv, bounds in COVERS:
            ok, violated, stats, out = tlc(COVER_MODULE.get(bounds, "StoreRetention"),
                                           None if bounds in UNRULED else "v2164", "none",
                                           bounds, [inv], tmp, track=True)
            hit = inv in violated
            print(f"  {'REACHED' if hit else 'UNREACHED'}  {inv}; {stats}", flush=True)
            if not hit:
                failures.append(f"cover/{inv}")

        check_preview(tmp, failures)

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
