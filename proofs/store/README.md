# Store retention models

Model-checked specifications of Hirsel Store retention (`core/programs/store/STORE-SPEC.md`), and
the Python crash and stream models STORE-SPEC §3.9 cites. They are opt-in, not part of pre-push:

```sh
python3 proofs/store/check_proofs.py                # everything, about 3 minutes on 8 workers
python3 proofs/store/check_proofs.py --save-traces  # also rewrite traces/ from the known holes
python3 proofs/store/check_proofs.py --deep         # also the export-lease and anchor baselines (+10 min; the two run about 4.5 min each)
python3 proofs/store/check_proofs.py --skip-python  # TLC only
```

TLC is the nightly `tla2tools-nightly.jar` plus `CommunityModules-deps.jar` under
`~/.local/opt/fleet-formal` (or `$FORMAL_TOOLS`), as for `proofs/fleet`; nothing is vendored here.

**Standing rule (STORE-SPEC §3.9).** A STORE-SPEC revision that touches retention (the probe,
deadlists and Tiered-idle cuts, promotion, leases, export and catch-up, anchors, retirement or the
janitor) updates these models to the new text and passes `check_proofs.py` before an
implementation lane starts.

## Artifacts

| File | What it checks |
|---|---|
| `StoreRetention.tla` | Tiered pack retention: commits, cuts and their publication, pins and forks, Snapshots and their leases, the candidate queue, Tiered-idle cuts, the compaction probe, promotion of a historical Snapshot, demotion Tiered → Memory, anchors (archival retention), AP16 Remove, the export lease and its copy, crash and reopen between any two steps. |
| `StoreCatchUp.tla` | Export catch-up and install over a Tiered index across a demotion and a rebuild (§3.5a, §13.10a, §13.7 D-D7-5), the holder's own replay and fold, and recovery of a commit prefix at or past the last acknowledged ticket. |
| `StoreJanitor.tla` | Host-stream retirement: the §12.11.8 release rule that lets `low` pass a batch. |
| `mutants.txt` | The seeded one-rule mutants and the invariant that must catch each. |
| `traces/` | TLC counterexamples for the v2.157 holes, written by `--save-traces`. |
| `python/` | The §3.9 executable models: `model_v2.py` (journal, checkpoint and damage cuts), `model_v25_packs.py`, `model_v29_refusal.py`, `model_v23_streams.py`, `model_v24_streams.py`, `model_v221_streams.py`, `model_v221_scope.py`, and `model-v2-mutants.txt` (the record of the hand-edited M1–M4 runs §3.9 cites). Each exits non-zero on a failed cut or a surviving mutant. |
| `check_proofs.py` | Runs the baselines, the known holes, the mutants and the Python models. A check passes only on its expected outcome: "No error has been found", or `Invariant <name> is violated` for the named invariant. |

## Rules under check

Each TLA+ module takes a `Rules` constant naming the reading of the spec it checks.

- **`StoreRetention`**
  - `v2157`: v2.157 as written. An idle step advances the cursor's row count, and a later reading probe resumes there ("Progress").
  - `v2157_restart`: the only other reading v2.157 admits. A reading probe restarts an idle-advanced cursor at row 0, which "Progress" forbids.
  - `v2161`: the current text (v2.161, D-TI-1). An idle cut reads nothing, never installs or moves a cursor, and retires the first selected pack whole. Promotion copies a frame of a pack an idle cut retired through the Snapshot's lease. Only a crash or a reselection resets a cursor. v2.164 (review 3177 F2–F3) lets an idle cut take every listed class-3/4 pack, selected or not; the model already lets any listed cursorless pack be selected at any cut, so these rules cover it (lineage cells are not modelled).
  - `count3157`: review 3157's alternative, which v2.161 lists under "Rejected" as sound but costlier. Idle progress is a count apart from the probe cursor, and the first non-idle cut restarts it at row 0, as a third allowed reset. Kept as a second passing baseline, so the rejection's "sound" stays checked.
- **`StoreCatchUp`**
  - `v2157`: as written. A step ships "the collection rows it changed": for a Tiered index that is X's new root. The child installs it under the overlay its replayed frames built.
  - `v2161`: the current text (v2.161, D-CU-1). A demotion step carries X as a superseded id, and `apply_steps` drops the child's overlay of X and installs the empty root (D-D7-5). A build chunk ships its entries, which the child applies against their priors as replay applies a frame. The §13.7 premise is scoped to holder reopen and log@1 followers; the prepare child is the exception.

## Invariants

| Name | STORE-SPEC section | Claim |
|---|---|---|
| `RetainReachable` | §13.6 "The probe"; §13.5 step 2 "Resolution frees nothing"; §1.9 | Every frame a durable holder names (a head, a pin or fork, an anchor, a queue row) is in a listed pack whose file exists, at the PACK_INDEX row that holds its stamp. Nothing reachable is retired or reused. |
| `SnapReadable` | §13.5 holder leases; AP16 Remove | An open Snapshot's frames stay on disk, and promotion never reads a removed frame. |
| `ImageComplete` | §13.10a (v2.157 D-ZL-2) | Every pack the copied MANIFEST lists is in the export image. |
| `AckedDurable` | §1.9; §3.5 step 6 | An acknowledged pin survives every crash. |
| `IdleReachesNoFrame` | §13.6 "Tiered-idle cuts", I-TI | A Tiered-idle generation reaches no class-3/4 frame. |
| `ProbeClaim` | §13.6 "A probe across cuts" | No participating root of the published generation names a frame behind an unfinished probe's cursor. Probe state can be rebuilt after a crash or a partial probe, because no root depends on it. |
| `Progress` | §13.6 "Progress" | Only a crash or a `compact_ratio` reselection resets a cursor. `count3157` adds its idle reset. |
| `NoFalseCorrupt` | §3.5a "Replay"; §13.10a | A catch-up round never refuses a valid store as `Corrupt`. |
| `InstallExact` | §3.5a; §13.10a (the demotion floor) | A caught-up child holds exactly OLD's X, X's state and the row. No frame below a demotion survives it, and none above it is missed. |
| `BackfillNoneEmpty` | §1.2 index states; L68 (g) | `Backfilling(None)` holds no entry, at OLD and in the child. |
| `HolderNoCorrupt` | §13.7; L68 (f) | OLD's own replay and fold never refuse a valid store. |
| `RecoveredPrefix` | §1.9; §3.4 | A recovered state follows a real commit prefix at or past the last acknowledged durable ticket. |
| `SupersedePremise` | §13.7 "No epoch is needed" | "No live journal frame predates a supersession." v2.157 states it of every replay. v2.161 scopes it to holder reopen and log@1 followers, where it holds by construction, so it is checked only under `v2157`. |
| `HeadBatchesKept` | §12.11.8 Invariants and the release rule | No batch a Place's head contains (`live` or `sealed`) is below `low`. |

## Results (bounds in `check_proofs.py` `BOUNDS`)

Counts are from one run with 8 workers. `check_proofs.py` prints them and never asserts them.

| Check | Outcome |
|---|---|
| `StoreRetention` v2161 (retention bounds) | no error; 196,378 distinct states, depth 20, about 35 s |
| `StoreRetention` v2161, export lease on (`--deep`) | no error; 1,403,440 distinct states, depth 22, about 4.5 min |
| `StoreRetention` v2161, anchors on, one key (`--deep`) | no error; 944,626 distinct states, depth 25, about 4.5 min |
| `StoreRetention` count3157 (3157's rejected rule) | no error; 258,295 distinct states, depth 23, about 66 s |
| `StoreRetention` v2157 | `RetainReachable` violated: the 3160 hole (`traces/tiered-idle-v2157.txt`) |
| `StoreRetention` v2157_restart | `Progress` violated (`traces/tiered-idle-v2157-restart.txt`) |
| `StoreCatchUp` v2161 | no error; 577,192 distinct states, depth 15, about 7 s |
| `StoreCatchUp` v2157 | `SupersedePremise`, `NoFalseCorrupt` and `BackfillNoneEmpty` violated: 3150 F1 (`traces/catchup-f1-*.txt`) |
| `StoreJanitor` | no error; 229,799 distinct states, depth 19, 2 s |
| Mutants | 13 of 13 caught under v2.161 rules, each in under 3 s |
| Python models (§3.9) | 7 of 7 pass; `model_v221_scope.py` takes about 28 s |
| Whole default run | 168 s |

**The Tiered-idle hole (v2157).** A plain cut writes pack P1 with two frames. Capture Snapshot S.
A Tiered → Memory cut hands the head's Cold root to the queue. A cut pins S (promotion copies S's
unflushed row into pack P2) and drains the queue. Unpin S, selecting P1 for a probe: the published generation has no Tiered holder, no
queue row and no anchor, so it is Tiered-idle, and the probe step advances P1's cursor one row
without reading it. Pin S again. Promotion keeps frame (P1, row 1) in place: a PagePtr carries an
offset and no PACK_INDEX row, and an idle step never learns the cursor's offset. The probe then
resumes at row 2 and finishes P1 at that same cut, which retires P1 while the new pin still names row 1. This is the
3160 mechanism.

**3150 F1 (catch-up v2157).** OLD begins an export, then writes r a→b (X: delete a, insert b),
which the child replays into its overlay. A demotion follows (a step), then r b→a with no X op
(the builder is behind r). A build chunk inserts a (a step), and r a→b follows again (delete a, insert b).
At the demotion the child installs the empty root under its overlay, and at the chunk the chunk's
root {a} under the same overlay, whose delete of a hides the new entry. The last frame's delete of a
then fails its prior check: `Corrupt(Malformed)` on a valid store. In the shortest stale-index
trace, a demotion with one earlier frame leaves the child's X non-empty under `Backfilling(None)`.
TLC runs with several workers, so `--save-traces` can write a different shortest trace on another
run.

## What is abstracted

- **One collection, small bounds.** `StoreRetention` has one Tiered collection of `NKeys` rows, one frame per row, a few stamps and packs, one crash. Pages, offsets, BLOB frames and the class-3/4 split are not modelled: a frame is a stamp at a PACK_INDEX row.
- **Budgets of 1.** `probe_budget` and `FreeBudget` are 1 row per cut, so a probe spans cuts, which is where both holes live. Larger budgets change only the step count.
- **Holders.** One pin name stands for every pin and fork: a FORK, the import pin and `ImportPin` follow the same promotion and probe rules. One Snapshot at a time. Anchors are optional (`WithAnchors`), and the export lease too (`WithExport`).
- **Accounting.** `live` and the deadlists are advisory (D-ZL-1). The model keeps `live` and retires only by a finished probe, so the mutant that retires on zero live shows why. Recount probes, cells, `repair_cuts` and `compact_ratio` arithmetic are abstracted: any listed pack without a cursor may be selected at any cut.
- **Cuts are atomic.** A cut is one action in §13.6a's order. A crash inside a cut is the state before the MANIFEST rename (§3.6), which equals no cut. The pre-rename durability steps are `python/model_v2.py`'s and `model_v29_refusal.py`'s job.
- **Catch-up.** One row and one Many index, with priors reduced to "present" and "absent". A demotion and a build chunk are the only steps. Rounds apply everything not yet shipped. The pause, ROUND and PREPARED files, `apply_round`'s kept import and the account checks are abstracted. The child's install is checked as a state predicate whenever it is caught up.
- **Janitor.** One stream and one Place. Elections, TERM, XINDEX and the forced settle are abstracted; a crash forgets the head summary and pins are rebuilt from durable state.
- **No liveness.** Only safety is checked. A GC liveness property (every delisted pack is eventually removed once its leases end) is not.

## Trace vocabulary (for a later trace-validation lane)

A trace-validation wrapper would mirror `proofs/fleet/TraceFleetMove.tla`. It would read an NDJSON
trace (`ndJsonDeserialize` from CommunityModules `Json` and `IOUtils`, file from `TRACE_FILE`),
match each line to one action of the module, and compare the line's `state` with the model's
variables after the step. The store would emit one line per event in this shape, which follows
SM1's `{seed, index, event: {tag, value}, actions, state}`:

```json
{"seed": 7, "index": 12, "event": {"tag": "StoreCut", "value": {...}}, "actions": [], "state": {...}}
```

Packs are named by sequence number, frames by `[pack, row, stamp]` (`row` is the PACK_INDEX row,
which the emitter derives from the offset), and roots by the set of frames they reach. No emission
is implemented. This section is the contract for the lane that adds it.

| Tag | `value` | `state` fields compared | Module action |
|---|---|---|---|
| `StoreCommit` | `{key, stamp}` | `head` | `StoreRetention!Write` |
| `StoreCapture` | `{snapshot, leased: [pack]}` | `snap` | `Capture` |
| `StoreDropSnapshot` | `{snapshot}` | `snap` | `DropSnap` |
| `StoreCut` | `{op: plain\|pin\|unpin\|toMemory\|toTiered\|archive\|unarchive, selected: pack\|0, idle: bool, fresh: pack\|0, emitted: [stamp], promoted: [{stamp, from, to}], probed: {pack, row, matched}\|null, drained: [frame], retired: [pack]}` | `gen.listed`, `gen.queue`, `gen.pin`, `gen.anchor`, `head`, `cursor[pack].rows` | `Cut(op, sel)` |
| `StoreRemove` | `{pack}` | `files` | `RemoveFile(p)` |
| `StoreExportBegin` | `{attempt, lease: [pack], listed: [pack]}` | `export.lease` | `BeginExport` |
| `StoreExportCopied` | `{attempt, image: [pack]}` | `export.image` | `CopyDone` |
| `StoreExportRelease` | `{attempt}` | `export.on` | `ReleaseExport` |
| `StoreOpen` | `{listed: [pack], removed: [pack], replayed: n}` | `files`, `gen` | `Crash` (the reopen half) |
| `StoreTicket` | `{seq, durable: true}` | `acked` | `StoreCatchUp!Sync` |
| `StoreIndexStep` | `{index, kind: demote\|chunk, supersede: bool, entries: [{op, key, prior}]}` | `xs`, `x`, `log` | `Demote`, `Build` |
| `StoreCatchUpRound` | `{attempt, from, to, steps: n, frames: n, result: ok\|Corrupt}` | `child`, `shipped`, `cErr` | `Round` |
| `StoreInstall` | `{attempt, index_entries: [key]}` | `child` | `EndExport` |
| `StoreJanitorLow` | `{stream, low, head_seen_at}` | `low`, `obs` | `StoreJanitor!RaiseLow` |
| `StorePlaceHead` | `{place, live: seq\|0, sealed: [[from, cutoff]]}` | `live`, `sealed` | `Attach`, `Seal`, `Settle`, `Report` |

The trace module's invariant is the same as fleet's: every recorded step is admitted by some
module action from the state the previous line left, and the run's last line is reached
(`l = Len(TraceLog) + 1`).
