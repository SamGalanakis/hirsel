# Store retention models

Model-checked specifications of Hirsel Store retention (`core/programs/store/STORE-SPEC.md`), and
the Python crash and stream models STORE-SPEC §3.9 cites. They are opt-in, not part of pre-push:

```sh
python3 proofs/store/check_proofs.py                # everything, about 17 minutes on 8 workers
python3 proofs/store/check_proofs.py --save-traces  # also rewrite traces/ from the known holes
python3 proofs/store/check_proofs.py --deep         # also the fork, export-lease and anchor baselines (the fork bound runs about 25 min)
python3 proofs/store/check_proofs.py --skip-python  # TLC only
python3 proofs/store/check_proofs.py --trace-only   # the retention trace checks alone, about 3 s
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
| `TraceStoreRetention.tla` | Trace validation: each row of a crash-sim retention trace is one `StoreRetention` action whose result equals what the sim observed (below). |
| `mutants.txt` | The seeded one-rule mutants and the invariant that must catch each. |
| `traces/` | TLC counterexamples for the v2.157 holes, written by `--save-traces`, and `idle-predicts-drain-f1.txt`, review 3177 F1's broken idle reading. `retention-trace-green.ndjson` is the crash sim's retention trace; `retention-trace-f1.ndjson` is the same trace hand-edited into F1's behaviour. |
| `python/` | The §3.9 executable models: `model_v2.py` (journal, checkpoint and damage cuts), `model_v25_packs.py`, `model_v29_refusal.py`, `model_v23_streams.py`, `model_v24_streams.py`, `model_v221_streams.py`, `model_v221_scope.py`, and `model-v2-mutants.txt` (the record of the hand-edited M1–M4 runs §3.9 cites). Each exits non-zero on a failed cut or a surviving mutant. |
| `check_proofs.py` | Runs the retention traces, the baselines, the known holes, the mutants and the Python models. A check passes only on its expected outcome: "No error has been found", or `Invariant <name> is violated` for the named invariant. |

## Rules under check

Each TLA+ module takes a `Rules` constant naming the reading of the spec it checks.

- **`StoreRetention`**
  - `v2157`: v2.157 as written. An idle step advances the cursor's row count, and a later reading probe resumes there ("Progress").
  - `v2157_restart`: the only other reading v2.157 admits. A reading probe restarts an idle-advanced cursor at row 0, which "Progress" forbids.
  - `v2164`: the current text (v2.161, D-TI-1, as amended by v2.164, review 3177 F1–F3; STORE-SPEC §13.6 "Tiered-idle cuts", lines 5405–5411 at v2.167). Idle is decided over the installed **and added** queue rows of the generation the cut publishes (F1), so the drain's own cut is never idle. An idle cut reads nothing, never installs or moves a cursor, and takes every listed class-3/4 pack in sequence order, selected or not (F2): with budget 1 it retires the lowest-numbered listed pack whole and deletes that pack's lineage cells (F3). The drain is optional (`dr`: a refused allocation or the free budget), and a cut may drop its probe progress (`pg`: a refused allocation or the mandatory-only retry), which drops the idle step too. A live-head fork (`fork`, `unfork`) is a holder whose row's watermark files kills into holder cells, so cells exist in an idle generation when the child's head is Memory. Promotion copies a frame of a pack an idle cut retired through the Snapshot's lease. Only a crash or a reselection resets a cursor.
  - `count3157`: review 3157's alternative, which v2.161 lists under "Rejected" as sound but costlier. Idle progress is a count apart from the probe cursor, and the first non-idle cut restarts it at row 0, as a third allowed reset. Kept as a second passing baseline, so the rejection's "sound" stays checked.
- **`StoreCatchUp`**
  - `v2157`: as written. A step ships "the collection rows it changed": for a Tiered index that is X's new root. The child installs it under the overlay its replayed frames built.
  - `v2161`: the current text (v2.161, D-CU-1, as amended by v2.167, review 3177 F4; §3.5a "Advance" and "Replay", §13.10a "Catch-up"). v2.167's amendments match the model as it stands: `apply_steps` drops main's overlay share only (the model has one branch and no `FORK` in the tail), the round's diff skips superseded ids (the model ships a superseded index's entries only as step rows and diffs nothing), and a superseded id whose new root is not empty is `Drift` (the model's only superseding step, the demotion, installs the empty root). A demotion step carries X as a superseded id, and `apply_steps` drops the child's overlay of X and installs the empty root (D-D7-5). A build chunk ships its entries, which the child applies against their priors as replay applies a frame. The §13.7 premise is scoped to holder reopen and log@1 followers; the prepare child is the exception.

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
| `CellsListed` | §13.6 "Probe completion and retirement"; "A Tiered-idle cut decides whole packs"; §13.6a T8 step 7 | No lineage cell names a pack that has left the list: a retiring cut, idle or reading, deletes the pack's cells (review 3177 F3). |
| `IdleProgress` | §13.6 "Progress" (v2.164) | A Tiered-idle cut that keeps its progress and lists a class-3/4 pack retires at least one, selected or not (review 3177 F2). |
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
| `StoreRetention` v2164 (retention bounds) | no error; 467,410 distinct states, depth 20, about 6 min |
| `StoreRetention` v2164, fork row on, one key, no crash (`--deep`, `retention_fork`) | no error; 1,634,153 distinct states, depth 26, about 25 min |
| `StoreRetention` v2164, fork row on, one key, one crash (run once by hand, not in the runner) | no error; 3,662,469 distinct states, depth 26, about 60 min |
| `StoreRetention` v2164, export lease on (`--deep`) | not rerun since the v2.164 fold (1,403,440 distinct states under the v2161 model) |
| `StoreRetention` v2164, anchors on, one key (`--deep`) | not rerun since the v2.164 fold (944,626 distinct states under the v2161 model) |
| `StoreRetention` count3157 (3157's rejected rule) | no error; 633,996 distinct states, depth 23, about 10 min |
| `StoreRetention` v2157 | `RetainReachable` violated: the 3160 hole (`traces/tiered-idle-v2157.txt`) |
| `StoreRetention` v2157_restart | `Progress` violated (`traces/tiered-idle-v2157-restart.txt`) |
| `StoreCatchUp` v2161 | no error; 577,192 distinct states, depth 15, about 7 s |
| `StoreCatchUp` v2157 | `SupersedePremise`, `NoFalseCorrupt` and `BackfillNoneEmpty` violated: 3150 F1 (`traces/catchup-f1-*.txt`) |
| `StoreJanitor` | no error; 229,799 distinct states, depth 19, 2 s |
| Mutants | 17 of 17 caught under the current rules, each in under 3 s; `idle_predicts_drain` is review 3177 F1's broken reading (`traces/idle-predicts-drain-f1.txt`) |
| Reachability witnesses | 11 of 11 reached on `retention_fork` with `Track = TRUE`, each in under 3 s |
| Python models (§3.9) | 7 of 7 pass; `model_v221_scope.py` takes about 28 s |
| Whole default run | 1,030 s (shared host; the two retention baselines are most of it) |

**The Tiered-idle hole (v2157).** A plain cut writes pack P1 with two frames. Overwrite the second row, then capture Snapshot S.
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
trace, r a→c is written before the export, so the child begins with it as an overlay. A
demotion's step installs the empty root under that overlay, leaving the child's X = {c} under
`Backfilling(None)`.
TLC runs with several workers, so `--save-traces` can write a different shortest trace on another
run.

## Pending model change: the build pin (STORE-SPEC v2.169)

v2.169 (§13.10a "Hot Tiered builds") adds a holder that neither model has. It is an owner-3 pin of fresh Hot pages that is never promoted, emitted by its own cuts, and grafted onto main by one `ReplaceMain` cut that drops it. The abstraction above, where one Snapshot pin stands for every pin, does not cover it: the import pin's own graft is not modelled either. So under the standing rule, `StoreRetention.tla` changes before the M3b-S5b implementation lane starts (DESIGN-v2 §4.4a gate):
- **Ops.**
  - `build`: a cut whose only holder change sets the build pin to a root of fresh stamps it emits.
  - `graft`: one cut that sets main's head to the pin's frames, drops the pin, and queues main's old Cold base as released roots.
  - The crash then reopen drops an orphan pin at the first holder-free cut.
  - A second collection that the plan leaves untouched, so that a probe at a `build` cut can relocate it.
- **Invariants.** The existing ones, unchanged: RetainReachable, SnapReadable, ImageComplete, AckedDurable, IdleReachesNoFrame, ProbeClaim, Progress, CellsListed, IdleProgress.
- **Mutants**, each required to violate one of them:
  - `graft_queues_pin`: the graft queues the pin's frames as released, so the drain retires frames main now names, which violates RetainReachable.
  - `graft_stale_root`: the graft publishes the untouched collection's staging-time root after a `build` cut's probe moved it, which violates RetainReachable.
  - `build_unlisted`: a `build` step writes its frames into a pack outside the cut's list (lane 3220's live `child_cut_tiered`). A crash then reopen removes the unlisted pack while the durable pin still names it, which violates RetainReachable.
- **`StoreCatchUp.tla` is unchanged.** The build pin exists only inside a hot `Migrate`, and the publish's lease step is `Drift`, so no catch-up round replays a hot build.
- **Not a model case.** An orphan rule keyed on `import` alone keeps a crashed build's pin listed forever. That is a liveness failure, and the models check only safety, so L102 (c) catches it instead.

## What is abstracted

- **One collection, small bounds.** `StoreRetention` has one Tiered collection of `NKeys` rows, one frame per row, a few stamps and packs, one crash. Pages, offsets, BLOB frames and the class-3/4 split are not modelled: a frame is a stamp at a PACK_INDEX row.
- **Budgets of 1.** `probe_budget` and `FreeBudget` are 1 row per cut, so a probe spans cuts, which is where both holes live. Larger budgets change only the step count.
- **Holders.** One pin name stands for every pin of a Snapshot (the import pin and `ImportPin` follow the same promotion and probe rules), and one live-head fork of main stands for the fork rows (its child's head is frozen at the fork). One Snapshot at a time. Anchors are optional (`WithAnchors`), and the export lease too (`WithExport`).
- **Accounting.** `live` and the deadlists are advisory (D-ZL-1). The model keeps `live` and retires only by a finished probe or a Tiered-idle cut, so the mutant that retires on zero live shows why. Lineage cells are holder cells of main's chain, one per frame (`k` the holder watermark, the pack, the stamp), filed, dropped and relocated as §13.6 says; origin cells, `m` keys and byte counts are abstracted (a frame's stamp stands for its `m`). Fork ghost rows are abstracted: `unfork` removes the row with the child's head. Recount probes, `repair_cuts` and `compact_ratio` arithmetic are abstracted: any listed pack without a cursor may be selected at any cut.
- **Reachability witnesses.** `ev` records what the last cut did. Baselines keep only the tag `IdleProgress` reads, so it adds no states. `check_proofs.py` reruns with `Track = TRUE` and requires TLC to violate each `No…` witness in the module, so each new behaviour (an idle retirement, of an unselected pack, of a pack with a cursor, of a pack with cells; an idle generation with a holder and cells; a dropped idle step; the drain emptying the queue at a non-idle cut; a skipped drain; `unfork` queueing roots; a filed and a moved cell) occurs.
- **Cuts are atomic.** A cut is one action in §13.6a's order. A crash inside a cut is the state before the MANIFEST rename (§3.6), which equals no cut. The pre-rename durability steps are `python/model_v2.py`'s and `model_v29_refusal.py`'s job.
- **Catch-up.** One row and one Many index, with priors reduced to "present" and "absent". A demotion and a build chunk are the only steps. Rounds apply everything not yet shipped. The pause, ROUND and PREPARED files, `apply_round`'s kept import and the account checks are abstracted. The child's install is checked as a state predicate whenever it is caught up.
- **Janitor.** One stream and one Place. Elections, TERM, XINDEX and the forced settle are abstracted; a crash forgets the head summary and pins are rebuilt from durable state.
- **No liveness.** Only safety is checked. A GC liveness property (every delisted pack is eventually removed once its leases end) is not.

## Retention traces

`test_store_retention_trace` (`core/programs/store/store_retention_trace_laws.hirsel`) runs one
fixed plan through the Store crash sim (`store_crash_sim_laws.hirsel`) with its recorder on, and
prints one NDJSON row per observed transition. The plan is one log@1 store: main goes Tiered,
takes 24 rows into one class-3 pack P, a Snapshot is captured, main goes Tiered → Memory with
`free_budget = 1`, cuts drain P's rows over four cuts while the Snapshot leases P, a
Tiered-idle cut retires P, the Snapshot is released, the next cut removes P's file, and a
crash and reopen follow. `compact_permille = 0`, so only the idle step can retire P. The crash
sim's own oracles (a)-(d) hold over the run.

| Row | Observed, where | Matched by |
|---|---|---|
| `cut` | After every cut (each checkpoint in `CS_CUT`, and any step whose call cut, such as `set_residency`): the installed generation's listed class-3/4 packs (`disk_state(db).pages.locations.packs`), the packs its deferred-free rows name (`l87_queue`), main's residency, and the class-3/4 pack files in the store directory on the simulated disk, plus the ones the call removed | `Cut(op, sel, dr, pg)` for some arguments, with `gen'.hres`, `gen'.listed`, the queue's packs and `files'` (present plus removed) equal to the row |
| `remove` | One per pack file a cut removed, from listing the store directory at the `std.sim.fs` boundary | `RemoveFile(p)`, apart from the cut |
| `capture` | `CS_CAPTURE` (and eviction from the held list as `drop`): the Snapshot's read inventory generation and its listed class-3/4 packs | `Capture` with `snap'.leased` equal to the row |
| `drop` | `CS_RELEASE`: the same for the released Snapshot | `DropSnap`, of a Snapshot leasing the row's packs |
| `reopen` | After a crash and a successful open: the installed lists and the files open left | `Crash`, with the installed lists and `files'` equal to the row |

The rows carry observations only, never the store's idle decision or what it retired: the
model's `Cut` decides from the observed generations whether a cut may retire a pack, so a
broken idle decision in the store cannot certify itself. `check_proofs.py` renumbers pack
sequence numbers 1..n in sequence order and writes the cfg (`NKeys = 1`, one `MaxPack` per
pack, one `MaxCrash` per reopen row; the queue is compared as the set of packs it names, and
the optional drain lets one key stand for P's many queued pages). A trace conforms iff TLC
reports `TraceUnfinished` violated; otherwise the search depth is the first row no action
admits, and the runner prints it. It also requires the trace to exercise the F1 window: at
least two cuts draining a held pack, a cut retiring a leased pack, a removal after the drop and
a reopen, with one cut per generation.

`retention-trace-f1.ndjson` is the green trace with the generation-6 cut (the first after
Tiered → Memory) retiring P while its queue still names P (review 3177 F1: the drain was predicted to empty the queue; it left P's
pages queued, and the Snapshot keeps the file, so reads stay green). The check refuses it at
that row, and the `idle_predicts_drain` model admits it, so the edit is F1's behaviour and not
a malformed trace. A removal is matched on its own: the runner also checks that the green
trace with P's removal moved before the drop is refused at the `remove` row.

The nightly board's laws stage (`tools/nightly-board.sh`) runs the law once, fresh, and
`check_proofs.py --trace-only --trace-from-tests` on its report; a red law or a refused trace
is a Store law red (about 10 s). Removal is observed by listing the directory after each call,
so it is ordered against the call's publication only by the cut row that precedes it.
