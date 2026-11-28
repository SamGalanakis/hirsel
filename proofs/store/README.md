# Store retention models

Model-checked specifications of Hirsel Store retention (`core/programs/store/STORE-SPEC.md`), and
the Python crash and stream models STORE-SPEC §3.9 cites. They are opt-in, not part of pre-push:

```sh
python3 proofs/store/check_proofs.py                # everything, about 17 minutes on 8 workers
python3 proofs/store/check_proofs.py --save-traces  # also rewrite traces/ from the known holes
python3 proofs/store/check_proofs.py --deep         # also the fork and export-lease baselines (the fork bound runs about 25 min)
python3 proofs/store/check_proofs.py --skip-python  # TLC only
python3 proofs/store/check_proofs.py --trace-only   # retention and archive traces alone
```

TLC is the nightly `tla2tools-nightly.jar` plus `CommunityModules-deps.jar` under
`~/.local/opt/fleet-formal` (or `$FORMAL_TOOLS`), as for `proofs/fleet`; nothing is vendored here.

**Standing rule (STORE-SPEC §3.9).** A STORE-SPEC revision that touches retention (the probe,
deadlists and Tiered-idle cuts, promotion, leases, export and catch-up, archive holds, retirement or the
janitor) updates these models to the new text and passes `check_proofs.py` before an
implementation lane starts.

## Artifacts

| File | What it checks |
|---|---|
| `StoreRetention.tla` | Tiered pack retention: commits, cuts and their publication, pins and forks, Snapshots and their leases, the candidate queue, Tiered-idle cuts, the compaction probe, promotion of a historical Snapshot, demotion Tiered → Memory, AP16 Remove, the export lease and its copy, a projector image carried as a Snapshot's tail base (`WithTail`), crash and reopen between any two steps. |
| `StoreCatchUp.tla` | Export catch-up and install over a Tiered index across a demotion and a rebuild (§3.5a, §13.10a, §13.7 D-D7-5), the holder's own replay and fold, and recovery of a commit prefix at or past the last acknowledged ticket. |
| `StorePreviewHold.tla` | Preview holds in `archive-head` over a service that compares only a tag (§15.7 "Preview holds"): admission, extension, retirement, the floor and erasure against the archiver, time, a skewed operator clock and an archive erased and created again. `StorePreviewHold.cfg` is the baseline (`tools/tla-check.sh -deadlock proofs/store/StorePreviewHold.tla`) and `StorePreviewHoldNaive.cfg` lane 3435's renewal, which must violate `NoResurrection` (`tools/tla-check.sh -deadlock -config proofs/store/StorePreviewHoldNaive.cfg proofs/store/StorePreviewHold.tla`). The bounds end every behaviour, so both need `-deadlock`. |
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
| `RetainReachable` | §13.6 "The probe"; §13.5 step 2 "Resolution frees nothing"; §1.9 | Every frame a durable holder names (a head, a pin or fork, a queue row) is in a listed pack whose file exists, at the PACK_INDEX row that holds its stamp. Nothing reachable is retired or reused. |
| `SnapReadable` | §13.5 holder leases, "Tail bases are protected too" (v2.177); AP16 Remove | An open, unexpired Snapshot's frames stay on disk, through its primary and its tail base alike, and promotion never reads a removed frame. |
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
| `HeldReadable` | §15.7 "Preview holds", Meaning and Floor | Every hold the head lists names an advertised boundary whose closure is stored. No clock appears in it. |
| `NoResurrection` | §15.7 "Preview holds", Reader | A child that reported expiry is never served again, by a late extension, by a new hold under its ID or by an archive erased and created again. |
| `PromiseKept` | §15.7 "Preview holds", Retire and "What a clock decides" | Within one incarnation, no hold leaves the head before the true time reaches its deadline. An action property; it holds only while Grace ≥ Skew. |
| `HoldStable` | §15.7 "Preview holds", Extend | While listed, a hold keeps its boundary and stamp, and its deadline never moves back. An action property. |
| `HeadNeverRepeats` | §15.7 "Preview holds", Record | Every head write moves `(incarnation, rev)` up, so a replaced head never returns and a PUT against its tag never applies. An action property. |
| `HeadBatchesKept` | §12.11.8 Invariants and the release rule | No batch a Place's head contains (`live` or `sealed`) is below `low`. |

## Results (bounds in `check_proofs.py` `BOUNDS`)

Counts are from one run with 8 workers. `check_proofs.py` prints them and never asserts them.

| Check | Outcome |
|---|---|
| `StoreRetention` v2164 (retention bounds) | no error; 467,410 distinct states, depth 20, about 6 min |
| `StoreRetention` v2164, fork row on, one key, no crash (`--deep`, `retention_fork`) | no error; 1,634,153 distinct states, depth 26, about 25 min |
| `StoreRetention` v2164, fork row on, one key, one crash (run once by hand, not in the runner) | no error; 3,662,469 distinct states, depth 26, about 60 min |
| `StoreRetention` v2164, export lease on (`--deep`) | not rerun since the v2.164 fold (1,403,440 distinct states under the v2161 model) |
| `StoreRetention` v2164, build pin on (`retention_build`) | no error; 302,662 distinct states, depth 23, about 4 min |
| `StoreRetention` v2164, tail base on (`retention_tail`) | no error; 304,456 distinct states, depth 23, about 90 s |
| `StoreRetention` count3157 (3157's rejected rule) | no error; 633,996 distinct states, depth 23, about 10 min |
| `StoreRetention` v2157 | `RetainReachable` violated: the 3160 hole (`traces/tiered-idle-v2157.txt`) |
| `StoreRetention` v2157_restart | `Progress` violated (`traces/tiered-idle-v2157-restart.txt`) |
| `StoreCatchUp` v2161 | no error; 577,192 distinct states, depth 15, about 7 s |
| `StoreCatchUp` v2157 | `SupersedePremise`, `NoFalseCorrupt` and `BackfillNoneEmpty` violated: 3150 F1 (`traces/catchup-f1-*.txt`) |
| `StoreJanitor` | no error; 229,799 distinct states, depth 19, 2 s |
| `StorePreviewHold` (`preview`: `LastTime = 2`, `MaxPut = 5`, `MaxWipe = 1`) | no error; 1,224,763 distinct states, depth 21, about 23 s |
| `StorePreviewHold` at `LastTime = 3`, `MaxPut = 6` (run by hand under `tools/tla-check.sh`, not in the runner) | not finished, and measured on the v2.184 model, not run again for v2.187: no error in 14,389,333 distinct states when the 300 s cap stopped it. With `MaxWipe = 0` the same bound passes: 7,541,572 distinct states, depth 24, about 160 s, the v2.182 count. No eraser is ever old at `MaxWipe = 0`, so v2.187 leaves that state space as it was |
| `StorePreviewHold`, operator clock unbounded (`Skew = LastTime`, `Grace = 0`) | `ArchiveComplete`, `HeldReadable`, `NoResurrection` and `HoldStable` hold (1,906,707 distinct states, depth 21, about 35 s); `PromiseKept` is violated at depth 7, which is the clock assumption |
| `StorePreviewHold` mutants and witnesses | 5 of 5 caught and 11 of 11 reached, each in under 5 s. `preview_flat_keys` also violates `ArchiveComplete` (run once by hand, 7 states: the old eraser needs no hold to break the new head) |
| Mutants | 20 pre-archive controls caught in this recorded run, each in under 3 s; `idle_predicts_drain` is review 3177 F1's broken reading (`traces/idle-predicts-drain-f1.txt`) |
| Reachability witnesses | 16 of 16 reached with `Track = TRUE`, each in under 3 s: 11 on `retention_fork`, 3 on `retention_build` (a build cut's probe moves U, the graft leaves a pin frame out, a holder-free cut drops an orphan pin), and 2 on `retention_tail` (an image-only pack retires under a live capture, and its file goes after expiry) |
| Python models (§3.9) | 7 of 7 pass; `model_v221_scope.py` takes about 28 s |
| Whole default run | 1,197 s (shared host; the two retention baselines are most of it) |

**The Tiered-idle hole (v2157).** A plain cut writes pack P1 with two frames. Overwrite the second row, then capture Snapshot S.
A Tiered → Memory cut hands the head's Cold root to the queue. A cut pins S (promotion copies S's
unflushed row into pack P2) and drains the queue. Unpin S, selecting P1 for a probe: the published generation has no Tiered holder, no
queue row, so it is Tiered-idle, and the probe step advances P1's cursor one row
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

## The build pin (STORE-SPEC v2.169, amended v2.171)

§13.10a "Hot Tiered builds" adds a holder: an owner-3 `build` pin of fresh pages, never promoted, set by its own build cuts and grafted onto main by one `ReplaceMain` cut that drops it. `StoreRetention` models it under `WithBuild` (bound `retention_build`: one key of the built collection plus an untouched collection U, one crash; Snapshots are left out of this bound because the pin shares no page with main):
- **Ops.**
  - `BeginBuild` keeps the staging head's root of U. `BuildWrite` writes a target row as a Hot page with a writer stamp, and `BuildDrop` leaves a pin frame out of the working map (a replaced tail, a chain's intermediate collection or a scratch run). `BuildAbort` is any error return: it forgets the build and leaves a listed pin as an orphan. No commit, Snapshot or pin runs during a build, because the call holds `mut db`.
  - `build`: a cut whose only holder change sets the build pin to the working map, which is fresh stamps the cut emits plus a subset of the previous pin's frames (review 3228 F7). The build continues from the pin's tree as the cut published it, and the probe may relocate U and the pin's frames.
  - `graft`: one cut that sets main's collection to the working map (review 3228 F1), gives U main's current root (untouched, so judged against the staging head), drops the pin and queues the source's old Cold base as released roots. Pin frames the graft leaves out are in neither main nor a pin afterwards.
  - A crash forgets the build. A holder-free (`plain`) cut drops an orphan pin.
- **Invariants.** The existing ones, unchanged. A listed build pin is a Tiered holder, so no generation that lists one is Tiered-idle.
- **Mutants.** `build_unlisted` (3220's live `child_cut_tiered`) and `graft_stale_root` (review 3228 F3) are in `mutants.txt`, and RetainReachable catches each.
- **Not model cases: release accounting.** 3231 listed two more mutants: `graft_queues_pin` (the graft queues the pin's frames as released) and `build_replace_frees_shared` (a `build` that replaces the pin releases every frame of the old pin, including those the new pin still names). Both survive the full `retention_build` bound, at 322,600 and 368,931 distinct states. In this model a durable frame is lost in only three ways: a probe finishes its pack while no participating root names the frame, a Tiered-idle cut takes its pack, or open removes an unlisted file. Queueing a frame adds a participating root, and the drain only files kills, which are advisory (D-ZL-1). So no §13.6 rule lets the drain retire a frame that main or a pin still names. Both mistakes are accounting drift (`live` and the cells), not loss, and no safety invariant can see them. No L102 case checks build accounting either; they are recorded here only.
- **`StoreCatchUp.tla` is unchanged.** The build pin exists only inside a hot `Migrate`, and the publish's lease step is `Drift`, so no catch-up round replays a hot build.
- **Outside the model.** Hot pages and stamp counters are not modelled. So review 3228's F1 (a graft that drops the Hot tail) and F2 (split stamp counters) are L102 cases (`graft_installed_pin`, `plan_counter_rollback`), not mutants.
- **Not a model case: liveness.** An orphan rule keyed on `import` alone would keep a crashed build's pin listed forever. That is a liveness failure, and the models check only safety, so L102 (c) catches it instead.

## Preview holds (STORE-SPEC v2.182, amended v2.184 and v2.187, text only, not yet built)

§15.7 "Preview holds" lists each by-reference Preview child in `archive-head`. `StorePreviewHold` extends the archive projection with the list and with head writes as the object service sees them: a PUT names the tag its issuer read, applies later or never, and applies only while the tag is unchanged. Spec `PreviewSpec`, bound `preview`.

- **Start.** Base 1 with journal unit 2 is published, and base 3, which shares a pack with base 1, is uploaded and unpublished. Two children. A hold is a boundary (2 or 3; boundary 1's closure is inside 2's), a deadline and the `rev` that admitted it. The head carries an incarnation and a `rev`, and the service's tag is the pair.
- **Ops.**
  - `IssueAdmit`, `IssueExtend` and `ApplyAuth`: the hold authority. An extension has no clock precondition. `Mount` publishes a child's genesis with an incarnation and a stamp the head once listed.
  - `IssuePublish` and `ApplyPublish`: the archiver, an ordinary head writer that changes the tag and carries the holds.
  - `IssueExpire` and `ApplyExpire`: the operator. One PUT retires every hold its clock sample puts at or past the deadline plus `Grace` and may move the floor, never past the founding base of a hold it keeps. The sample is any value up to `now + Skew`: the head read and the clock read are separate steps in either order, and a sample may be arbitrarily old.
  - `Erase` is the archive projection's erase under a published floor. `ReadHead` is a child's exact head read, with no clock.
  - `Recreate` erases every archive object, the head with them, and creates the archive again from the unchanged source: the first head, `rev` 1, no holds, the next incarnation. PUTs issued before it stay in flight. At most `MaxWipe` times, and it counts toward `MaxPut`. It also moves every applied floor to `stale`, tagged with the incarnation that carried it, so an eraser outlives the head it wrote.
  - `OldErase` is that eraser. It names any key by the ordinal rule of its old floor and skips the survivors it verified. It deletes in the directory of its own incarnation (§15.7 "Preview holds", Keys), which is never the current one, so the step changes nothing the current head stores.
  - `Tick` advances true time.
- **Crashes.** Issuers keep nothing between steps, so a crash or a restart is an issuer that stops stepping while its PUT stays in flight. TLC covers that without a crash variable, because no step is forced. Each role has one PUT in flight and a newer one replaces it. That loses no behaviour: two PUTs against one tag cannot both apply, and issuing changes nothing else. An applied PUT drops the others, which is sound because `HeadNeverRepeats` holds.
- **Clocks.** `HeldReadable` and `NoResurrection` hold with the operator clock unbounded, so removing a hold at any time (an early retirement, or the release when a child is deleted) is safe for every other hold. `PromiseKept` alone needs `Grace ≥ Skew`.
- **Mutants.** `preview_naive_renewal` (lane 3435's design: the authority checks the clock before it sends an extension, and a child also reports expiry at the deadline), `preview_no_clamp`, `preview_unstamped`, `preview_no_incarnation` and `preview_flat_keys` are in `mutants.txt`. The first reproduces 3435's trace after an admission: extension sent, tick, expiry reported, extension applied. The last is the v2.182 text, whose tag is `rev` alone and whose reader compares only the stamp. It reproduces lane 3450's law: admission at `rev` 2, the archive erased and created again, expiry reported, the same child ID admitted at `rev` 2 again. `preview_flat_keys` is the v2.184 text, in which every incarnation stores its keys in one namespace. It reproduces lane 3450b's law, and a shortest trace has 9 states: base 3 published, the floor moved to 3, the archive erased and created again, a Preview admitted at boundary 2, then the old eraser deletes unit 1, which is below its floor and inside the new hold's closure.
- **Witnesses.** `Watch` names one behaviour and `NoWitness` must be violated for each name in `check_proofs.py` `PREVIEW_WATCH`: a retirement applied over an extension in flight and the reverse, an extension of a lapsed hold, an archiver write over other PUTs in flight, a child reporting expiry, an erase beside another child's live hold, a re-admission of a mounted child, a genesis published after its hold left, a child reporting expiry at a head that lists its stamp under another incarnation, a recreated head at the `rev` a PUT in flight was issued against, and an old eraser naming a key in the closure of a hold the recreated head lists.
- **Outside the model.** The head's other fields, the `Applied`, `Conflict` and `Uncertain` answers (an issuer learns its outcome only by reading again, which the stateless issuers already are), the hold limit, Gap, rebinding and the child's own store. Eventual retirement of a lapsed hold is liveness, and only safety is checked. `Recreate` is one step, so the request that creates a head is never in flight: a copy of it delivered after the head is erased brings the old incarnation back, as does a Bucket restored to an earlier state, and §15.7 puts both outside the rules. The model's tag is the head's identity, which is the spec's assumption that equal tags mean equal bytes. The sink is the current incarnation's directory. Older directories, the uploads that arrive in them late and their reclaim are not modelled, and nothing reads them. In the baseline `OldErase` changes nothing by construction: the model states the directory rule and does not derive it. What it checks is that the old eraser is live across `Recreate` (the witness) and that one namespace breaks `HeldReadable` and `ArchiveComplete` (the mutant). There is no index key: a unit whose cursor starts again stands for an index page whose ordinal starts again. `Recreate` folds the write that creates the empty head into the first publications, so writes issued against those intermediate heads are not separate cases. A second erase is not in the runner's bound. `MaxWipe = 2` was run once by hand at the same `MaxPut = 5` and passes (1,843,165 distinct states, depth 21, about 35 s); each recreation counts toward `MaxPut`, so two leave three other writes.

## The tail base (STORE-SPEC v2.177, text only, not yet built)

§13.3 "The captured read roots" lets a Snapshot whose primary is Memory read a Tiered projector image as a tail base, and §13.5 "Tail bases are protected too" extends the Snapshot's lease to it. `StoreRetention` models it under `WithTail` (bound `retention_tail`: one key, three stamps, two packs, one crash; three packs did not finish in 25 minutes):
- **Ops.**
  - `image`: a cut that publishes main's live head as the projector's image pin `img`, a Tiered holder like a pin (§13.5 "Pinning the live head").
  - `unimage`: a cut that replaces or releases the image. Its frames lose their durable holder, so the probe may retire the image-only pack.
  - `Capture` also runs with a Memory head (after `toMemory`) while the image is held, and carries the image as the Snapshot's tail base. The lease covers the listed packs whenever the captured set reaches a Cold slot.
  - `Expire` ends the lease while the Snapshot is held. Its Cold reads then answer `Reclaimed(floor)`, and it cannot be promoted. `pin` promotes only a Tiered-primary, unexpired Snapshot.
- **Invariants.** `SnapReadable` covers the tail base as it covers the primary, for an unexpired Snapshot. A listed image is a Tiered holder, so no generation that lists one is Tiered-idle.
- **Witnesses.** `NoTailRetire` (an image-only pack leaves the list while a live Memory-primary capture still reaches it) and `NoTailReclaim` (its file is removed after the capture expires) must both be violated with `Track = TRUE`, so the case is not vacuous.
- **Mutant.** `tail_lease_ignored`: the lease is taken only when the primary reaches Cold slots (v2.176's reading). `SnapReadable` catches it at depth 9. The shortest trace needs no release: write, publish the image, demote, capture (Memory primary, image as tail base), a probe that relocates the image's frame and retires its pack, then open's removal of the file. The capture still holds its original shares, as §13.5 says.
- **Outside the model.** One branch, so the tail base is main's own image; other projectors and other branches' tails are not modelled. Re-execution, the pin map and the miss source (§13.3) are attempt state, not retention, and are law rows (§13.12 L106–L110), not model cases.

## What is abstracted

- **One collection, small bounds.** `StoreRetention` has one Tiered collection of `NKeys` rows, one frame per row, a few stamps and packs, one crash. Pages, offsets, BLOB frames and the class-3/4 split are not modelled: a frame is a stamp at a PACK_INDEX row.
- **Budgets of 1.** `probe_budget` and `FreeBudget` are 1 row per cut, so a probe spans cuts, which is where both holes live. Larger budgets change only the step count.
- **Holders.** One pin name stands for every pin of a Snapshot (the import pin and `ImportPin` follow the same promotion and probe rules), and one live-head fork of main stands for the fork rows (its child's head is frozen at the fork). One Snapshot at a time. Optional holders include the export lease (`WithExport`) and the hot build's owner-3 `build` pin (`WithBuild`, above).
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

## Point-in-time archive (§15.7)

`ArchiveSpec` in StoreRetention projects a bound writer onto three stable cuts, with one journal unit between cuts. Each cut allocates a new pack and can share any subset of its predecessor's packs. This is the A2 allocation premise. The source, sink and committed head are separate. Source files match the selected base plus any durable hold after reopen. A pack costs two byte quanta and a manifest or journal chunk one. The additional-byte budget is four. History and the set of previously uploaded keys are model oracles. A source cut may release a receipt while the sink is unavailable.

| Invariant | Bad transition it detects |
|---|---|
| `ArchiveComplete` | An interval lacks its independent base, a complete successor unit or a referenced pack. |
| `ArchiveIntervals` | A new interval silently follows an unclosed interval, intervals overlap, or floor/through disagree with the advertised endpoints. |
| `ArchiveBounded` | Additional source bytes exceed the hold budget, counting shared packs once. |
| `ArchiveSourceReadable` | Retirement or cleanup erases a still-held source file. |
| `ArchiveStagedSafe` | An erase using a current or older published floor deletes a staged object needed by a future unit. |

`traces/archive.ndjson` is a design fixture starting at the first unpublished base. Each `archive` row records a cut, one key upload/removal, reopen inventory or publish CAS. A `gap` row closes the interval at the stated cursor. An `expire` row records either the floor CAS or one erase under an observed floor. The actions run separately, so TLC interleaves a publisher with delayed erases. The fixture loses a successor past budget, reopens, closes with Gap, publishes a fresh base and expires the old interval. Omit the pack upload before the first publication and the correct model rejects that CAS. The publish-early mutant admits it. Run a supplied archive trace with `--trace-only --trace FILE`.

This projection checks stable retention decisions. The thirteen Directory barrier interleavings, S3 response ambiguity, resolver and restored-child adoption are product law obligations in STORE §15.8.

## Format 15 direct and foreign packs

`StoreRetention.Closure` reads each unit's own set of pack identities. A GROUP can introduce a pack before a base lists it. A base records its allocation frontier. `Live` includes the selected base and its uncovered GROUP. `CoveredCut` transfers dependencies to the new base or to the bounded archive hold. Archive publication, source reclaim and delayed expiry all use the same unit history. ArchiveComplete compares the advertised intervals against that immutable history.

| Projection | Bounds and observations |
|---|---|
| `StoreDirectPack.DirectSpec` | Four pack ordinals, up to three bases, one row, one reader lease, one durable holder and one outstanding introduction. Reserve, seal, commit, erase, cut, abandon, crash/reopen, source removal, archive upload and CAS are distinct transitions. A Pin or DeferredFree row contributes the same physical obligation here. The Tiered model checks their individual cut mechanics. Reopen loses the volatile hold, resets allocation to the durable frontier and sweeps after recovering the prefix. Restore can serve a journal-only pack before the source cuts. Follow selects a captured GROUP whose source files may since have been reclaimed. |
| `StoreDirectReplay.Spec` | Up to four live ops. An ordinary edit draws one stamp live and one or two on replay. A direct edit carries its fixed stamp. TLC checks unique live frame stamps and the allocator's lower bound for full and incrementally extended prefixes. A witness requires a real collision with an earlier private draw and a follower re-base. |
| `StorePreviewHold.PreviewSpec` | The held boundary at cursor 2 now includes a pack that neither adjacent base lists. The existing hold races, floor clamp and incarnation-scoped eraser operate on that pack too. |
| `StorePreviewPack.ForeignSpec` | One child and Snapshot, one held journal pack, and an existing pack outside the held boundary. Try foreign locators under the wrong owner, incarnation or boundary. Copy-on-write produces a child-owned file. Reclaim, capture, release, archive damage and an exact-head read interleave. Witnesses require a Snapshot reading the parent **after** compaction followed by actual removal of the child-owned file with the parent pack still present, loss while held, and expiry. |

The direct controls exercise commit before seal, sweep before replay, stale abort, publication without a GROUP's pack, follower advance without fetching the pack, and both replay stamp collisions. The foreign controls admit a valid object outside the held closure or let the child erase its parent's pack. Each row of `mutants.txt` names the invariant that must report the error. The default `python3 proofs/store/check_proofs.py` runs these baselines, controls and witnesses with real TLC.

A model pack is an immutable identity. `Seal` stands for the completed file+directory barriers or exact object creation. A model GROUP stands for an accepted prefix after native framing and D6 have selected its end. `EraseValue` projects a later logical deletion in that suffix without changing its pack dependencies. File contents, checksums, provider fences, host-stream cutoff selection, allocator exhaustion, I/O budgets, latency and fair cleanup scheduling require product laws. The child projection models validated coordinates and reads with an exact head in hand. The hold projection separately checks creation, concurrent CAS, expiry and physical recreation. These are bounded safety results.
