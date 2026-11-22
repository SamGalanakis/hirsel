# STORE-SPEC v2.40: the M4 store library, final form (lane store-spec-1001)

v2.40 (lane store-spec-v240-1693, from store-c-review-1692 findings 1–13 and the orchestrator's two capacity rulings of 2026-11-22): text only, **not yet built**. §11 is corrected so M1-4 can be built from it:
- PREPARED's 0x0081 payload has a format-3 layout carrying NEW's four limits (AP1b).
- A cut encodes one prospective set of holder records (AP16).
- `serve` and `conclude` have their own journal refusals, which never become resident `NoRoom`.
- Segments are read by a bounded streaming reader.
- The journal floor has a restart precondition and overflow-free arithmetic.
- The cut's plan has a dependency order and a retry base.
- AP21 precedes every cleanup, the prepare opener and NEW's output.
- A CALL's stream has one op, and W2 uses the last published generation.
- Three laws get independent oracles, the cap check moves to final f, and the index list is bounded by occupied bytes.

Every capacity refusal now carries `StoreCapacity = Journal | Resident | Allocation` through its operation's own result (§11.16). See CHANGES-v2.40.

v2.39 (lane store-m1-spec-c-1671, from schemasmash-durable-1683 findings 1 and 2): text only, **not yet built**. **AP21 is restored** as the format-3 reader's validation procedure, which v2.38 removed by mistake. A parsed MANIFEST and its decoded pages become a validated inventory and live graph only after its checks, and open, cleanup's retirement and an install's pre-activation check consume that validated value. **A journal CALL now rides the one Replied stream op that owns it** (AP1a): COMMIT section tag 2 is retired in format 3, and the streams section gains op tag 3, a Replied put carrying its call, so no CALL can name an Admitted put, a delete or a missing op. `recorded` keeps its result and the one-CALL rule.

v2.38 (lane store-m1-spec-c-1671, Sam's gavel ruling 1 of 2026-11-22, option C): text only, **not yet built**. §11's checkpoint becomes **bbolt-style paused incremental copy-on-write** (§11.0 choice 7, AP16): the writer plans and writes every cut itself while it is paused, writing only the frames no earlier cut wrote, and the format, L5's write bound and atomic `delete_branch` are unchanged. **A refused cut loses nothing**: the old MANIFEST plus the journal still recover every committed write, and the journal keeps growing until a cut succeeds. **Writes are refused at once at the journal cap** (`journal_limit`, default 256 MiB), with c = `Capacity{requested: U + f, available: journal_limit}`: `Refused(c)` from `run` and `run_on`, as a `resident_limit` refusal surfaces, and `Full(c)` from `admit` and a live-head `fork` (AP15). The checkpoint reserve, the credit charged on commit, migrate, fork, pin, open and install, the checkpoint task and its fixed workspace, generation-delta staging and bare rotation are deleted, and the v2.36 and v2.37 clauses that existed only for them are withdrawn (CHANGES-v2.38). v2.35's accounting wording stands: `resident_limit` bounds the logical account, journaling buffers have their own fixed bounds, page memory is reported and not admitted, and a live-head fork is O(1), one group sync. It answers the fifth rereview (`store-wholehog/m1-review-1625/REREVIEW-5.md`, U1–U13) by removal or by the rows below; `SHAPE-1644.md` beside it has the comparison of options.

v2.37 (lane store-m1-spec-fix-1629, round 5 of the M1 review): text only, **not yet built**. **v2.38 withdraws its reserve, staging, plan hand-off and bare rotation.** §11 answers astra's fourth rereview (`store-wholehog/m1-review-1625/REREVIEW-4.md`). **Planning is funded by a checkpoint reserve proportional to the physical state** (§11.0 choice 7, AP16, T9): every existing pack, unlocated frame and root holder holds credit, paid before the operation that creates it publishes, and the size proof in AP16 shows every plan fits, so no covering cut is refused by the writer or its task, and a `delete_branch` spends credit it already holds. Data relocation is the one discretionary step: it is funded when the closure is known and deferred if the writer's limit refuses it. **Staging is a generation delta** (T1): the installed view is untouched until the ack, and the only exits after staging are the ack and poison. Records hold no body share, so private edits stay in place (T4); the payer and release of every piece of writer state are tabulated (T5). Every segment is bounded by rotation alone, with a bare rotation while a cut is in flight (T9). The plan is an owned value moved to the task, its checksums filled children first (T8); list pages moved unchanged keep their stamps and only reachable list pages are emitted (T3). Part 1 of v2.37 (earlier on main) fixed the lease removal interval (T2), pack-contiguous emission and the workspace W(`pack_bytes`) (T6), commit-op ordering by index (T7), install's pre-activation checks (T10), class-2 pointer confirmation (T12) and L28 (T11).

v2.36 (lane store-m1-spec-fix-1629, round 4 of the M1 review): text only, **not yet built**. **v2.38 withdraws its checkpoint task and fixed workspace.** §11 answers astra's third rereview (`store-wholehog/m1-review-1625/REREVIEW-3.md`). **The writer plans every cut and the checkpoint task executes the plan in a fixed workspace** (§11.0 choice 7): staging, selection, the closure, the list edits, the list pages' stamps and every frame's location are decided on the writer, over checkpoint records the writer allocates with the frames they describe, and the task only encodes, writes, syncs, renames and removes. So a checkpoint is never refused for memory by its task: `checkpoint_task_bytes` has a fixed minimum that open enforces, and §3.5 Q1's `Refused` ack has no M1 cause (N8, S4, S5). The planning's own list pages, pack rows and retired seqs are the named residual: writer memory with no reservation. List pages take their stamps on the writer before the watermark is captured (S1); the retirement list is rebuilt whole at every cut, so retiring a class-2 pack can never empty another (S2); open checks that listed, retired and reached class-2 packs are pairwise disjoint and below `next_pack` before any removal, and streams each reached class-2 file once (S6, S7); the encoder writes each frame into one output buffer (S3); L5 counts list pages (S8); list rows are inline whatever the threshold and the resident report starts from the list roots (S9); replayed FORKs keep the record-byte total (S10).

v2.35 (lane store-m1-spec-fix-1629, round 3 of the M1 review): text only, **not yet built**. §11 answers astra's second rereview (`store-wholehog/m1-review-1625/REREVIEW-2.md`). **The MANIFEST's pack and retirement lists become two page trees** in class-2 packs found from two MANIFEST roots, so the MANIFEST is 152 bytes plus branch and pin records, one size expression is checked by every producer of a record, and no cut is ever refused for MANIFEST size (N8, H14, R5; §11.0 choice 6). No single figure bounds process memory any more: the frames handed to the worker, the frame being built and the worker's group buffer each have their own fixed bound (R1). `migrate` charges its catalog rows and publishes the exact account of its proposed roots (R2); install classifies each id as remove, replace or keep, so a prepared drop passes (R3); the fork preparation belongs to the live-head arm only (R4); location-table links name stamps, so relocating a parent changes no child's link (R6); `accounts.exact` is cleared by a conservative delete (R7); the bulk build passes real separators up (R8); and H8, H9, H13 and N3's laws are corrected.

v2.34 (lane store-m1-spec-fix-1629, round 2 of the M1 review): text only, **not yet built**. §11 answers astra's re-review of v2.33 (`store-wholehog/m1-review-1625/REREVIEW.md`). The conflict between delete-only admission and exact disk frame accounting is settled by one end state: `resident_limit` bounds the logical account alone, and journal frames in flight have their own `FRAME_LIMIT` with backpressure, so a delete-only commit is never refused on disk or in memory and a frame never causes a refusal (N2, N3; §11.0 choice 5). A cut's ack adjusts the account by its own capture's difference, so a delayed ack never erases newer charges (N1). Install gains a **keep** edit for collections whose identity and contents survive a schema-only release (N7). Also: the MANIFEST budget splits into branch/pin records and lists (N8, H14); `fork` prepares everything before it sends and installs infallibly after the sync (N5); relocation stages fresh records before selection (N13); the private page holds one body per live BlobRef (N11); the incremental split keeps two entries per piece (N6); the settle is one root walk of the map (N10); and the partly resolved B4, B5, H1, H2, H5, H6, H8–H10, H13, H14, M2–M4, M7 are closed in their laws. The built §1.11 sentence that frames sent and not acked count in the account is superseded for M1 by AP15.

v2.33 (lane store-m1-spec-fix-1629, the M1 review): text only, **not yet built**. §11 answers the astra review of v2.32 (`store-wholehog/m1-review-1625`): the admission unit becomes the distinct row version, so the account is independent of page partition and copy-on-write history and a delete-only commit is still never refused (B4, B5); a live-head `fork` syncs its FORK frame before it returns the branch (B1); relocation finds its closure by live parent links over physical frames, and open resolves only live edges by location (B2, B3); a collection-map leaf owns its collections' roots (B6); plus the fourteen high and eight medium fixes. Clauses AP1–AP21, laws L37–L51. See CHANGES-v2.33.

v2.32 (lane store-m1-spec-1618, the M1 target): text only, **not yet built**. §11 specifies the one format cutover of the store roadmap's M1 (`store-wholehog/MASTER-SPEC.md`): packed copy-on-write pages, per-collection roots, `PagePtr`, `BlobRef` with a 1 KiB inline threshold on 4 KiB leaves, the reserved `base` root and the journaled live-head `fork`, as clauses AP1–AP20 and laws L37–L48. §0–§10 still describe the store as built, and the M1 cutover slice flips §11 to built. See CHANGES-v2.32.

v2.31 (durable-cut spec lane; tape Durable mode is removed, so the store is the one durability contract): text only. `admit` now reserves its open entry before it commits, so §1.8 holds as written (AO1). §1.8's `Admission` comment no longer cites REPLAY-SPEC rule S3 (AO2), and §1.13 names the trigger refusal `StaleAttempt` and the run state "held" (AO3). See CHANGES-v2.31.

v2.30 (Sam's ruling C, auth-remove-1423; RUNNER-SPEC v2.19): the store does no authorization. Programs decide access with the Caller. AN1 deletes key scopes, the policy, the authority register and every scope digest. AN2 gives a stored reply 41 + R bytes and the format word 2. See CHANGES-v2.30.

v2.25 (gap study 1117 #6, lane service-ops-spec-1127): keyset pages over a Snapshot, `scan_page`/`find_page` with row and byte limits and an in-process continuation `Resume`, resumed by `scan_next`/`find_next` strictly after the last full logical key, with `Moved` for another Snapshot and `Stale(id)` for another incarnation (§1.5, L36). Text only; nothing about the format changes.

v2.24 (Sam ruled C on request identity, DECISIONS 2026-11-17; the rule is astra-req-digest/ADVICE.md, "C's complete rule" and "Byte cost"): the store binds each admitted position to a request digest. `admit` takes the digest; the `streams` value becomes (position, digest, Admitted | Replied(reply)); at an equal `p` the digest is compared before `Duplicate` or `Abandoned`, and a mismatch is `RequestMismatch`; below latest the answer is `HistoryUnavailable` (AH1, AH2). This replaces AG2's client-only rule. **The format changes:** a `streams` value grows by 32 bytes (AH3). No store writes a `streams` row yet: on main (f6d9de64) `store.hirsel` keeps an empty `streams` tree, and the runner's S5 stand-in (`core/programs/store/runner.hirsel:349`) keeps positions in an ordinary table. So nothing migrates. **Decision of this version, not of the ruling:** the digest's byte encoding and the place of `request_digest` (a store function, so the runner and the store hash one way) are chosen here. Reversible until S5 lands; RUNNER-SPEC should cite §1.8 for the encoding rather than define one. The names `RequestMismatch` and `HistoryUnavailable` are RUNNER-SPEC v2.11.3's `Refusal` tags 6 and 7, mapped 1:1. The join-path checks (RUNNER H1 and V2) are RUNNER-SPEC v2.11.3's, referenced here and not restated. SHA-256 is std's, built by std-sha256-1071; this spec names it and does not specify its API. Model: new `model/model_v224_client.py` (RUN, receipt `model-v224-client-run.txt`). L35's digest values are RUN by Python's `hashlib`, a separate implementation.

v2.23 (wire-spec-1059 v2's items S-a, S-b and S-c, filed after wire-attack-1060's F6–F8 and F10): text only. Two parts: client position rules in §1.8, "Client positions" (AG1, AG2), and the published value of a row with `published_value`, plus `span_changes` for a Cover's initial rows, in §1.5 (AG3). F10 agrees with RUNNER-SPEC v2.11 §3 "Rows" and L16 `rows_normalized`, read at 23:37. The store source cited is BY READING `core/programs/store/store.hirsel` at 6c90185b: `row_value` :1811-1825, `put_impl` :1878-1884, `Change` and `diff_emit` :3587-3622, and the extractor erasure :1753-1760. The codec format is BY READING `core/std/api/codec.md`. No byte of the format changes. Model: new `model/model_v223_client.py` (RUN, receipt `model-v223-client-run.txt`). L34 is hand-derived. WIRE-SPEC should cite "STORE-SPEC §1.8 Client positions" from now on; line numbers move with each version.

v2.22 (the orchestrator's answers to v2.21's two questions, 2026-11-16): text only. AF1: every commit path settles unsettled charges before it checks `resident_limit`, as `run` does; `admit` and schema commits no longer skip it (§1.11, §1.8, L27). AF2: store-build-1010 builds v2.19; the v2.19 header is corrected. No byte of the format changes, and no model changes: the account is not in any model, and L27 is hand-derived.

v2.21 (the orchestrator's S5 rulings, 2026-11-16, the "orchestrator -> store-spec-1001" entry in followups.md): text only. Streams AE1–AE3 (§1.8, §1.11) and scopes AE4 (a)–(e) (§1.4). No byte of the format changes. Models, both RUN: `model/model_v221_streams.py` (`model_v24_streams.py` plus AE1–AE3; receipt `model-v221-streams-run.txt`) and `model/model_v221_scope.py`, which is new because no scope model existed (receipt `model-v221-scope-run.txt`). Built in S5.

v2.20 (a call record carries its code revision; orchestrator ruling, from code-revision-1038 V4): text only. `code.revision` is BY READING `docs/spec/programs.md` PROG-9 and `core/core/code.hirsel:6` at origin/main, and R5 is BY READING RUNNER-SPEC v2.10 §7. **One format change: the COMMIT call section gains a revision field.** Format 1 is not released, so there is no compatibility path. No model changes. Built in the slice after S3; S3 lands as queued.

v2.19 (the store step bound parameter; orchestrator ruling 2026-11-16, DECISIONS.md "Store step bound parameter", and the FAIL-16 OutOfTime scope ruling): text only. The step rules cited are BY READING `docs/spec/budgets.md` MEM-30 at origin/main 0c18d366 and RUNNER-SPEC v2.9 rows 3 and 4. No byte of the format changes, and no model changes. store-build-1010 builds it; step-bounds-1051 did only the lowering it rests on (AF2).

v2.18 (`migrate` and indexes, from store-build-1010, to be fixed before S4 is built): text only, worked by hand. The index erasure it reuses is RUN `index_erase` (§1.2). No byte of the format changes, and no model changes.

v2.17 (a lost write in `adopt`, from store-build-1010, to be fixed before S4 is built): text only, worked by hand. The MANIFEST already has `parent` and `forked_at` (§2.6), so no byte of the format changes, and no model changes.

v2.16 (two items from store-build-1010, neither blocking S3; built after v2.15): text only. Z1 is worked by hand. Z2 is BY READING `delete-l4-916/STORE-RECORD.md:494`, `place-store-681/REPORT.md:400-405` and `core/twins/m4/durable_commit.rs` with its `MEASURE.md` (889, 6c3809d2). No byte of the format changes, and no model changes.

v2.15 (the journal ack channel, logged in X1 as not gating S3): text only. The proof is BY READING §3.3, §1.3 and `docs/spec/tasks.md` (CONC-4, CONC-5, CONC-12, CONC-13), and the counterexample is worked by hand. No byte of the format changes, and no model changes. Built after S3 lands; S3 does not wait for it.

v2.14 (one durability defect from store-build-1010's S3 crash sweep, `store-build-1010/evidence/s3-rotation-order/cut20.log`): text only. The proof that the new rule closes every ordering is BY READING §3.3, §3.4 step 5, §3.5, §1.10 and `docs/spec/tasks.md` (CONC-4, CONC-10); it is not model-checked. No byte of the format changes, and no model changes.

v2.13 (two S3 gaps from store-build-1010, direction set by the orchestrator): text only. The concurrency rules cited are BY READING `docs/spec/tasks.md` (CONC-4, CONC-5, CONC-10, CONC-12, CONC-13) and `core/core/core.hirsel:129` (`IoError`). No byte of the format changes, and no model changes.

v2.12 (from the independent S2 format check of store-build-1010 at 631305fc, `S2-CHECK.md`): the genesis MANIFEST vector re-derived from the spec text with libxxhash 0.8.2 (`s2check/derive_manifest_vector.py`, receipt `s2check/derive-run.txt`, RUN). No byte of the format changes, and no model changes.

v2.11 (one transaction limit, from 1010's L29 boundary line): BY READING arithmetic from §2.4. No layout change and no model change.

v2.10 (1010's S2 ticket/close seam): slice S2 redrawn. No byte of the format changes, and no model changes.

v2.9 (a refused checkpoint, no COMMIT encode without a journal, fair twins): refusal model `model/model_v29_refusal.py`, receipt `model-v29-refusal-run.txt` (RUN); R1 figures from store-r1-cost-1030 (`REPORT.md` §1, §5, §6, RUN by that lane, read here); twins checked BY READING `core/twins/m4/` at origin/main 93f4029b. No byte of the format changes.

v2.8 (three items from RUNNER-SPEC v2.2): N12 checked BY READING `store.hirsel` at 1f9148af; the L16 case is runner-spec-1020's probe p17 (RUN by that lane at 62f15988, read here). No byte of the format changes.

v2.7 (I2 re-decided on figures): checker probes `programs-v27/` (`probes-v27.txt`, RUN at 1f9148af); per-operation figures from store-r1-cost-1030's callgrind of R1 (`work/prof/cg-R1.out`, RUN by that lane, read here).

v2.6 (three §1 choices from the S1 review, store-s1-review-1027/ATTACK.md "Section 1 differences and unspecified choices", BY READING the review and store.hirsel at 1f9148af).

v2.5 (two S2 gaps from store-build-1010): pack model `model/model_v25_packs.py`, receipt `model-v25-packs-run.txt` (RUN). No byte of the format changes.

v2.4 base: origin/main **1f9148af**, 18 crates (RUN); checker rebuilt there (RUN, `build-v24.log`, BUILD SUCCEEDED). v2.4 probes: `programs-v24/` (`probes-v24.txt`). Streams model: `model/model_v24_streams.py`, receipt `model-v24-streams-run.txt` (RUN). ENDSTATE text: `STORE-ENDSTATE-TEXT.md`.

v2.3 base: origin/main **1e866455**, 18 crates (RUN); checker rebuilt there (RUN, `build-v23.log`, BUILD SUCCEEDED). v2.3 probes: `programs-v23/` (`look_view` ACCEPTED, `look_put` REFUSED OWN-5; `probes-v23.txt`). Streams model: `model/model_v23_streams.py`, receipt `model-v23-streams-run.txt` (RUN). The runner's calls model re-run at this base: `runner-calls-model-run-v23.txt` (RUN).

v2.2 (checksum ruling): model re-run `model/model_v22.py`, receipt `model-v22-run.txt` (RUN).
v2.1 base: origin/main **41493a1a**, 18 crates (RUN); checker rebuilt there (RUN, `build-v21.log`, BUILD SUCCEEDED). v2.1 probes: `programs-v21/` (`authority`, `shape_plain`, `shape_arc`; outcomes in CHANGES-v2.1; `shape_*` checked against std-codec-1008's core at 227d3382).
v2 base: origin/main **78e07cb1**, 18 crates (RUN). Checker: the worktree's `hirsel`, built at that commit (RUN, `build-v2.log`, EXIT 0).
Probe programs and their script: `store-spec-1001/programs/` (`check.sh <dir>`). The outcomes on 78e07cb1 are in `probes-v2.txt` (RUN). The v1 outcomes on 072c82b7 are in `probes.txt`.
Executable model: `model/model_v2.py`, receipt `model-v2-run.txt`, mutants `model-v2-mutants.txt` (RUN, §3.9).
Review answered: `store-spec-review-1006/ADVICE.md` (base 596b3309), with `law-audit.md`, `model.py` and `api-programs/`.
Labels: **RUN** means I ran it. **BY READING** means I read it in the cited source. **GUESS** means it is my estimate, and the first measurement replaces it.

---

v2.29 (RUNNER-SPEC v2.18, lane caller-scope-1128, astra's third attack, ATTACK-3). This version is text and format; no store has been deployed, so the cutover is a fresh store.

- **`serve`'s contracts are completed:**
  - a refusal keeps the run (AM1);
  - a migration is one commit (AM2);
  - the first delivery comes back in a receipt (AM3);
  - errors before and after handoff are told apart (AM4);
  - the reserves are derived (AM5);
  - a concluded fault keeps its CallInfo (AM6);
  - the launch, namespaces and sizes are completed (AM7).
- **The store gains the release design's three hooks (AM8):** authority tables with their counter, `authorize`, and the counter's reader.
- **Two RUNNER-SPEC slices consume this version.** AM3 and AM8 are small and are built first, as slice RA. RS then takes AM1, AM2 and AM4–AM7, and RR builds on AM8 (RUNNER-SPEC §9). Neither RS nor RR waits on the other.

v2.28 (RUNNER-SPEC v2.17, lane caller-scope-1128, astra's second attack, ATTACK-2): text and format; no store has been deployed, so the cutover is a fresh store. `serve` runs an admitted position: it runs the policy and the body as two isolated executions over one snapshot and writes the reply, and the record when there is one, in the body's transaction; a fault keeps the run's digest in a `Pending` that `conclude` writes with the fixed reply (AL1). A duplicate returns the stored reply with its scope digest (AL2). The scope digest begins with the StoreId (AL3). `migrate` is its own phase over a `Migration`, and `Schema` only opens (AL4). The CALL section carries the authority bytes (AL5). The sizes follow the 32-byte scope digest, and `serve` keeps no read log (AL6).

## CHANGES-v2.40 (store-c-review-1692: §11 and store.md, not yet built)

| # | Item | Verdict | What v2.40 does | Where |
|---|---|---|---|---|
| C-1 | AP1's kind table kept 0x0081 "unchanged", although format 3 deletes `checkpoint_task_bytes` and install must check NEW's pack, checkpoint and journal limits | **TARGET, not built** | 0x0081 gets its own row and its own layout: the codec encoding of `PreparedMeta`, with field offsets and widths, 212 + d bytes. The writer stores NEW's effective options and measures the encoded buffer against 1,048,832 (d ≤ 1,048,620), and the reader decodes exactly n bytes. Install checks only the four decoded values, bound by the import digest. Law L51 (e′) covers the floor and one byte below with identical OLD options. | §11.1 kind table, T10, AP1b, AP4; L51 (e′); store.md Install |
| C-2 | AP16 step 4 encoded the live holder records, so a `delete_branch` cut could name the deleted branch | **TARGET, not built** | Plan item 0 builds the prospective holder delta H, with its record storage and slot reserved. Step 4 encodes the live records with H applied, every root at its candidate location; `first_live` is step 1's segment and `next_pack` is one past the last pack written. Step 5 applies H without allocation, and a refusal drops it. Live counters (stamps, a cut-path fork's id) are kept apart from prospective ones (`next_table`, T, the last published generation). | §11.12 AP16 steps 2, 4, 5, H, refusal; L28 (e) |
| C-3 | AP15's "every frame producer" gave `serve` and `conclude` no result, and resident `NoRoom` would have counted journal pressure toward settle 2 | **TARGET, not built** | A producer table. `serve` returns `Before(Full(Journal(c)), None)`: the position stays Admitted and re-admission gives `Abandoned`. `conclude` returns `Before(Full(Journal(c)), Some(p))`, handing its Pending back so a retry after a cut keeps the original context. `NoRoom` stays resident or allocation only. **Ruling (a) deviation, small:** `ServeError.Before` gains an `Option[Pending]`, because `Before(Error)` cannot return the Pending that `conclude` took. | §11.11 AP15; §11.16; L50 (e); store.md Journal cap |
| C-4 | A segment was said to hold at most `checkpoint_bytes` + `FRAME_LIMIT`, which refused rotations, extension frames and torn tails all break | **TARGET, not built** | U is the accepted COMMIT/FORK bytes and the publication limit. A bounded streaming segment reader (65,536-byte chunk plus one `FRAME_LIMIT` frame buffer) keeps the checksum, END, `prev_end` and torn-tail rules for recovery and `recorded`. HEADER/END pairs are counted for open rotations and for cuts. No physical-disk bound is given; one would be a separate contract. | §11.11 AP15 segment reads, Disk |
| C-5 | The floor proof had no restart precondition, and `checkpoint_bytes` near u64::MAX overflowed the floor and `requested` | **TARGET, not built** | The proof applies to an uninterrupted interval from U = 0 after a published cut. `since` is 0 at open, and U keeps the recovered suffix (worked restart example). The comparison is U > J, then f > J − U, and `requested` saturates. The options step runs before LOCK, so it can be checked without a store, and covers out of range (> 18,446,744,073,692,774,319) and below the floor. The attempt schedule is fixed to 83,886,160 / 167,772,320 / 251,658,480, and L50 (e)'s maximal frame is two puts. | §11.11 AP15; AP21 open order step 0; L28 (d), L50 (e) |
| C-6 | AP16's plan computed list edits before the data layout, and the optional retry undid only the worklist | **TARGET, not built** | The plan order is H, fresh set, settle (the mandatory retry base), optional relocation, data layout, list edits, list stamps, class-2 layout, buffers; encoding is children first. A refused optional part restores the base, discards dependent state and re-emits from the installed `next_pack`. Leftovers are contiguous, and consumed stamps stay consumed. L41 (vii) adds a refusal after an earlier rename, checked against an independent parse. | §11.12 AP16 step 2, relocation, list edits, refusal; L41 (vii) |
| C-7 | §3.4's retained order cleaned up before AP21 validated, and the prepare opener and NEW's output were unvalidated | **TARGET, not built** | The open order is restated: options, LOCK, MANIFEST, O1, AP21 steps 0–4, cleanup from the validated result, rebuild, journal, account, rotate, streams, the journal task alone. The inventory excludes members of R that were already unlinked. List reads are bounded (`len` ≤ 8,384 and offset arithmetic) before reading. The prepare opener validates with no cleanup, and NEW's private output and prospective MANIFEST are validated before the rename. Validation, capacity and I/O failures stay distinct. | §11.12 AP21; §11.1 T10; AP16 step 4 |
| C-8 | AP1a let a second op of the CALL's stream disagree with the reply, and W2's fallback could pick another refused cut | **TARGET, not built** | Section tags must be ascending and unique, and the call-owning key has exactly one streams op. The built completion/record stream check is kept (`Before(Foreign, None)`). W2 step 2 uses the last *published* generation. L29 (d) adds 337-byte settle 1 and 2 cases comparing every `Recorded` field, duplicate cases, and the two-refused-cuts case. | §11.1 AP1a; L29 (d) |
| C-9 | L28 (a), L28 (c) and L51 (l) asked for observations their oracles cannot make | **TARGET, not built** | L28 (a) drains before the copy and adds a late insert refusal and crash images across step 4. L28 (c) parses the saved prefix and checks heads through the prepare opener. L51 (l) compares incoming edges as multisets by stable holder identity, through an injective serial-to-name projection, with lease history. | §11.15 L28, L51 |
| C-10 | The cap check sat "before taking an id", before a transaction's final f was known | **TARGET, not built** | The check runs once the final f is known: after the body returns, before the settle, publication, the frame-room wait and a fork's id. A refused COMMIT may consume private stamps, and step charging is unchanged. | §11.11 AP15; AP17; L50 (e) |
| C-11 | The index list's bound was stated on its length while its charge is its granted capacity | **TARGET, not built** | 8n occupied bytes < 14,913,152. The capacity is 8 × G, with G the granted slots and no fixed bound claimed. `try_reserve` is kept: no exact reservation or allocator grant proof is needed, because no hard scratch capacity is promised and the charge is the actual grant. L50 observes 10,000 occupied slots, 8 × G and no key copy. | §11.11 AP15 frame being built, summary; L50 |
| C-12 (LATER) | AP16 and AP18 understated pause and fork cost | **TARGET, not built** | t_drain spans the queued groups, and t_remove and the END sync are named. A fork's direct cost is stated apart from its end-to-end cost when it crosses the threshold: a refused cut is not the fork's failure, and cut I/O poisons it after the FORK is durable. L44's fixture is unchanged. | §11.12 pause; §11.13 AP17 threshold cuts, AP18 |
| C-13 (LATER) | The §11 rewrite list omitted §3.1, §3.3, §3.4 and §3.5a | **TARGET, not built** | They are named, with §1.3, §1.8 and §4's table, and the removed-task text and withdrawn changelog entries are marked as history. | §11 status |
| (a) | Ruling (a): each operation keeps its own result shape | **TARGET, not built** | `run`/`run_on` → `Refused(cause)`; committing `admit` and live-head `fork` → `Error(Full(cause))`; `serve`/`conclude` → `Before(Full(cause), …)`/`After`. The one change is C-3's `Option[Pending]`. | §11.11 AP15 producer table; §11.16 |
| (b) | Ruling (b): one store-local cause | **TARGET, not built** | `StoreCapacity = Journal(Capacity) \| Resident(Capacity) \| Allocation(Capacity)`, built at the refusal site. It is used in `Error.Full`, `Outcome.Refused`, `Looked.Refused`, `Served.NoRoom` and `DeployError.FullNew`. Core `Capacity` and `Stop.OutOfBytes(Capacity)` are unchanged, and no frame, allocator or replay identity changes. Every §11 refusal and law value is retagged. | §11.16; §11.1, AP15, AP16, AP17, L27–L51; store.md Errors |

## CHANGES-v2.39 (schemasmash-durable-1683: §11, not yet built)

| # | Item | Verdict | What v2.39 does | Where |
|---|---|---|---|---|
| V1 | v2.38 deleted AP21's validation steps while 18 lines still cite it (finding 1) | **TARGET, not built** | AP21 is restored, steps 0–5: list resolution and the disjointness of listed, retired and reached class-2 seqs below `next_pack`; reached list offsets confirmed as PAGE heads by a streamed file scan; bounded pack reads; one location per reached stamp across both graphs; dead frames never followed. Its output, the validated inventory and live graph, is the only input of open's trees and location table, cleanup's retirement and an install's pre-activation check. Built by the opening task before the writer serves anything; no reserve, plan or hand-off state | AP21, §11.1 install |
| V2 | a COMMIT's CALL section named its stream independently, so it could name an Admitted put, a delete or nothing that carries the reply (finding 2) | **TARGET, not built: format 3's COMMIT payload changes** | section tag 2 is retired; streams op tag 3 is a put of a Replied value followed by its call (no stream field: the op's key is the stream); at most one per COMMIT; `recorded` sets `CallRecord.stream` from the key. A call commit is 5 + \|stream\| bytes shorter | AP1, AP1a, L29 |

## CHANGES-v2.38 (gavel ruling 1, option C: §11, not yet built)

| # | Item | Verdict | What v2.38 does | Where |
|---|---|---|---|---|
| C1 | Sam's gavel ruling 1 (2026-11-22): option C | **TARGET, not built** | the writer runs every cut inline while paused, writing only what no earlier cut wrote; a refused cut loses nothing; writes are refused at once at the journal cap (`Refused(c)` from `run`/`run_on`, `Full(c)` from `admit` and live-head `fork`). Prior art: bbolt v1.4.0 `Tx.Commit` (adopted, adapted), RocksDB `WriteOptions::no_slowdown` (adopted), RocksDB write stalls (not taken) | choice 7, AP15, AP16 |
| C2 | the checkpoint task, its channel, `checkpoint_task_bytes`, the workspace W(`pack_bytes`), the `Cut`, `Checkpointed` and `Refused` messages | **deleted** | the paused writer encodes through one pack buffer of max(`pack_bytes`, 16,777,476) bytes from its own limit, fallibly | AP4, AP15, AP16, §11.16 |
| C3 | nothing bounded the journal while cuts are refused | **TARGET, not built** | `journal_limit` J, default 268,435,456: a frame that would take U past J is refused at once with `Capacity{requested: U + f, available: J}`, the same way for every frame producer; open requires J ≥ `checkpoint_bytes` + `FRAME_LIMIT` | AP15, AP17, L28, L50 |
| C4 | the checkpoint reserve and its credit | **deleted** | records exist only for frames a cut wrote, are made by the cut and hold no body share; no commit, migrate, fork, pin, open or install pays checkpoint state | AP16 |
| U1, U2, U5 | credit owed at the ack; the pack-share cap; reserved slots | **dissolved** | no credit, no reserve, no share cap | — |
| U3 | the owned plan could not encode the captured state | **answered** | no captured copy: the paused writer encodes the MANIFEST from its own records; a BlobRef's checksum is over the value bytes | AP16 |
| U4 | ordinary failure exits after staging | **answered** | every allocation precedes the MANIFEST rename and may refuse the cut, losing nothing; after it only I/O fails, which poisons; a stamp draw past 2⁶⁴ − 1 is `Limit(Ids)` | AP6, AP16 |
| U6 | credit and cleanup lacked a lifetime rule | **answered** | no credit; the carried set of lease-skipped retirements stays until a removal unlinks it | AP16 |
| U7 | in-memory identities narrower than the format | **answered** | tagged, full-width incoming sources | AP16 |
| U8 | list construction followed the only link pass | **answered** | the list edits run on the final layout with their own records and `List` entries; L51 (l) compares the table with a reopen | AP16, L51 |
| U9 | the closure assumed a key-range operation on core `Map` | **answered** | the record map is a persistent `std.omap.OMap` keyed by stamp; one emission stack per tree | AP16 |
| U10 | the sorting vector assumed coalesced, exactly sized storage | **answered** | one op per distinct key in the Tx's `List[Op]`; the index list charges 8 × its granted capacity | AP15 |
| U11 | the public guarantees and the channel total disagreed | **answered** | the checkpoint channel is deleted; open's own allocations may be `Full(c)` | AP15, §11.16 |
| U12 | two schedules could not produce their observations | **answered** | L28 (c) states each of three cuts' removals | L28 |
| U13 | optional relocation could take the mandatory part's memory | **answered** | relocation is allocated after the mandatory part, and dropped with one redo on any later refusal | AP16, L41 |
| v2.37 T1, T5, T8, T9/N8, T6's W; v2.36 N8/S4/S5 | the reserve, staging, the plan hand-off, bare rotation and the checkpoint task's workspace | **withdrawn** | superseded by C1–C4. T2, T3, T4, T6's emission order and T12 stand; T7 is amended by U10; T10 drops its allowance check; T11's cases are restated in L28 | — |

## CHANGES-v2.37 (round 5 of the M1 review: §11, not yet built)

> v2.38 withdraws T1, T5, T8, T9/N8 and T6's workspace W; T7 is amended by U10 and T10 by C2; T11's cases are restated; T2, T3, T4, T6's emission order and T12 stand (CHANGES-v2.38).

| # | Item | Verdict | What v2.37 does | Where |
|---|---|---|---|---|
| T1 | a refused plan could leave the installed graph holding a rejected operation's staged links | **TARGET, not built** | staging is a delta tagged by generation; the installed view changes only at the ack; after staging the only exits are the ack and poison | AP16, L41 |
| T2 | the removal interval could unlink a segment an export lease holds | **TARGET, not built** | the covered interval runs from the lowest segment left on disk, minus the lease's segments, as at most two ranges | §3.5a, AP16, L28 |
| T3 | moved list pages took fresh stamps; a discarded retirement page could be emitted | **TARGET, not built** | one ordered pass: edit, discard, relocate (moved pages keep stamps), classify from final locations, rebuild, emit only reachable pages; the bound counts L | AP6, AP16, L51 |
| T4 | a record's body share made every private page shared, so edits copied | **TARGET, not built** | records hold no share; the plan takes shares when it freezes; a commit that then finds a page shared copies | AP16, L39 |
| T5 | no payer or release for writer checkpoint state | **TARGET, not built** | a payer/release table; maps grow by `try_reserve` at the paying operation; resident charges per slot | AP15, AP16 |
| T6 | the workspace proof needed pack-contiguous emission | **TARGET, not built** | blobs, then data pages, then list pages, each children first; W(`pack_bytes`) = max(`pack_bytes`, 16,777,476) + 65,536 | AP16, L28 |
| T7 | commit ops sorted with uncounted scratch | **TARGET, not built** | ops sorted through an exactly reserved index list; units labelled | AP15 |
| T8 | the plan was shared with the task and its checksum order unstated | **TARGET, not built** | an owned plan moved to the task and back; result cells filled children first; writer time bound | AP16 |
| T9, N8 | plans could be refused, leaving segments to grow | **TARGET, not built** | the checkpoint reserve with its size proof; relocation deferrable; plans taken at §3.5 step 0a; bare rotation bounds each segment | choice 7, AP15, AP16, L51 |
| T10 | install activated NEW without checking its checkpoint allowance and layout | **TARGET, not built** | pre-activation checks; failure is `FullNew(c)` with OLD kept | §11.1 install |
| T11 | L28 did not exercise the stated bounds | **TARGET, not built** | counted-allocator zero for k = 1 and 1,000; a lease hole through two cuts; minimum W at `pack_bytes` = 1 | L28 |
| T12 | class-2 pointers were not confirmed as PAGE heads | **TARGET, not built** | reached list pages share the stamp table; streamed class-2 files confirm every reached offset | AP21, L47 |
| S1–S4, N2, R1 | partly resolved in v2.36 | **TARGET, not built** | finished by the reserve (S4, N8), the stamp rule (S1), the list pass (S2), the units (N2, R1, S3) | as above |

## CHANGES-v2.36 (round 4 of the M1 review: §11, not yet built)

> v2.38 withdraws the N8, S4, S5 row: the checkpoint task is deleted (CHANGES-v2.38). The other rows stand.

| # | Item | Verdict | What v2.36 does | Where |
|---|---|---|---|---|
| N8, S4, S5 | a checkpoint task with a fixed allowance could refuse every retry: its records, list pages and removal names grew with frames, packs and segments | **TARGET, not built** | the writer plans each cut over records allocated with the frames; the task runs the plan in a fixed workspace that open checks; removals are an interval and a seq list; planning's list pages, pack rows and retired seqs stay unreserved (named residual) | choice 7, AP4, AP15, AP16, L28 |
| S1, S2 | list pages had no stamps the writer could not reuse; retiring a class-2 pack could empty another | **TARGET, not built** | list pages are stamped on the writer before the watermark is captured; `retired` is rebuilt whole each cut, so retirement reaches its fixed point in one pass | AP6, AP14, AP16, L51 (j) |
| S6, S7 | open could accept a reached class-2 pack that is also retired; class-2 `bytes` had no source at open | **TARGET, not built** | listed, retired and reached class-2 seqs pairwise disjoint and below `next_pack` before any removal; each reached class-2 file streamed once | AP16, AP21, L41, L47 |
| N2, R1, S3 | encoding held a payload beside its output; worker scratch and segment reads were uncounted | **TARGET, not built** | one output buffer per frame, no cloned keys; the worker's fixed scratch named; segment reads bounded by their own rules | AP15, L50 |
| S8, S9, S10 | L5 omitted list pages; list rows and the resident report ignored the lists; replay skipped the record-byte ceiling | **TARGET, not built** | L5 adds the list paths; list rows inline always and list roots in the report; a running record total in replay and the writer | AP8, AP15, AP19, L5, L51 (k) |

## CHANGES-v2.35 (round 3 of the M1 review: §11, not yet built)

| # | Item | Verdict | What v2.35 does | Where |
|---|---|---|---|---|
| N8, H14, R5 | a fixed MANIFEST share for the pack lists could leave every covering cut too large, and AP4 stated two budgets | **TARGET, not built** | the lists are AP3 page trees in class-2 packs; the MANIFEST is 152 + records; `manifest_bytes` checked by fork, pin and cut; no cut refused for size | choice 6, AP4, AP14, AP16, AP20, AP21, L37, L47, L51 (a), (g) |
| R1, N2 | the memory claim left out the frame being built and the worker's group buffer | **TARGET, not built** | no process-wide bound; each journaling buffer has its own; F charged at handoff, released by cumulative acked bytes | choice 5, AP15, L50 |
| R2, H5, R3 | migrate left out its catalog rows; install refused valid drop records | **TARGET, not built** | catalog rows in W, publication at the proposed roots' exact account; install's remove, replace and keep | §11.1, AP13, L49, L51 (e) |
| R4, R6, R7, R8, N3 | cut-path fork prepared from the parent head; links by location; exact flag; empty separator passed up; reopen comparison | **TARGET, not built** | live-head-only preparation; stamp-keyed records and links; `exact` rule; separators after grouping; two continuations from one saved prefix | AP13, AP15, AP16, AP17, L41 (vi), L49, L50, L51 (d), (i) |
| H8, H9, H13 | API threshold wording; export images; `published_value` bytes | **TARGET, not built** | the loaded threshold in the API; four export images with their own expectations; `published_value` gives O or N with no prefix | L40, L43; `core/std/api/store.md` |

## CHANGES-v2.34 (round 2 of the M1 review: §11, not yet built)

| # | Item | Verdict | What v2.34 does | Where |
|---|---|---|---|---|
| N1 | a delayed cut ack installed its exact figure over charges made after its capture | **TARGET, not built** | the writer revision advances on every publish; an ack whose capture is stale subtracts only its own difference, and one after a settle leaves A alone | AP15, L50 (c) |
| N2, N3, B5 | frame bytes in the account broke delete-only and made the same roots answer differently | **TARGET, not built** | one end state: `resident_limit` bounds the logical account; frames are bounded by `FRAME_LIMIT` = 16,777,296 with backpressure; a live-head fork is never refused by admission | §11.0 choice 5, AP15, AP17, L44, L50, L51 (d) |
| N7, H12 | a schema-only install had no rule to keep existing rows | **TARGET, not built** | install edits are replace, remove, create-empty and keep, derived from the validated inventory and catalog identities | §11.1, L51 (f) |
| N4, N5, N8, H1, H9, H10, H14 | fork oracle, preparation before send, MANIFEST list share, export of FORK, id bounds | **TARGET, not built** | observed-durable-prefix oracle; one preparation block before send; branch/pin records share 16 MiB and lists 48 MiB; replay checks `next_branch` ≤ id < 2³² − 1 | AP4, AP17, AP19, L43, L51 |
| N6, N9–N13, H5, H6, H8, H13, M2–M4, M7, B4, H2 | split occupancy, fixture numbers, one-root settle, sidecar bodies, law ownership, staging, workspace charge, borrow rotation, threshold persistence, schema reads, record bytes, decode order, scoped `ViewRows`, slice order | **TARGET, not built** | as listed in `m1-review-1625/RESOLUTION.md` round 2 | AP3, AP8, AP12–AP16, L5, L27, L40, L41, L45, L47, L49; M1-DESIGN |

## CHANGES-v2.33 (the M1 review: §11, not yet built)

| # | Item | Verdict | What v2.33 does | Where |
|---|---|---|---|---|
| B4, B5 | the account depended on page partition and history, so replay and a conservative charge could disagree with publication | **TARGET, not built** | the logical account counts distinct row versions over participating roots; the conservative charge is proven an upper bound; open's exact figure never exceeds the admitted one | §11.0, AP15, L50, L27 |
| B1, H2, H10, H14 | a reissued branch id after a crash; fork's decision order, names, id exhaustion and the MANIFEST budget | **TARGET, not built** | a live-head fork returns only after its FORK is synced; one ordered list of checks; `Limit(Ids)`, `Limit(Manifest)`; replay accepts id ≥ `next_branch` | AP4, AP17–AP19, L43, L51 |
| B2, B3, H7, M6 | relocation retired packs still referenced, and duplicate stamps and dead pointers broke open | **TARGET, not built** | physical records with live parent links; the closure is all live ancestors of moved frames; open resolves live edges by location; the watermark covers listed packs | AP6, AP16, AP21, L41 |
| B6, M3 | map leaves had no owning edge; structural validation gaps | **TARGET, not built** | map-leaf kids are collection roots, with the order rule; bounded decode; retired kinds refused | AP2, AP3, AP20, AP21, L47 |
| H1, H3–H6, H8, H9, H11–H13, M1, M2, M4, M5, M7, M8 | replay oracle, unsealed images, plan-then-apply, migrate sort, branch occupancy, decode ceilings, FORK in export, drop, install, schema reads, law observability, resident report, `ViewRows`, BLOB vector, slice order, pack bytes | **TARGET, not built** | as `m1-review-1625/RESOLUTION.md` lists, row by row | §11 |

## CHANGES-v2.32 (the M1 target: §11, not yet built)

| # | Item | Verdict | What v2.32 does | Where |
|---|---|---|---|---|
| AP1–AP4, AP20 | M1's format: word 3, the PAGE, BLOB, FORK and MANIFEST 0x0031 kinds, NODE and MANIFEST 0x0030 retired, the page layout and `PagePtr`, and limits with the value limit moved out of the format | **TARGET, not built** | specified byte by byte, with vectors | §11.1–§11.2 |
| AP5–AP8 | logical keys and stored suffixes, stamps, per-collection roots (`CollectionRoot`), and inline values and BlobRef | **TARGET, not built** | | §11.3–§11.6 |
| AP9–AP13 | copy-on-first-write per transaction; in-place edits across commits only for writer-side applies with a unique path; `View[Bytes]` reads; bottom-up bulk build | **TARGET, not built** | | §11.7–§11.9 |
| AP14–AP16 | the format-3 MANIFEST with the reserved `base` root; logical and resident accounts; the relocation closure and its cascade bound | **TARGET, not built** | | §11.10–§11.12 |
| AP17–AP19 | the journaled live-head `fork` and its recovery in every journal reader | **TARGET, not built** | | §11.13 |
| L37–L48 | the laws M1 adds, each in a named existing harness | **TARGET, not built** | | §11.14 |

## CHANGES-v2.31 (the store is the one durability contract)

| # | Item | Verdict | What v2.31 does | Where |
|---|---|---|---|---|
| AO1 | A same-open admit whose admit map was full committed `Admitted(p)` again; §1.8 (D6, AH3, AE3) says the same-open `Abandoned` commits nothing | **CODE FIXED; §1.8 unchanged** | `admit` reserves its open entry before it commits. If there is no room, the admit is `Full` and commits nothing, which is §1.8's `Full` row. After the commit the reserved entry is filled in place without allocating. A reserved entry left by a refused commit answers nothing, so the next admit of p re-commits. The old path could not be reached under charge accounting: a commit's transient memory exceeds the map write, so a commit that fit always left room for the write. Case: `journal_law_streams` refuses an earlier open's re-commit once with `Full`, then admits p under this open; the mutant `reserved_answers` (a reserved entry answers `Abandoned`) is CAUGHT. | §1.8, L13 |
| AO2 | §1.8's `Admission` comment cited REPLAY-SPEC §7.3 rule S3, which REPLAY-SPEC v3 no longer has | **TEXT** | It now cites the store's own rule: D6, re-admission under a new open. | §1.8 |
| AO3 | The trigger build needs two store-level names that STORE-SPEC did not have | **ADDED, text only** | §1.13 names the refusal `StaleAttempt` (the attempt fence) and the run state "held" (the hold mark), from ENDSTATE storage::triggers and storage::steps. The store adds no API, kind or tree for them. | §1.13 |

## CHANGES-v2.30 (ruling C: the store does no authorization; RUNNER-SPEC v2.19)

This version supersedes every scope, policy and authority item of v2.26–v2.29: AJ1, AJ3, AE4, AK1, AK3's scope slot, AL1's policy execution, AL3, AL5's `authority`, AM3's `digest`, `scope` and `seq`, AM8 and the L14 scope law. Amended ruling 73 (RUNNER-SPEC §3) replaces what the scope digests guarded: queued subscription data is re-checked against current state before it is sent, and a call's saved reply is delivered as it was committed.

| # | Item | Verdict | What v2.30 does | Where |
|---|---|---|---|---|
| AN1 | The store authorized each call through a policy, a narrowed Tx and a scope digest beside every reply (ruling C withdraws store-level authorization) | **DELETED** | Deleted: `KeyScope`, `Access`, `scope`, `scope_keys`, `union`, `narrow`, `narrow_view`, `everything`, `none`, `scope_within`, `span`, the `OutOfScope` error; `declare_authority`, the authority tree, `authorize`, `Authorized`, `Digest`, `catalog_table`, `snapshot_seq`, the `seq` counter, the `Latest[Seq]` register, `AuthorityReader`, `authority_reader`, `dup` and `load`; `Stop::Policy`; `Plan.n_p` and `Plan.s_p`; the policy argument and `caller` of `serve`; `Receipt.digest`, `scope` and `seq`; the scope digest in `Pending`, `Stored` and the streams value; `CallRecord.authority`. A Tx may read and write every key of its store. `serve` runs one isolated execution, the body's, under `(n_e, s_e)`. `Plan` is `{ n_e, s_e, reply_max }`; `Receipt` is `{ ticket, reply }`. `CallRecord.code` holds the code that ran: u64 n ‖ u64 s, big-endian, then the replay identity (32 bytes, or none when the launch has none). | §1.3, §1.4, §1.8 |
| AN2 | A stored reply carried a 32-byte scope digest | **CHANGED: 41 + R, format word 2** | The streams value is (position 8, request digest 32, state 1) and, when replied, the reply R: 41 B admitted, 41 + R replied. `Stored` is `{ reply }`, and a duplicate returns its saved reply unchanged. The HEADER's format word is 2. A reader accepts only format 2; any other value is `Unsupported(format)`. The CALL section's last blob is `code` (16 or 48 bytes). | §1.8, §1.11, §2.3, §2.4 |

## CHANGES-v2.29 (caller-scope third attack; RUNNER-SPEC v2.18)

This version supersedes v2.28 AL1's `NoRoom`, `Committed(Ticket)`/`Kept(Ticket)` and `policy` closure. It also supersedes AL4's commit unit and AL6's `ADAPTER_*` text, and v2.28's "one bounded execution" summary.

The checker accepted `serve`'s two isolated executions (probe `sp3`, with controls refused by MEM-30 and FAIL-16). It accepted a static policy's `StaticFn` shape (probe `auth`, with control `authc` refused by PROG-9); that probe checked the shape only. **ATTACK-4 corrections, in place:** `Auth` is deleted for a narrowed `Tx` with `catalog_table` naming grant targets, `KeyScope` is `send`, the register is a `Latest[Seq]` published before the followed snapshot, `AuthorityReader` is `send` with `dup`, `Receipt` and `Pending` keep the run's `seq`, `NoRoom` carries its `Capacity`, `CallInfo` costs 44 B, `recorded` returns its settle context with `settle` 2, the replay identity is derived in `place_head`, and `scope_within` is public (AM1, AM3, AM5–AM8). Checked by probe `/tmp/cs1128-probe/p5s` (a copy of the worktree's `store.hirsel` with these declarations added), accepted: a `StaticFn[fn(Tx, Caller) -> ..]` policy that fetches from one table through `catalog_table`, grants `scope_keys` over another table named the same way, tests it with `scope_within`, and sends the scope through a channel inside a `send` record; and a `Latest[Seq]` reader made, `dup`ed and `load`ed. Controls: `p5c` (`KeyScope` not `send`) is refused with CONC-2, and `p5sx` and `p5sy` (a type error put in the policy body and in the channel function) are refused, so both bodies are checked. A first try named the function `within` and was refused with LANG-7, since `within` is already a parameter name in the store; so the public name is `scope_within`. The catalog lookup is a stub and nothing was run, so the probe checks signatures and ownership only.

| # | Item | Verdict | What v2.29 does | Where |
|---|---|---|---|---|
| AM1 | A refusal dropped the run, so keyed recovery lost its authority and reason. A fault lost its CallInfo (RUNNER-SPEC v2.18 B4, M6) | **CHANGED** | `Served = Committed(Receipt) \| Kept(Receipt) \| Failed(Pending, Stop) \| NoRoom(Pending, Capacity)`. `Pending` (opaque, not send) holds the Completion, the run's digest, scope and authorization `seq`, the record, the run's `CallInfo` and its base version, and on `NoRoom` the outcome the commit would have stored. `NoRoom` is a refusal **before the publish point**: nothing is stored, and its `Capacity{requested, available}` is that admission's (ATTACK-4 B7). A stream position drops the `Pending`, so it stays `Admitted`. A keyed claim's third resident refusal (http.md K-8) passes it to `conclude(mut db, take p, take r, Reserve.Claim)`, with `r` = the encoded `Failed(OutOfBytes(Room{requested: c.requested, available: c.available}))` from the returned `Capacity`; it writes that control-only reply with the digest, record, CallInfo, base, `settle` 2 and the claim's expiry and index mutations, from the claim's reserve, once. The body never runs a fourth time. `untried(take c, take rec, info: CallInfo)` gives `none`'s digest for a gone key. | §1.8 |
| AM2 | A migration's operations each checkpoint, so a failed migration left a partial catalog (B5) | **CHANGED: one catalog publication** | Inside `migrate`, `create_table`, `create_index`, `drop_table` and row `migrate` write their pages, but none publishes a manifest. When the phase returns `Ok`, one checkpoint publishes the new manifest with every change and advances the catalog version once. An `Error`, a fault or a crash before that checkpoint leaves the old manifest. Pages that no manifest names are discarded at open, as an interrupted checkpoint's are, so rule 12's promise of the old catalog holds. The open contract is unchanged: an open sees the old manifest or the new one (the §3.5 cut table's `migrate` row, now for the whole phase). | §1.2 :536; §3.5 |
| AM3 | A fresh success returned only a ticket, so the head could not build the first `Told` (M5) | **CHANGED: a receipt** | `pub type Receipt { ticket: Ticket, reply: Bytes, digest: Digest, scope: KeyScope, seq: u64 }` (not codec, not send). `serve` returns the reply bytes it stored, by move (it stores a copy), together with the digest, scope and authorization `seq` of the run (ATTACK-4 B2: the runner stamps the first `Told` with that `seq`, never with a later register load). `conclude` returns one for the fixed fault. The head builds the first `Told` from it, with no read-back, so first delivery never sets HTTP's `Idempotent-Replayed`. | §1.8 |
| AM4 | A post-publication error could follow a durable reply, yet promised `Admitted` (B6) | **CHANGED: before and after handoff** | `serve` and `conclude` return `Result[Served, ServeError]`, with `ServeError = Before(Error) \| After(Error)`. `Before` is any refusal or failure before the publish point (`run` step 4, §3), and leaves the position exactly as it was. `After` is any error from the publish point on (frame handoff, a cut, rotation). The runner treats it as poison: it answers nothing and exits (RUNNER-SPEC §2.1 Poison), and reopen recovers whatever prefix is durable, which may hold the reply (then a re-send gets `Duplicate`). Cancellation: during settle it drops the draft (I4); at any later wait it is a crash cut. An HTTP disconnect does not cancel the head. No `Pending` survives a restart. | §1.8; §3 |
| AM5 | `ADAPTER_STEPS` and `ADAPTER_BYTES` had no derivation; variable work lay outside both bounds (B7) | **CHANGED: derived** | **Byte grants.** Let P = `n_p` and E = `n_e`. The policy's execution reserves P, and its result L ≤ P stays charged. The body's execution holds a clone C ≤ L, plus the body's whole draft (dirty roots, op lists, issued-ID log, the return value and the stored reply copy), all under E. **The body's byte bound** covers the reply write and creation and growth of both logs. **The body's step bound** also covers frame encoding, whose bytes come from the head's admitted `frame_max`. **Outside both bounds, as built (ATTACK-4 B8):** the issued-ID finalization loop, whose steps are in A_s below. **I4's settle is the head's (VERIFY-5 B8).** `settle_admit` runs after `serve`'s step bound has returned its unused steps, under no call bound. That covers `current_state`, `cut_state` with its `state_account` walk over the pinned graph, the frame's re-encode and its checksum's 64-byte stripes. Its bytes are the Db's (a refusal is `Refused(c)`, as in I4), and its time is the head's, as frame encoding is under `frame_max`. So a cost that grows with pinned state never decides a call's outcome. **Peak:** L + C + E ≤ 2P + E; the handed-over L is not counted again. **Bytes reserved** = 2·P + E + A_b, where A_b is the sum of the MEM-24 allocations actually charged outside P and E. The body isolate contributes its 32-byte cause cell. The policy isolate's cause cell stays under P. Inline isolate results and the inline fields of `Pending` and `Receipt` contribute zero, including the 32-byte `Digest`, the 44-byte `CallInfo` and the moved reply's list handle. The moved reply's storage remains charged to E. L33 checks exact admission at the sum and refusal one byte below it. **Frame encoding and the commit** use the head's own admitted `frame_max`, not this reserve. **Steps reserved** = `s_p + s_e + A_s`, with A_s = K_fixed + K_id·⌊n_e / ID_CELL⌋ (ATTACK-4 B8; VERIFY-5 B8 deletes the re-encode term): K_fixed counts `serve`'s straight-line calls outside both bounds, and K_id the steps per issued id of the finalization loop (at most one id per ID_CELL bytes of E). The build computes the constants from the code; L33 checks exact step use on both exits. **Registration** computes both with checked multiplication and addition, and an overflow is `Bounds`. | §1.8; budgets.md |
| AM6 | A concluded fault's record replayed under the conclude frame's CallInfo (M6) | **CHANGED** | Tag 2 gains `settle u8` after `authority`. 0 means the record ran in this frame (its CallInfo is the frame's). 1 means it was concluded after a body fault, and 2 that it was concluded after the run's commit was refused for room (ATTACK-4 M3); both are followed by `base u64-BE ‖ at u64-BE ‖ seed u64-BE`, the attempted run's. `recorded` returns the record with this settle context: `pub type Recorded { record: CallRecord, settle: u8, branch: Branch, base: u64, at: u64, seed: u64 }`. Replay forks at `base` (v-1 for settle 0), derives the attempted id from `base` and `branch`, and uses `at` and `seed`; settle 2 is replayed as `Unknown(AdmissionRefused)`. The one-row call example grows by 1 byte to **374 + L B**, or **406 B** for L = 32; a concluded record is 24 bytes longer. | §2.4 |
| AM7 | Launch identity, namespaces and an obsolete size (M7, M8, m1) | **CHANGED** | **Replay identity (ATTACK-4 M4).** The launch digest stays the launcher's hash of the image bytes (launch.md), fixed before main. `place_head` derives the replay identity when it is given the selected constructors: H(launch digest ‖ u32 len ‖ `place`'s identity ‖ u32 len ‖ `migrate`'s ‖ u32 len ‖ `handles`'s), and `CallRecord.authority` carries it in place of AK4's launch digest (so L = 32 when it is known, 0 when not). A policy without a revision (PROG-9) records 32 zero bytes as its revision; an unavailable identity is empty (L = 0). Recorded-code mode answers `Unknown(RevisionUnknown)` for either. **Namespaces.** The `namespaces` entry is compared on kind and sender names at bind as well as at open, and a retired op's binding is compared with its own op record **at open**, namespace included (http.md K-1, K-9; ATTACK-4 M6: `bind` runs before the store is open). **§2.4.** "A reply adds R" becomes "a reply adds 32 + R" (the scope digest and the bytes). | §2.4 :1008; http.md K-1 |
| AM8 | The release design's hooks (RUNNER-SPEC v2.18 B1–B3; corrected in place by ATTACK-4 B1–B3, M1, M5) | **NEW** | **Authority tables.** `declare_authority(mut db, names: List[Str]) -> Result[(), Error]`, called once at open. A name that is not a table in the catalog is `Missing(name)`. An authority commit is a commit on main that replaces main's `authority` or `catalog` root, and every `adopt` (VERIFY-5 B3). A cut or rotation shares its roots and is not one. **The authority tree (VERIFY-5 B3).** Every snapshot of main carries `authority`, an in-memory tree holding a copy of every row and index entry of the declared tables, keyed as in `data`. `declare_authority` builds it at open from `data`. A commit that writes a declared table also applies those writes to it, admitted with the commit and charged to the Db. The policy's reads go to it, while bodies use `data`. It is never written to disk, so the format, cuts and replay are unchanged. So only an authority commit rebuilds what a policy can read, and two evaluations at one `seq` are equal. A commit on another branch never is: the register protects main. **The counter.** Every snapshot of main carries `seq: u64`, in memory only: 0 at open, plus 1 for each authority commit. **The register (B1).** A `Latest[Seq]` root (CONC-13; `Seq { v: u64 }`, `shareable + send`), made at open with `Seq{v: 0}` and charged to the Db. An authority commit admits its next `Arc[Seq]` before its last refusal point (`run` step 3; `adopt` before `cut_install`), so a failed admission is `Refused(c)`, a `Before`. `publish(db, main)` stores it with `latest.publish` **before** the followed snapshot's `latest.publish`, the commit's first publication another task can observe; a `memory` store does the same. **The reader.** `pub opaque type AuthorityReader: send` wraps one `Latest[Seq]` handle; `authority_reader(db)` makes one, `dup(r)` another (`latest.handle`, never fails), and `load(r) -> u64` is one `latest.read`. **Authorize.** `pub type Digest { a: u64, b: u64, c: u64, d: u64 }` (plain, `shareable + send`). `authorize(v: Tx, caller: Caller, policy: StaticFn[fn(Tx, Caller) -> Result[KeyScope, Capacity]], n: u64, s: u64) -> Result[Authorized, Stop]` runs the policy in an isolated execution under (n, s) on `v` narrowed read-only to the declared tables (B2: v2.29's opaque `Auth` is deleted). `Tx` is `pub opaque scoped` (PROG-2; VERIFY-5 B2), so a policy cannot read its fields. **One metered path (B3):** on every path the policy's reads go to a fresh, empty read log made inside its bound; a read the log cannot record, or any other capacity failure of a read, stops the evaluation with `Stop::Policy` and is never returned to the policy. On success inside a `look`, that log is appended to the look's log; at the head it is dropped. It checks every entry's `StoreId`, empty scopes included (`Foreign` → `Stop::Policy`), and returns `Authorized { scope: KeyScope, digest: Digest, seq: u64 }`, with the digest by AL3. `serve`'s policy execution calls it. **Grant targets (B2).** `catalog_table[K, V](tx: Tx, take name: String, take key: Key[K], take value: Codec[V]) -> Result[Table[K, V], Error]` names any table in the Tx's catalog with a handle's codec check (`Missing`, `Schema`), and commits nothing; naming a table does not make it readable. **Containment (M5).** `scope_within(outer: KeyScope, inner: KeyScope) -> bool` is `narrow`'s relation, the existing private function made public: equal `StoreId`, and each inner entry inside one outer entry of the same table with equal or greater access. **Sendable scope (B2).** `KeyScope` and its entries are `send`. | §1.4; §1.5; §1.8; §3.1 |

## CHANGES-v2.28 (caller-scope second attack; RUNNER-SPEC v2.17)

It supersedes v2.27 AK2's `Schema.migrate` and AK3's public `reply` and `record`. The `serve` shape was accepted by the checker with the Tx built inside the bound; lending an outer `mut Tx` to the bound was refused (MEM-30), and moving the `Completion` inside the closure was refused (LANG-14) (RUNNER-SPEC v2.17 header).

| # | Item | Verdict | What v2.28 does | Where |
|---|---|---|---|---|
| AL1 | The runner's callback could not consume a `Completion`, a crashed open's position had none, and a fault lost the run's digest (RUNNER-SPEC v2.17 B4, B2) | **CHANGED: `serve` owns the sequence** | `pub fn serve(mut db: Db, take c: Completion, plan: Plan, take rec: Option[CallRecord], policy: scoped[view] fn(Tx) -> Result[KeyScope, Capacity], body: scoped[view] fn(mut Tx) -> Result[Bytes, Bytes]) -> Result[Served, Error] waits`. `Plan { n_p, s_p, n_e, s_e, reply_max }` holds the policy's and the body's byte and step allowances and the longest reply. `serve` runs two isolated executions over the head's snapshot, with nothing committed between (it holds `mut db`). The policy's, under `s_p`, builds a Tx from views, runs the policy and computes the scope's digest (AL3), returning both. Between the two, outside both, `serve` sets that digest in its local `c`, so it survives any later exit. The body's, under `s_e`, builds a second Tx narrowed to a `clone` of the scope, moves it into the body's bound, gets it back on every normal exit, and writes the reply (`Replied(r, scope_digest)`) from a view of `c`. A raise is caught by that execution's `isolate`. After the body's execution it attaches `rec`, encodes the frame and commits, so the record and the reply commit together or not at all. `pub type Served = Committed(Ticket) \| Kept(Ticket) \| Failed(Pending, Stop) \| NoRoom` and `pub type Stop = Raised(u32) \| OutOfSteps(u64) \| OutOfBytes(Capacity) \| Policy`: `Committed` for `Ok`; `Kept` for `Error` (the reply alone, F2); `Failed` for a raise, an exhausted bound, a reply over `plan.reply_max`, or a policy that faulted, ran out or returned a `Foreign` scope (AL3), with the Tx dropped, which rolls it back; `NoRoom` when the commit passes `resident_limit`, with the Tx, the record and `c` dropped and the position left `Admitted`. `pub opaque type Pending` (not copy, codec or send) holds `c` with its digest (the run's, or `none`'s for `Policy`) and `rec`. `pub fn conclude(mut db: Db, take p: Pending, take r: Bytes) -> Result[Served, Error] waits` writes `r` with the Pending's digest and record in one streams-only commit and runs no code; it gives `Committed` or `NoRoom`. `pub fn untried(take c: Completion, take rec: Option[CallRecord]) -> Pending` gives `none`'s digest, for a position that runs nothing (a gone key). `Admission` becomes `Admitted(Completion) \| Abandoned(Completion) \| Duplicate(Stored) \| Rejected(u64) \| RequestMismatch \| HistoryUnavailable`; `Abandoned` carries a `Completion` for both of its rows, and `ticket(c)` gives the admit's ticket. The public `reply` and `record` are deleted; no body ever holds a `Completion` or a `Pending`. A copy of store.hirsel with `serve`, `conclude` and `untried` was accepted (63 files). These were refused: the digest set inside the policy's execution (MEM-30), a nested `isolate` around the body taking the Tx mutably (FAIL-16), and both executions inside one outer bound (MEM-30). The store already has local names `answer` and `pending`, hence `conclude` and `untried` (LANG-7). `serve` returns `Error` for a store failure, after which the position stays `Admitted`. | §1.8 (:547-548, :737-740, :786) |
| AL2 | A duplicate returned bytes without the digest it ran under (RUNNER-SPEC v2.17 B2) | **CHANGED** | `Duplicate(Stored)` with `pub type Stored { reply: Bytes, scope_digest: [32]u8 }`, for streams and keyed claims alike. The store decides nothing about release; the runner's writer compares the digest (RUNNER-SPEC v2.17 §3). | §1.8 |
| AL3 | Two stores could give two scopes one digest (ATTACK-2 B2) | **CHANGED: the digest is local to its store** | The digest input becomes `u8 15 ‖ "hirsel.scope.v1" ‖ StoreId (hi u64-BE ‖ lo u64-BE) ‖ body`, 16 bytes more than AK1. `scope_digest(db: Db, s: KeyScope) -> Result[[32]u8, Foreign]`: a scope with an entry whose table or index belongs to another store is `Foreign`, and `serve` reports it as `Failed(_, Policy)`. `none` digests body `u8 0 ‖ u32-BE 0` under this store's id. | §1.4 |
| AL4 | `Schema.migrate` let H keep a bit derived from row data (RUNNER-SPEC v2.17 B3) | **CHANGED: two phases** | `pub type Migration` (not codec, not send) is made by the runner for the `migrate` phase only. It offers `create_table`, `create_index`, `table`, `index`, `migrate` (§1.2) and `drop_table`, which commit as §1.2 says, and nothing else. `Schema` keeps `table` and `index` and only **opens**: a name not in the catalog is `Missing(name)`, a schema that does not resolve is `Schema(i)` as before, and it never commits, so `table` over a `Schema` never appends a schema number. A schema change therefore belongs to `migrate`. | §1.2 |
| AL5 | Replay could not compare the authority (RUNNER-SPEC v2.17 M1) | **CHANGED: the CALL section carries it** | Tag 2 gains `authority (u32 length ‖ bytes)` after `revision`. The bytes are AK4's: 64 B (`code.revision(policy)` 32 and four u64) plus the launch digest's length L. A call section that ends before it is `Corrupt(Malformed)`. | §2.4 (:968, :979) |
| AL6 | The sizes still said 41 + R, and the read log was unbounded (RUNNER-SPEC v2.17 M2, M4) | **CHANGED** | An `Admitted` value is 41 B and a `Replied` value 73 + R (position 8, request digest 32, state 1, scope digest 32, reply R). A replied row costs 62 + r8(S) + r8(73 + R); an admitted row is unchanged at 118 for S = 8. Pack leaves: 49 + S admitted, 81 + S + R replied. The one-row call example grows 273 → 305 from the digest, and to 373 + L with AL5's authority. An admission frame stays 92 + S. L29's generator uses both value sizes. `serve` keeps no read log for the body (the head is the only writer and no transact result uses a read set); a `look`'s log is its read set, charged as RUNNER-SPEC v2.17 M4 says. (v2.29 AM8, ATTACK-4 B3: the policy's evaluation has one fresh read log inside its bound on every path, dropped at the head.) `serve`'s own work outside the two bounds is derived in AM5 (ATTACK-4 B8: the issued-ID loop is outside, as built, so `ADAPTER_STEPS` is replaced by A_s; VERIFY-5 B8: I4's settle is the head's, outside `serve`'s step bound); the digest, its sort and its encoding run inside the policy's bound. | :868, :979, :981, §3, L29 |

v2.27 (RUNNER-SPEC v2.16, lane caller-scope-1128, astra's attack on v2.26): text only. A reply is stored with the digest of the scope it ran under (AK1). `handles` gets a schema-only handle (AK2). Completing a position needs a `Completion` that only `admit` makes (AK3). A call record names the policy that authorized it (AK4). AJ1's fallback is corrected, and misses and empty ranges are reads (AK5). The streams value and `CallRecord` gain bytes (AK1, AK4); no store has been deployed, so the cutover is a fresh store (RUNNER-SPEC v2.16 m3).

## CHANGES-v2.27 (caller-scope attack fixes; RUNNER-SPEC v2.16)

| # | Item | Verdict | What v2.27 does | Where |
|---|---|---|---|---|
| AK1 | A stored reply could be released to a caller who lost the right to read it (RUNNER-SPEC v2.16 B2) | **NEW: `scope_digest` beside every reply** | The streams value becomes (position, digest, Admitted \| Replied(reply, scope_digest)), and a keyed claim's reply the same. `reply` takes the digest from its `Completion` (AK3), which the adapter fills before the body runs. `scope_digest(s: KeyScope) -> [32]u8` is SHA-256(`u8 15 ‖ "hirsel.scope.v1" ‖ body`). For the outermost scope, body = `u8 1`. Otherwise body = `u8 0 ‖ u32-BE count ‖ entries`, each entry encoded as `u32-BE table ‖ u8 whole ‖ u8 write ‖ len32 lo ‖ lo ‖ len32 hi ‖ hi`, sorted bytewise with duplicates dropped, so two scopes with the same entries have one digest whatever order the policy built them in. Cost: 32 B per reply. | §1.4, streams (:731) |
| AK2 | `handles` received `mut Db`, so H could hold a `Snapshot` and read outside every read set (RUNNER-SPEC v2.16 B3) | **NEW: `Schema`** | `pub type Schema` (not codec, not send) is made by the runner from its `Db` for the duration of `handles` only. It offers `table`, `index` and `migrate` as `Db` does, and nothing else: no `snapshot`, `look`, `run`, `admit` or row read. It refuses a name in the reserved `hirsel.` namespace with `Exists("reserved")`. | §1.2, §1.4 |
| AK3 | `reply` took a stream name and position, so any `mut Tx` could complete any position (RUNNER-SPEC v2.16 B4) | **CHANGED: `admit` returns a `Completion`; `reply` and `record` consume it** | `Admission.Admitted` carries `Completion`, an opaque value that is neither copy nor codec nor send. It names the stream (or claim), the position and the bound request digest, and holds a slot for the scope digest that the adapter sets once (`set_scope(mut c, s: KeyScope)`). `pub fn record(mut tx: Tx, c: Completion, take rec: CallRecord) -> Result[(), Error]` views it, and `pub fn reply(mut tx: Tx, take c: Completion, take r: Bytes) -> Result[(), Error]` consumes it. So in one Tx `record`, when there is one, comes first, and `reply` is last, and a position is completed exactly once. The stream-and-position forms are deleted. A `Completion` whose scope digest was never set is `Missing("scope")` at `reply`. When the policy itself fails, there is no scope to digest: the adapter sets `none(tx)`'s, and the reply-only `Failed` it writes carries no application bytes, so the digest is never compared. | §1.4 (:536, :727-728) |
| AK4 | Replay could not tell that the policy had changed (RUNNER-SPEC v2.16 M1) | **NEW: `CallRecord.authority`** | `authority: Bytes` = `code.revision(policy) ‖ u64-BE policy n ‖ u64-BE policy s ‖ u64-BE entry n ‖ u64-BE entry s`, followed by the launch digest when the runner has one. The store treats it as opaque bytes and journals it with the record. | §1.7, §1.8 |
| AK5 | AJ1's fallback ran the policy in a separate `look` and passed its scope in, which would lose the policy's reads | **CORRECTED** | If a nested view cannot share `look`'s read log, `look` runs the policy and then the scoped body itself, on one snapshot and one log. A scope precomputed by another `look` is never used. In either form a `fetch` that finds nothing records its Point, and an empty `range` records its Span (I1), so a policy that grants because a row is absent re-runs when that row appears. | §1.4 (:4268 in store.hirsel) |

v2.26 (RUNNER-SPEC v2.15, lane caller-scope-1128, GAPS.md gap 3): text only. A query can now run narrowed to a caller's scope: `narrow_view` (AJ1). A span named without reading obeys `range`'s scope rule: `span` (AJ2). The two whole scopes are public (AJ3). §1.7's forward note on the principal is superseded (AJ4). No byte of the format changes, and no model changes.

## CHANGES-v2.26 (a caller's scope over views; RUNNER-SPEC v2.15)

| # | Item | Verdict | What v2.26 does | Where |
|---|---|---|---|---|
| AJ1 | A query (a view `Tx` inside `look`) could not be narrowed: `narrow` takes `mut Tx` | **NEW: `narrow_view`** | `pub fn narrow_view[R](tx: Tx, take within: KeyScope, body: scoped[view] fn(Tx) -> R) -> Result[R, Error]`. It has `narrow`'s containment rule and errors (AE4 e), and it runs the body on a view Tx that shares `tx`'s roots, read log and issued log, with `within` as its scope. Reads made through it are recorded in the same `Seen.reads`. Nothing is swapped back, because `tx` itself is unchanged. **Build probe:** whether a scoped `Tx` can hold a second view of the read log, as `look` makes the first (store.hirsel:4282). If it cannot, see AK5 (v2.27): `look` runs the policy and then the scoped body itself. A scope precomputed by a separate `look` is never used. | §1.4 |
| AJ2 | A Cover names a span without reading it, so no scope check applied to it | **REPLACED: `key_span` → `span`** | `pub fn span[K, V](tx: Tx, t: Shape[K, V], lo: Bound[K], hi: Bound[K]) -> Result[Read, Error]` gives the data-tree span `key_span` gave. It is `OutOfScope` when the span is not within the union of `tx`'s scope entries for `t`, of any access, by AE4 b's rule. It records nothing in the read set. `key_span`, whose only caller was RUNNER's `cover`, is deleted. | §1.4 |
| AJ3 | A place's policy must say "everything" or "nothing" | **NEW: `everything(tx)`, `none(tx)`** | `pub fn everything(tx: Tx) -> KeyScope` is the outermost scope (AE4: one ReadWrite entry per table, tables made later included). `pub fn none(tx: Tx) -> KeyScope` has no entries, so every read and write under it is `OutOfScope`. | §1.4 |
| AJ4 | §1.7: "the principal joins in M5a as a new non-MUST kind" | **SUPERSEDED (RUNNER-SPEC v2.15 item 7)** | The Caller is the stream key's prefix (`caller_key ‖ u32-BE(stream)`), so `CallRecord.stream` already names it, and replay decodes it from there. No frame kind is added, and the reserved kind 0x0060 PRINCIPAL is retired, never reused. | §1.7, §2.7 |

## CHANGES-v2.25 (keyset pages; gap study 1117 #6)

| # | Item | Verdict | What v2.25 does | Where |
|---|---|---|---|---|
| AI1 | GAPS.md #6: `scan` and `find` return a cursor, but no bounded page and no exact resume position exist. `find` takes `Bound[J]` only, so a caller cannot resume between entries with equal J | **NEW: pages over the existing byte cursor; an in-process continuation** | `scan_page`/`find_page` and `scan_next`/`find_next` (§1.5). The limits are rows and bytes, and zero is `Missing`. A page holds at least one row when one remains. A one-key peek ends the last page with `next` = None. `Resume` binds the Snapshot's identity, the table or index id, the upper bound on the logical key and the last full logical key, and resumes at Excluded(that key). Another Snapshot is `Moved(version)`, and another incarnation is `Stale(id)`. **Rejected:** a byte limit that refuses an oversized first row (a new `Limit` case, and a caller below one row's size is stuck); a continuation that holds the Snapshot (the caller already owns one, and a hidden hold would keep old nodes alive with nobody accounting for it); a `codec` continuation now (a served token must also bind caller and scope, gap #3, and is http.md B-11's). | §1.5, L36 |

## CHANGES-v2.24 (request identity at stream positions; Sam ruled C)

| # | Item | Verdict | What v2.24 does | Where |
|---|---|---|---|---|
| AH1 | AG2 left P2 and P3 to the client: `admit` took no request bytes, so changed bytes at a retained `p` could get another request's `Duplicate` or run after its `Abandoned` | **RULED C (Sam, DECISIONS 2026-11-17). This is the /wholehog option:** one digest, computed by the server, compared by the store at admit and by both runner join paths, so no path shares an answer without it. **Rejected:** A, the v2.23 client-only rule (smaller, and Kafka/Convex practice, but a buggy or non-SDK client gets another request's reply); B, the digest without `HistoryUnavailable` (with latest = 7, new bytes at 0 still got `Superseded`) | **The digest.** `request_digest(key, args)` is SHA-256 (std, std-sha256-1071) over `u8 17 ‖ "hirsel.request.v1" ‖ u32 len(key) ‖ key ‖ u32 len(args) ‖ args`: versioned, domain-separated, length-delimited. `key` is the immutable entry Key (16 identity ‖ 8 args_fp ‖ 8 result_fp); `args` are the exact argument bytes. The slot, the deadline and the server's executable revision are not inputs: a reconnect remaps slots, a retry changes its deadline, and deployed code may change. The runner computes it at ingress (RUNNER-SPEC v2.11.3 row 1) and never takes a digest from a client. **At admit:** the stream value is (`p`, digest, Admitted \| Replied(reply)). At `p` = latest the digest is compared first; a different digest is `RequestMismatch`, with nothing committed and nothing run. An equal digest gives `Duplicate` or `Abandoned` as before. Identical bytes at a new admissible `p` are a new call. **The digest is kept** through a reply, a reply-only `Failed`, a typed error's reply (F2), and an earlier open's `Abandoned` re-commit. Only `Admitted` binds a digest: pre-admission refusals, `Rejected`, `RequestMismatch`, `HistoryUnavailable` and `Full` bind nothing. **Model:** `model_v224_client.py`: the three v2.23 client mutants run as buggy clients and the store keeps law I for each; seven store mutants, all CAUGHT. | §1.8, L33, L35 |
| AH2 | `Rejected(l)` with `p` < l was the proof of `Superseded`, but the store keeps one state per stream, so it holds no digest for an older `p` | **RULED: `HistoryUnavailable`** | Below latest, `admit` answers `HistoryUnavailable` and commits nothing. The store cannot say whether `p` ran these bytes. The answer is terminal for the sender: the stream retires, and no replacement work is started automatically (WIRE W4c fences; work whose bytes were lost stays `LocallyUnknown`). The store does not mark the stream: a later `p` = latest + 1 from the stream's owner still admits. Keeping historical digests would break one state per stream (M4-PLAN §2.4). `Rejected(l)` now always means `p` > l. RUNNER's `Superseded` loses its store basis (followups to runner-spec-1020). | §1.8 |
| AH3 | Byte cost of the digest | **ACCEPTED as ADVICE "Byte cost" derives it** | The digest is embedded fixed-size, with no length. Stream value 9 + R → 41 + R. Accounted row 62 + r8(S) + r8(9 + R) → that + 32: for S = 8 an admitted row is 86 → 118 bytes. One-stream admission COMMIT 60 + S → 92 + S (68 → 100 bytes for S = 8). Pack leaf entry 17 + S + R → 49 + S + R. §2.4's transact example 241 → 273. Each committing admit, cross-open re-commit and reply writes 32 more bytes; `Duplicate`, `RequestMismatch`, `Rejected`, `HistoryUnavailable` and the same-open `Abandoned` write nothing. Hashing is O(key + args) per send, with no request copy kept. | §1.11, §2.4, L29 |

## CHANGES-v2.23 (client positions; the published value of a row)

| # | Item | Verdict | What v2.23 does | Where |
|---|---|---|---|---|
| AG1 | S-a (F8): the admit table said which `p` the store admits but gave a client no rule for refusals or for re-using `p` | **ACCEPTED; stated as client rules P1 and P2 beside the admit table** | **The store fact the rules rest on:** `admit` takes no request bytes. Every answer depends only on (stream, `p`) and the stream's state, and the bytes that run at an admitted position are those of the send the runner ran there. **The answer table** says which answers change the stream (`Admitted`, and the `Abandoned` re-commit for an earlier open) and which leave it as it was (`Duplicate`, `Rejected`, same-open `Abandoned`, AE3's `Full`, and a refusal before admission, which never calls `admit`). **P1:** `p` advances only after a reply or a `Superseded`. A pre-admission refusal does not consume `p`, and the client re-uses that `p` for its next request only when the refusal answered the request's only send. **P2:** a client never sends `p` with other bytes while an attempt at `p` may still be admitted or running. **Hand trace for P2:** attempt A at `p` is admitted and running, and request B is sent at `p`. If A replies first, B gets `Duplicate(A's reply)`. If A faults, B gets `Abandoned` and B's bytes run at `p`, so A's caller never learns that A did not run. **Model:** `model_v223_client.py` with laws H (no hole) and I (identity) and four mutants, all CAUGHT. RUN: 7,018 states, 0 failures. The receipt counts how often each answer was reached. A client that follows the rules never meets `Superseded`, because it never sends below latest, so that branch is reached only under the mutants. WIRE W4c's `refused_p_reused` and `commit_before_send` are the built client laws. The store halves are L13 (gap) and law D. (v2.24: `admit` now takes the request digest, AH1; the answer table, P1 and P2 are rewritten.) | §1.8, L33 |
| AG2 | S-b (F6, F7): what `Rejected(l)` with `p < l` proves | **ACCEPTED; client rule P3** | `Duplicate(r)` and `Superseded` prove that position `p` completed with the bytes that ran there. They prove nothing about bytes sent later. Recovery re-sends the original principal, stream, `p` and request bytes (WIRE §4 rules 1–4). Work lost with its bytes is never sent at the old `p`: WIRE reports it `LocallyUnknown`. Mutants `recover_new_bytes` and `no_directory` are CAUGHT by law I. `no_directory` restarts at `p` = 0 on the same stream after the durable state is lost, which is WIRE's lost-stream case. **The /wholehog end state, not taken here:** the store could keep a digest of the request bytes with the admitted position and refuse a mismatch, so that P2 and P3 would be enforced rather than only stated. That is a format change to the `streams` value, and none of the filed items asks for it, so it is put to the orchestrator as a question. **Superseded by v2.24 (AH1, AH2): Sam ruled C, so the store compares the digest; P2 and P3 are enforced at latest, and below latest is `HistoryUnavailable`.** | §1.8, L33 |
| AG3 | S-c (F10): `Change.value` is the stored row, a u16-LE schema number followed by the codec bytes (`put_impl` :1883), so it is not a wire value | **ACCEPTED: one typed store function gives the published value; `Change` is unchanged** | `published_value(t: Shape[K, V], c: Change) -> Result[Option[Bytes], Error]`. For a deleted key it gives None. For a row stored under the Shape's current number it gives the stored codec bytes after the 2-byte prefix, which are the bytes `put` wrote with the current codec. For an older number held by the Shape's Readers, it reads the row with that number's Reader and encodes it with the current codec. A number the Readers do not hold is `Stale(id)`, as `row_value` gives. A value shorter than 2 bytes is `Decode(Truncated(0))`. A `tree_key` whose 4-byte id is not the Shape's table, such as an index row's key, is `Missing("table")`, and that is checked first. The key is not touched: `Row.key` is `tree_key` itself. No Snapshot is taken, because a table id is never reused (I3) and the Shape fixes the numbers. **`look` and `get`** give typed values, never stored bytes, so only `Change.value` carries the prefix. **Erasure:** the function is typed, and the store adds no erased type. Proposed to runner-spec-1020, since RS v2.11 does not say: R2's adapter (R2-b) erases it per Cover in `cover(t: Shape[K, V], …)`, as `erase` (:1753) erases an index extractor. **Check order:** the table id first, then None, then length, then number. So an index row's Change, including a deleted one, is `Missing("table")`. **Rejected:** making `changes` itself normalize. `changes` takes two Snapshots and holds no typed Readers, so it would need a table registry, a second source for what the Shape already holds. **Law:** L34, with hand-derived bytes and mutants `strip_two_bytes` and `raw_value`, named as RUNNER-SPEC v2.11 L16 `rows_normalized` names them. **Initial rows (RS v2.11 §3 "Rows"):** the adapter serves a Cover's initial rows as well as `changes`. So `span_changes(s, lo, hi)` gives the data-tree rows of Snapshot `s` in [lo, hi), in key order, each as a Change with `Some(stored bytes)`. One function, `published_value`, then normalizes both, and a typed `scan` followed by `encode` would be a second path, which this avoids. "Current" is the Shape built at open (RS rule 12). | §1.5, L34, §7 S5 |

## CHANGES-v2.22 (one settle order for every commit; v2.19's owner)

| # | Item | Verdict | What v2.22 does | Where |
|---|---|---|---|---|
| AF1 | v2.21's question: §1.11 gave only `run` the settle step (I4), so `admit` and schema commits could answer `Full` on conservative charges that a settle would have removed | **RULED (orchestrator): every commit path settles first, exactly as `run` does; no commit kind skips it. This is the /wholehog option: one settle-then-check order for every commit, with like commits treated alike.** | **The rule.** Before any commit is checked against `resident_limit`, the writer settles the charges added since the last cut under a pin or branch. A `memory` store counts inline; a disk store requests a checkpoint and waits for its ack. Then the check runs on the exact figure. This holds for every commit that can be refused for the limit: `run` (which answers `Refused(c)`), a committing `admit`, every schema commit (`create_table`, `create_index`, `drop_table`, `migrate`, and a schema number appended by `table`), and `fork`. The last three answer `Full(c)`. A `Full` or `Refused` therefore always rests on the exact figure. Each of these is already `waits`, so the settle adds no new wait kind. **Why one order:** with `admit` exempt, a stream under a pin could be told `Full` while the exact account had room. The runner would then treat a transient conservative charge as a capacity failure. **Rejected:** settling only on `Refused` paths, which is v2.21's text, now withdrawn. **Law:** L27 gains (c) and the mutants `admit_skips_settle` and `schema_skips_settle`. | §1.11, §1.8, L27 |
| AF2 | v2.21's question: the S5 ruling named store-build-1010 as v2.19's owner, but v2.19's header said step-bounds-1051 builds it | **RULED (orchestrator): store-build-1010 builds v2.19; step-bounds-1051 did only the lowering** | The v2.19 header now names 1010. Nothing else in v2.19 changes. | v2.19 header |

## CHANGES-v2.21 (S5 rulings: streams and scopes)

| # | Item | Verdict | What v2.21 does | Where |
|---|---|---|---|---|
| AE1 | ruling (1): which `p` a new stream admits. The admit table's first row read "or the stream is new", which admitted any `p` there. | **RULED: only `p` = 0; any other `p` is `Rejected(0)`.** `model_v23_streams` already refused it and committed nothing, but it gave no position; `model_v221_streams` gives 0. | A new stream that admitted `p` > 0 would have latest = `p` with 0..p−1 unreplied, which breaks S7 (the model's mutant `new_any_p` is caught by law A). The row now reads: a new stream and `p` = 0 commits `Admitted(0)`; a new stream and any other `p` is `Rejected(0)`, with nothing committed. `Rejected(0)` with `p` > 0 claims nothing about `p`: S7's proof covers only `p` < l, so the runner's `Superseded` never fires on it. /wholehog: one rule, "admit only latest + 1", where a new stream's latest + 1 is 0, so like positions are treated alike. Rejected: `Missing` for an unknown stream, which would give the same wrong-position case a second answer. | §1.8, L13 |
| AE2 | ruling (2): the error for a `reply` without its admit | **RULED: `Missing("reply")`** | This is the string `record` already uses. No `Error` case is added. | §1.8 |
| AE3 | ruling (3): `admit` against `resident_limit` | **RULED: `Error(Full(c))`, nothing committed, as schema commits do** | Only the two rows that commit consult the account: `Admitted(p)`, and the `Abandoned` re-commit for an earlier open. `Duplicate`, `Rejected` and the same-open `Abandoned` commit nothing, and they answer whatever the account holds. `c` = `Capacity{requested: the account after it, available: resident_limit}`. Like a schema commit, `admit` does not settle unsettled charges first. Whether `admit` and schema commits should settle, as `run` does (I4), is put to the orchestrator; it is not ruled here. (Superseded by v2.22 AF1: they settle.) It is `Full`, not `Refused`, because `admit` is not a `run` and has no `Outcome`; `Error(Full)` is the store's answer for a commit outside `run`. Model: each admit move has a twin past the limit. The mutant `full_after_commit` (it commits, then answers `Full`) is caught by law E: the next admit of `p` in this open has no ticket to give back. | §1.8, §1.11, L13 |
| AE4 | ruling (4): what a scope covers, and how `range`, `lookup`, `union` and `narrow` use it | **RULED (a)–(e); the /wholehog option is the ruling as given, with five consequences newly stated** | **(a)** A `KeyScope` is a list of entries (table, lo, hi, access), with bounds encoded as §1.3 encodes a span. An entry for `t` covers `t`'s rows whose `Key[K]` bytes lie in [lo, hi), and `t`'s index rows whose primary key it covers, with the same access. **(b)** A `range` whose span reaches outside the scope is `OutOfScope` before any read. The store decides it on the encoded span, before it reads any row: the span's [lo, hi) must lie within the union of the scope's encoded intervals for its table, by byte compare. The entries cover together: two entries whose encoded bounds meet cover a span that neither covers alone. Newly stated here: (iv) a byte gap between entries is outside the scope, even when no key of K can fall in it. On u64 keys, entries `Included(0)..Included(4)` and `Included(5)..Included(9)` leave the bytes from `Key(4) ‖ 0x00` up to `Key(5)` uncovered, so `range(t, Included(2), Included(8))` is `OutOfScope`. Entries meant to be adjacent end with `Excluded(k)` and start with `Included(k)`, and those meet. (v) A span whose encoded lo is not below its hi reads nothing and is never `OutOfScope`. **(c)** A `lookup` is `OutOfScope` up front unless the scope has an entry, of any access, for the index's table. `next_indexed` then gives `OutOfScope` at the first row whose primary key is not covered; it returns no row and records no Point for it. **(d)** `union` keeps both scopes' entries without merging; a key is `ReadWrite` if any entry covering it is. **(e)** `narrow` accepts `within` only when every inner entry lies inside one outer entry with equal or greater access. **Newly stated here:** (i) the outermost scope is one `ReadWrite` entry with `Unbounded` ends for every table, including tables created later, so (c) and (e) hold outside any `narrow`; (ii) after (c)'s `OutOfScope`, the `IndexRows` stays at that row, so every later `next_indexed` gives `OutOfScope` again, and the Span recorded when the lookup was made stays in the read set; (iii) the asymmetry between (b) and (e) is recorded, not reconciled; (iv) and (v) are under (b). With the current scope {t: 0 ≤ k < 5, t: 5 ≤ k < 10}, `range(t, 2, 8)` succeeds, but narrowing to t: 2 ≤ k < 8 is `OutOfScope`; the caller narrows to the two entries instead. The scope model counts 4,620 such narrows that are sound by key yet refused. A narrow that checks the union would need entries merged, and that was not ruled. **Model:** new, `model_v221_scope.py`. Its store side compares encoded bounds; its oracle enumerates the keys each bound names. It checks every scope of up to two entries over two tables (112 entries, 2,686,600 checks, 0 failures), and 10 mutants are CAUGHT. | §1.4, L14 |

## CHANGES-v2.20 (`CallRecord.revision`)

| # | Item | Verdict | What v2.20 does | Where |
|---|---|---|---|---|
| AD1 | code-revision-1038 V4: RUNNER-SPEC §7 (R5) re-executes "under the recorded code" and answers `Unknown(RevisionUnknown)` when it cannot, but `CallRecord { key, args, stream }` held no revision. The key's 16 identity bytes name the function, not its code (PROG-9: a body edit keeps the identity). | **RULED (orchestrator, /wholehog): the record carries the revision of the entry that ran.** | **The field.** `CallRecord` gains `revision: Option[Bytes]`. It is `code.revision`'s `Option[View[Bytes]]`, copied into owned bytes so that it outlives the StaticFn's scope. `Some(r)` holds exactly the 32 bytes that `code.revision` (or, natively, `hirsel_code_revision` for the entry's root) gave for the registered entry's `StaticFn`, in the run that wrote the record. The runner reads it once at registration and keeps it in the Slot beside the identity. The store never computes, fills or changes it. `record` refuses a `Some` whose length is not 32 with `Missing("revision")`, the existing case, as W1 used it. No `Error` case is added. **What None means, precisely.** `None` means that `code.revision` returned None for that entry in the run that recorded it. By PROG-9 that happens only when the program was assembled without its revisions. A program `hirsel build` links is never assembled that way, so on cranelift and llvm every registered entry has `Some`. The evaluator gives None unless its harness passes the `Revisions` table. It is not "unknown, probably equal", and it is not 32 zero bytes. **Entries with no record.** An unregistered or gone entry has no record at all (the `Failed(Gone)` row, `Unknown(NoEntry)`), so it never needs a revision. The runner's reply-only `Failed` commit for a registered entry carries that entry's revision like any other record. **What R5 does with it** (RUNNER-SPEC §7 owns the verdict, and this only restates it): when the recorded revision and the candidate's are both `Some` and equal, re-execution proceeds to `Same` or `Different`. If either is None, R5 answers `Unknown(RevisionUnknown)`. It never compares identities instead, and never treats None as equal. What R5 answers for two different `Some`s is §7's choice. **Format.** The call section (tag 2) gains one field after `stream`: `revision` (u32 length ‖ bytes), where the length is 0 for None or 32 for Some. Any other length is `Corrupt(Malformed)`. This is the same length-prefixed form as the other three fields, so like fields are alike. The section grows by 4 bytes with None and by 36 with Some. **Rejected:** a revision per deployment in the activation row. The call already names its entry, so that would be a second source for the same fact (1038's reasoning, adopted). **Stores from S3:** a store written by the S3 build with call sections does not read under this layout. Format 1 is not released, and those stores are test stores, so there is no migration (/wholehog: one layout, no compatibility path). | §1.3, §2.4, L24 |

## CHANGES-v2.19 (`run`, `run_on` and `look` take a step bound)

| # | Item | Verdict | What v2.19 does | Where |
|---|---|---|---|---|
| AC1 | RUNNER-SPEC v2.9 row 3 puts `bound steps s` inside `store.execute`'s isolate, around the Tx's creation, `body(mut tx)` and the frame's encoding, and inside `look`'s isolate around the query call. The scope must own the Tx (MEM-30 "Places outside"), so the store has to take `s`. | **RULED (orchestrator, 2026-11-16): one parameter, `steps: Option[u64]`, on `run`, `run_on` and `look`, with no separate bounded entry points. `Outcome` and `Looked` each gain `Stopped(core.OutOfTime)`.** | **Shape.** `run(mut db, steps, body)`, `run_on(mut db, b, steps, body)`, `look(s, steps, body)`. `None` opens no step scope. The call is then exactly v2.18's, and any enclosing bound still counts its steps. `Some(n)` runs `isolate(fn() { bound steps n { make Tx; body(mut tx); encode the COMMIT } })` for `run` and `run_on`, and `isolate(fn() { bound steps n { body(tx) } })` for `look`. **A stop.** The scope returns `Error(OutOfTime { steps: n })`, and the call returns `Ok(Stopped(o))` with that value. The Tx was the scope's own, so it is gone, and with it every write. No frame was encoded and none is sent. Nothing is published, the version and the account are unchanged, there is no ticket, and `recorded` finds nothing. If `n` is more than the enclosing bound has left, the body does not run, and the result is the same `Stopped` (MEM-30). The steps of the store's own code inside the scope (path copies, index upkeep, the encode) are the entry's, as RUNNER-SPEC row 3 says for decode and encode. The account check, the publish point and the handoff run after the scope closes, so a stop can never fall between them. **A mixed chain** (the FAIL-16 scope ruling): if cleanup adds a cause other than the bound's `OutOfTime`, the failure leaves the step scope, and the isolate catches it as an ordinary failure. The result is `Failed(causes)`, with the same rollback as any `Failed`. **An enclosing bound's `OutOfTime`** is not this scope's. Inside that bound's scope, `isolate` does not catch it (FAIL-16), so it passes out of `run` or `look` as a failure, as it always did. The store never turns it into `Stopped`. **The runner** maps `Stopped(o)` to `Failed(OutOfTime(o.steps))` (RUNNER-SPEC row 4). The store makes no reply commit for it: that is the runner's, as for every `Failed`. **Why one parameter (/wholehog):** one entry point per operation, with no bounded twin that could drift from the unbounded one. `Stopped` is its own variant because a stop is neither a fault of the body (`Failed`) nor a refused admission (`Refused`), and the runner answers it differently. | §1.3, §1.5, §3.1, L32 (new) |

## CHANGES-v2.18 (`migrate` takes the target table's full descriptor)

| # | Item | Verdict | What v2.18 does | Where |
|---|---|---|---|---|
| AB1 | 1010's S4 gap: `migrate` changes V to V2 and promised to rebuild the indexes, but it took no extractors for V2. A Table's registered extractor is an erased `fn(V) -> Bytes` that cannot read V2, and a new extractor has a different `code.identity` in the catalog. No rule covered dropping, refusing or reading back. | **the orchestrator's lean, which is also the /wholehog option: `migrate` takes a `TableSpec[K2, V2]` that declares the target's name, codecs and indexes. In the same commit it drops every old index and builds the target's indexes from the migrated rows. `create_table` takes the same `TableSpec`, so like tables are declared alike.** | **Hand trace.** Table `t` (id 5) has value `Old { a: u64 }` and index `by_a` (id 6), whose `by` = `Old.a` has identity X. Rows: k1 → Old{a: 7}, k2 → Old{a: 9}. Index entries: (7 ‖ k1), (9 ‖ k2). The program calls `migrate(db, t, s, f)`, where `s` = `spec("t", Key[u64], Codec[New])` with `with_index(s, "by_b", New.b, Key[String])` (identity Y), and `f(k, Old{a}) = Some((k, New{b: to_string(a)}))`. **v2.17:** the rebuild must call X, a `fn(Old)`, on `New` values, which is impossible. If it computed `by_a` from `Old` before `f`, the first keys would be right, but no extractor could maintain the index on later `New` puts, and the catalog would still name X. **v2.18:** in memory, `f` gives k1 → New{b: "7"} and k2 → New{b: "9"}. Then Y gives ("7" ‖ k1) and ("9" ‖ k2) under the new index `by_b`, id 8. The table gets id 7. The cut installs data, index and catalog together at version + 1: the catalog lists `t` (id 7, one schema number, New) with exactly one index, `by_b`, identity Y. `by_a` and its entries are gone, and X is never called on a `New` value. Old handles return `Stale(5)` and `Stale(6)` against current state, and read M from Snapshots older than the migrate (I3). The program then reopens its handles as usual: `table(db, "t", Key[u64], Codec[New])`, then `index(db, mut t, "by_b", New.b, Key[String])`, which checks identity Y against the catalog. **The rules.** (1) The target's indexes are exactly those the `TableSpec` declares. An old index the spec does not redeclare is dropped. One it redeclares under the same name gets a new id and is rebuilt from V2 rows by the new extractor. No old extractor is applied to V2, and no V2 bytes are read back as V. (2) `migrate` refuses, with nothing changed, if any part cannot be built: `f` returns `Capacity` (`Full(c)`); an extractor returns `Capacity` (`Full(c)`); a row key or an index key is over the key limit (`Limit(Key)`); two rows map to one K2 (`Exists(name)`, newly stated here: a migrate never silently drops a row); the spec repeats an index name (`with_index` returns `Exists(name)`); or the target name belongs to another live table (`Exists(name)`). Building happens in memory before the cut, as it already did, so nothing is ever half migrated. (3) `create_table(db, s)` creates the table and every index in `s` in one schema commit. `create_index` is still the way to add one index to an existing table. **Rejected:** migrating with no indexes and then calling `create_index` again. That takes two schema commits, and there is a committed version where the table exists without its indexes, so it is not atomic. **Rejected:** keeping old extractors through a reverse reader from V2 to V. There is no such reader in general (New{b: String} → Old{a: u64}), and it would keep a dead type alive. | §1.2, L31 (new) |

## CHANGES-v2.17 (`adopt` checks the fork's parent)

| # | Item | Verdict | What v2.17 does | Where |
|---|---|---|---|---|
| AA1 | 1010's S4 counterexample: `adopt` compared only versions, and a version names a state only within one branch | **a real lost write. `adopt(b)` now requires that `b`'s parent is main, and that main's version is `b`'s `forked_at`. This is the orchestrator's lean, and it is the /wholehog option.** | **Hand trace under v2.16.** (1) Main is at v1. `fork` b from main: parent 0, `forked_at` 1. (2) Main commits v2 = A. b commits its own v2 = B. (3) `fork` c from `snapshot_of(b)`, which is at version 2: `forked_at` 2. (4) `adopt(c)`: main's version is 2 and c's `forked_at` is 2, so it passes. Main takes c's roots, which are B, at version 2. A is gone, and main's version did not even move. **Under v2.17:** c's parent is b (id 1), not main, so the result is `Diverged(c)` and nothing changes. **The rule.** `fork(db, name, from)` records `parent` = the branch `from` was taken on and `forked_at` = `from`'s version. Both come from the Snapshot, which carries its branch and version (§1.5), and both are written to the MANIFEST's existing fields. `adopt(b)` succeeds exactly when `b` is not main, `b.parent` = 0 (main), and main's version = `b.forked_at`. Any other case is `Diverged(b)`, and the store is unchanged. That is one comparison of two fields, with no walk up the lineage. **Why it is sufficient.** Within one open, main never holds two different states at the same version. Every commit and schema commit raises the version by one. `adopt` sets it to `b`'s version, which is at least `forked_at`, and it is equal only when b made no commits, in which case the roots are main's own. Branch ids are never reused (`next_branch`). After a reopen, a fork and main's head at its fork point were taken in one cut (§1.9: fork makes a cut that carries every branch's roots), so both are durable or neither is. So when main's version equals the fork point of a fork of main, main's head is exactly the state the fork started from, and adopting is a true fast-forward. **Consequences.** A fork of a fork can never be adopted into main. Its parent may be deleted, or adopted and so deleted, and `parent` is then an id that no longer exists. That id is only compared, never resolved, so it is not `Dangling`. `adopt(main)` is `Diverged(0)`. **The other option, rejected: versions unique across branches.** A store-wide version counter would let `forked_at` = main's version alone prove the fork point. But each branch's versions would no longer run v, v+1, …. Replay checks per-branch continuity (§3.4 step 6: "the head + 1"), CallInfo derives `seed` and `id` from (branch, version) (§1.7, L18), and `recorded(db, b, v)` addresses by branch-local version. All three would change, to save one field compare on a field the format already has. | §1.9, §2.6, L30 (new) |

## CHANGES-v2.16 (`recorded`'s fast path; R4's workload)

| # | Item | Verdict | What v2.16 does | Where |
|---|---|---|---|---|
| Z1 | 1010's S4 gap: W2's step 3 skips the barrier when the requested *version* is at or below `journaled`, which is a *frame sequence*. The two count different things. | **a real defect: the fast path was unsound. It now compares frame sequences, as X1's step 0 does.** | **Counterexample, by hand.** Main makes 100 commits with frames 1–100. `durable` takes acks through frame 100, so `journaled` = 100. `fork` takes a branch from main at version 1. `run_on(fork)` commits fork version 2, and its CALL frame 101 is queued. `recorded(db, fork, 2)`: 2 ≤ 100, so the v2.13 step skips `Barrier` and reads the segment. Whether frame 101 has been written yet depends on when the journal task runs: if it has not, the answer is None, and if it has, Some. That is the timing W2 exists to remove. In S3, main is the only branch, and schema commits raise the version without a frame, so there the comparison was only conservative, and S3 is unchanged. **Why a fast path must be sound.** `recorded` may skip the barrier only when skipping cannot change the answer: every frame this Db has sent must already be written. Then the answer is the same whether or not the barrier was skipped, and only the wait differs. How many acks the writer has taken (`journaled`) depends on timing, but this soundness makes that harmless. **The rule.** `recorded` skips `Barrier` exactly when `journaled` ≥ the sequence of the last frame this Db has sent. That is the condition of X1's step 0, and it means nothing is outstanding. An acked frame was synced, so it was written. Versions from before this open were recovered from disk. Otherwise `recorded` sends `Barrier`. It reads both counters through its view and changes neither. No per-branch map is needed. **The trace again:** `journaled` is 100 and the last frame sent is 101, so `recorded` sends `Barrier`, waits for `Written`, reads, and gets Some under every schedule. **Rejected:** a per-branch acked-version map. It is sound too, but it adds state that has to be kept in step with forks and `delete_branch`, to save a wait that is only a write, never a sync. **Rejected as well:** always sending a barrier. That is sound but costs a channel round trip on every call, even when the journal is idle. | §1.3, L24 |
| Z2 | 1010 and 889: R4 said "K = 16 tables" and "history 64" and defined neither. | **my text was wrong: K is 16 *keys in one table*, as in the source figure. Both terms are now defined, byte for byte with 889's twin.** | **Source.** STORE-RECORD.md:494 records place-store-681's measurement: "K keys of 8 B with 32 B values, C commits of history … then E one-key commits", and 75.5k instructions at K = 16, C = 64. `durable_commit.rs` (889) runs the same workload: `KEYS = 16`, `HISTORY = 64`. "Tables" was my error. **The workload, commit i (i = 0, 1, 2, …):** one table. The key is `i mod 16` as a u64, so its user key is 8 codec bytes. The value is 32 codec bytes: four 8-byte words, word j (j = 0..3) is `(i × 0x9E3779B97F4A7C15 mod 2^64) rotated left by 16j bits`, little-endian, exactly as `durable_commit.rs`'s `workload(i)` writes it. Each commit is one `put` in one `run`, so its COMMIT is §2.4's 97 bytes. **History 64:** commits 0–63 are made in the same open before timing starts, each followed by `durable`. They are the journal's history: the live segment holds their 64 frames. The store retains no older version: there is no pin, no `follow`, no fork and no checkpoint. `checkpoint_bytes` is set high enough that no cut runs during the row. Both sides keep exactly these 64 frames in their log. Neither keeps 64 snapshots. **R4 measures** commits 64 … 63 + N, each as `run` then `durable` of its ticket. Per commit is (median at N − median at N = 0) / N, as in MEASURE.md, which subtracts the start-up and the history. **R5** continues the same i sequence after the same history, with no `durable` except after every 64th measured commit, for 10 s. **The twin** must also do R1's per-commit work and write the store's exact 97-byte frames (unchanged from v2.9's twin note). | §6 R4, R5 |

## CHANGES-v2.15 (the journal ack channel cannot block the journal)

| # | Item | Verdict | What v2.15 does | Where |
|---|---|---|---|---|
| Y1 | The spec never gave the journal ack channel a capacity, or said when the writer empties it. The journal task's `send(Synced)` is a waiting send. If the ack channel is full, the journal task stops before it reads the next message. A `recorded` call takes only a view, so it cannot take acks, and it then waits on `Written` forever. 1010's build uses capacity 128 and drains at the start of `run`. | **a real deadlock, fixed with a finite capacity and one drain rule. The protocol is unchanged.** | **Counterexample at 128 (= 2 × `group_commits`), by hand with `group_commits` = 1, so the frame queue holds 2 and the ack channel 2.** The journal's syncs are slow. `run` 1 sends f1, and the journal task takes it and begins its sync. `run` 2 and `run` 3 queue f2 and f3. `run` 4 drains, finds no ack, and sends f4, which waits because the queue is full. The syncs finish: ack f1, the journal takes f2, and f4 enters the queue, so `run` 4 returns. Then ack f2, then f3 is taken, and the ack for f3 meets a full channel (2 of 2), so the journal task stops. Now `recorded` sends `Barrier`, which fits behind f4, and waits on `Written`. The journal task never gets to it. That is a deadlock, and no `mut` call can come while `recorded` is waiting. **The drain rule.** Before every `send` of a `Frame`, the writer takes acks with `try_receive` until `Empty`, applying each one as `durable` does, so `Failed(e)` poisons and stops the call before the send. Every call that sends a frame takes `mut` Db, so it can do this. **The capacity.** The ack channel's capacity is 3 × `group_commits` + 3, which is 195 with the default. If `channel.new` refuses it, `open` returns `Full(c)`, as W1 does for the frame channel. **Proof that the journal's ack `send` never waits, BY READING:** (1) Every ack is either `Synced` for a group that holds at least one frame, so there is at most one ack per frame (X1: a frameless group acks nothing), or the single `Failed(e)` the task sends before it ends. (2) Look at the last drain. After it, the channel is empty, and every ack sent since then is for a frame that was unacked at that drain or was sent after it. (3) At the drain, the unacked frames are at most 2 × `group_commits` in the queue, plus `group_commits` in the group being written or synced, plus 1 frame the journal task holds back because it would pass `group_bytes` (§3.3, bounded drain). (4) After the drain, the writer sends at most 1 frame before the next drain, by the drain rule. (5) So the channel never holds more than 3 × `group_commits` + 2 `Synced`, plus 1 `Failed`, which equals the capacity. The send never waits, whatever the writer is doing. **The cases asked about.** *`recorded` waiting on `Written`:* the journal task never stops at an ack send. It stops only at `receive` when the queue is empty, and `Barrier` is queued, so it reaches `Barrier` and answers. *`recorded`'s own `send(Barrier)` when the queue is full:* the journal task drains the queue with no help from the writer, so the send goes through. *Any view Db:* `recorded` is the only call on a view that touches the journal channel. Its `Barrier` is not a frame, and a group that holds only `Barrier` acks nothing. A view can wait for any length of time and the bound still holds. *`durable`, the cut's step 0 and `close`* take acks themselves. **A monotone position instead (the other clean option), rejected.** The journal task would publish its synced `(seq, end)` into a single cell, and the Db would read it, so there would be nothing to fill. But CONC-12 allows no shared mutable state except `Latest`, and a `Latest` read never waits (CONC-13). `durable` and step 0 would still need a channel to wake on, and `Failed(e)` still needs one too. That is two mechanisms where one bounded channel does both. A finite capacity exists, so the channel stays. | §3.1, §3.3, §4, L24 |

## CHANGES-v2.14 (rotation waits for the old segment's last ack)

| # | Item | Verdict | What v2.14 does | Where |
|---|---|---|---|---|
| X1 | 1010's S3 crash sweep, cut 20: a 1,048,639-byte frame is queued for `j-5`. The cut then creates `j-6` with HEADER `prev_end` 1048711, syncs it, writes its END (`d` = 72) and runs `sync_dir`. The crash comes before the journal task writes the `j-5` frame. Reopen: `j-5` is 72 bytes long, below `prev_end`, so `Corrupt(Truncated)`. A crash made the store unopenable. | **a real defect in §3.5 steps 1–2; 1010's fix is ACCEPTED. A cut first waits until every frame already sent is acked.** | **Cause.** §3.5 step 1 took `prev_end` from segment `n`'s *current* end, the end the writer counts, which includes frames still queued. Creating `n+1` makes that number durable. §3.4 step 5 then reads `n` as a middle segment, whose valid end is `prev_end`, so every byte below it must be on disk. Queued frames were not. A write barrier (W2) is not enough: written is not synced. **The rule (new step 0).** Before it creates anything, the cut waits until `journaled` ≥ the sequence of the last frame the writer has sent. It is a condition, not a `receive`: when nothing is outstanding (right after open, for two cuts in a row, or when only `Barrier`s were sent since the last ack) it holds at once and receives nothing. Otherwise the writer receives acks, as `durable(Journal(s))` does. Then `prev_end` is segment `n`'s end as the writer counts it. Step 0 has made that end synced, because an ack is sent only after its group's `sync` (§3.3). It is not taken from the ack's own fields. **Every ordering, BY READING.** (a) *Frames queued at the cut, including mid-drain:* the writer is the only sender (§3.3) and the cut runs inline in its task, so nothing is sent between step 0 and the `Rotate`. Every byte below `prev_end` is synced before `n+1` exists. Until `n+1` exists, `n` is the last segment and its END `d` governs, which is unchanged. A crash at any step of step 1 is the existing "segment creation" cut. (b) *Rotation racing close:* `close` takes the Db (§1.10) and a cut runs inside a `mut` call, so they cannot overlap. A cancelled cut is a crash cut and falls under (a). A `Rotate` that was sent is queued before close drops the Sender, so by FIFO order it is handled before `Closed`. (c) *The prior sync fails:* the journal task sends `Failed(e)` instead of the ack and ends. Step 0 receives `Failed(e)`, stores it as poison (§3.8) and stops. Nothing is created, no `Rotate` is sent and no Cut is taken. The call ends exactly as it does for a failed host call in step 1 (V6), and that includes `run`'s background trigger, whose commit is already published. (d) *The journal dies after the ack,* for example on its idle END write: the `Rotate` `send` returns `Closed` (CONC-4). The writer receives the `Failed(e)` that precedes the end, poisons, and takes no Cut. `n+1` already exists with a `prev_end` that is synced, so reopen is exact. (e) *Open's rotation* (§3.4 step 8) already follows this rule: step 5 syncs each segment before reading it, and nothing is queued at open. **No deadlock on the 2 × `group_commits` channel.** While it waits in step 0 the writer sends nothing and receives acks, so the journal task's ack `send` cannot block on it. The queue holds only frames already sent. The journal task drains them in groups with `try_receive` and acks each group after its sync. It never waits for a full group: `Empty` ends the drain (§3.3). So every outstanding frame is acked in a bounded number of groups with no help from the writer. **Cost:** one sync wait per cut, and none when nothing is outstanding. **Frameless groups.** Separately, v2.13's loop ran `sync; send Synced(last seq, end)` after a group that held only a `Barrier`, which has no last seq. A group with no frames now writes, syncs and acks nothing. 1010 already guards this (`if frames != 0`). **The model.** `model_v2.py`'s checkpoint scenarios always finish a group before the cut (`group_steps` then `checkpoint_steps`), so no frame was ever queued at rotation. That is why the model missed this defect. L4 adds the case. **Not part of X1, and not gating the S3 land:** the capacity of the journal ack channel, and whether its `send` can block while the writer waits on a full frame queue, is unstated. It is logged as a separate followup. | §3.1, §3.3, §3.5, §3.6, §3.7, L4 |

## CHANGES-v2.13 (zero group bounds; `recorded` without writer timing)

| # | Item | Verdict | What v2.13 does | Where |
|---|---|---|---|---|
| W1 | 1010's S3 option gap: `group_commits` and `group_bytes` are unrestricted u32 values, but §3.3 needs at least one frame per write, so `group_commits` = 0 has no legal group | **`open` refuses zero for either one, with `Missing(name)`. The two bounds are treated alike.** | **The rule:** `open` checks both bounds before it opens LOCK. `group_commits` = 0 is `Missing("group_commits")`, and `group_bytes` = 0 is `Missing("group_bytes")`. Nothing is created, and no task is spawned. A zero bound means nothing: it is a bound that was not given. **The error:** the spec had no error for options. `Missing(String)` is the existing case that names an absent required thing, as `Missing("reply")` and `Missing("branch")` do. No `Error` or `Limit` case is added. **Treated alike:** `group_bytes` = 0 is not read as "every frame is oversized and goes alone". Both bounds are refused at zero for the same reason, so §3.3's oversized-frame rule applies only to a frame larger than a `group_bytes` of at least 1. **Channel capacity:** the journal channel's capacity is 2 × `group_commits`, which is 128 with the default. If `channel.new` refuses that charge, `open` returns `Full(c)`. **`memory`** has no journal and returns no `Result`, so it does not read the two fields and does not check them. | §1.1, §3.3, §3.4, L21 |
| W2 | 1010's S3 visibility gap: right after a successful `run` with a CALL section, its frame can still be in the journal queue, so a `recorded` that reads segment bytes would give Some or None depending on when the journal task wrote | **`recorded` waits for a write barrier, then reads. Compaction is decided by the Db's own cut history, not by which files exist.** | **Why:** writer timing is not an event that replay sees, so an answer that depends on it would not replay. Both waits below are channel events (CONC-10), and replay records those. **The barrier.** `Msg` gains `Barrier`. `recorded` sends `Barrier` on the journal channel. That takes only a view of the Sender (CONC-5). The Db's task is the only sender, so `Barrier` comes after every frame of every earlier call. The journal task takes messages in FIFO order. When it meets `Barrier` it ends its drain, writes its group, and after that `write` returns (before the `sync`) it sends `Written` on a second channel, `written`. That is a write barrier, not durability. The answer does not depend on grouping: only how long `recorded` waits does. **`written`.** It is created at open with capacity 2. Its Receiver is in the Db, and so is one Sender (`channel.sender`). The journal task holds the other Sender. `receive` takes a view of the Receiver. The ack channel is never read, so `journaled`, `checkpointed`, `resident` and `poison` are untouched. A view Db can therefore wait on the written position without consuming acks or touching the accounts. **Failure.** On any failure, before it ends, the journal task does `try_send(written, Failed(e))` as well as sending its `Failed(e)` ack. So any `recorded` that reaches a dead journal task finds `Failed(e)` there: as the answer to its `Barrier`, or by receiving after `send` returns `Closed`. That `recorded` returns `Poisoned(e)`. It puts `Failed(e)` back with `try_send` through the Db's own Sender, because `IoError` is a plain value, so the next `recorded` finds it too. The channel holds at most one `Written` and one `Failed` at a time, so a `try_send` never sees `Full`. The journal task still allocates nothing after spawn (Q1a). `e` is the same error the next ack reader stores. **Compaction.** The Db keeps the head and the `first_live` of the latest cut it has taken, unless it has learned that the cut was refused, in which case it keeps the cut before that. At open these are the manifest's. A version at or below that head is None, without any read. A later version is searched in segments from that `first_live` on. Those segments cannot be removed while the Db is only viewed, because only a cut taken by a `mut` call removes segments, and only segments below its own `first_live`. A refused cut becomes known when a `mut` call consumes its ack. From then on, versions it would have covered give Some again, because their segments were never removed. That is deterministic: it follows the Db's call history. **The other option, rejected:** `recorded` requires `durable`, that is, it answers only up to the Db's acked position. That is also deterministic, and it needs no new message. But a successful `run` would then read as not recorded until the caller called `durable`, which the direction rules out, and the failure path is the same in both. | §1.3, §3.3, §3.8, L24 |

## CHANGES-v2.12 (six places where the S2 check found the text silent or unclear)

| # | Item | Verdict | What v2.12 does | Where |
|---|---|---|---|---|
| V1 | §2.2 and §2.6 say a MANIFEST file is "HEADER, then one MANIFEST frame", while §2.7 lets an unknown kind stand "anywhere after HEADER", reserves AUTHORITY "after MANIFEST", and L11 (an S2 law for manifests) requires a skipped MUST = 0 frame and `Unsupported(kind)` for MUST = 1. 631305fc reads "exactly one frame" and returns `Corrupt` for both. | **§2.7 and L11 rule; §2.2 and §2.6 are restated** | A MANIFEST file is its HEADER, then frames to the end of the file, of which exactly one is MANIFEST (kind 0x0030). Any other frame is read by §2.7 rule 4: an unknown MUST = 0 frame of at most `UNKNOWN_LIMIT` is skipped, before or after the MANIFEST frame; an unknown MUST = 1 frame is `Unsupported(kind)`; a second MANIFEST frame, a known kind of another role, or no MANIFEST frame is `Corrupt`. Every file role reads the same way: frames to the end, each by §2.7. **Room for skipped frames:** in MANIFEST, pack and END files, the frames of unknown kinds total at most `UNKNOWN_ROOM` = 1 MiB (1,048,576 bytes, heads included), and the reader's bound for each of those files adds it. The MANIFEST file is therefore read with a bound of 72 + 16 + 64 MiB + 1 MiB = 68,157,528 bytes. Journal segments have no fixed bound. A new MUST = 0 kind for those three files must fit the room (§2.7 rule 3). **Why:** a reserved kind that an old reader turns into `Corrupt` would make row 12 a format change, which rule 4 exists to prevent. | §2.2, §2.6, L11 |
| V2 | how big a listed pack may be when it is read: §1.11 bounds it by this open's `pack_bytes`, but `pack_bytes` is an `Options` field and can differ between opens. A pack written with `pack_bytes` = 128 MiB, then read by an open with the 64 MiB default, is refused as `Corrupt` | **the reader's bound comes from the manifest, never from `Options`** | `pack_bytes` binds only the writer of a cut. A reader bounds listed pack `p-x` at 72 + nodes × (16 + NODE payload limit) + `UNKNOWN_ROOM` bytes, which is 72 + nodes × 33,686,044 + 1,048,576, from that pack's manifest entry (saturating), and reads it whole as before. A longer file is `Corrupt(Malformed)`. Open's transient peak is the account plus the largest listed pack. A pack holds at most max(`pack_bytes` of the open that wrote it, 33,686,116) bytes. If that passes the checkpoint task's root allowance, the open is `Full(c)`, never `Corrupt`. **Why the manifest:** the bound must not change meaning between opens, and the manifest already carries the node count, which is checked against the pack (§3.4 step 4). **Rejected:** capping `pack_bytes` at 64 MiB, which needs a new `Limit` case to refuse a larger value and still ties the reader to an option. | §1.11, §2.5, §3.4 |
| V3 | §2.3 gives `identity` as 16 bytes and §1 gives `StoreId { hi: u64, lo: u64 }`, but nothing maps one to the other | **the drawn bytes, in order, are the identity; `hi` and `lo` are their two halves, little-endian** | `identity[16]` (HEADER offset 16 and the check's binding) is the 16 bytes `host.random` drew, in draw order. `StoreId.hi` is bytes [0, 8) read as u64 LE, and `lo` is bytes [8, 16) read as u64 LE, so writing `hi` LE then `lo` LE gives back the same 16 bytes. 631305fc already does this (`disk_identity`, `disk_header_bytes`, `disk_check`). | §1.12, §2.1, §2.3 |
| V4 | §3.4's genesis says "write MANIFEST generation 1" but gives none of its other fields, so no independent vector could be derived | **the genesis manifest is written out, with a vector** | Generation 1, limits as V5, `next_node` 1 (node ids start at 1; 0 means an empty tree), `next_branch` 1, `next_table` 1 (§2.4: the first id is 1), `next_pack` 1, `first_live` 1 (`j-1`), `prev_first` 1 (nothing to remove), no packs, no retired packs, no pins, and one branch: id 0, name `main`, parent 0, forked_at 0, version 0, at 0, all three roots 0. **Vector (RUN, libxxhash 0.8.2, `s2check/derive_manifest_vector.py`):** with identity bytes `01 00 00 00 00 00 00 00 02 00 00 00 00 00 00 00` (`StoreId { hi: 1, lo: 2 }`), the MANIFEST file is 228 bytes: a 72-byte HEADER with check `0x4ec70261482fc196`, then a MANIFEST frame at offset 72 with a 140-byte payload and check `0x45ed8898ed9e05c8`. xxh3-64 of the whole file is `0xef84b5cbe926a0a1`. The same frame at offset 73 has a different check. | §2.6, §3.4 |
| V5 | §2.6 `limits` lists four fields but not which fan-out is written, or what a reader does with other values | **format 1 fixes all four** | `limits` is key 4096, value 1,048,576, transaction 16,777,280 (U1) and fan-out 32, the most entries a node holds. The minimum of 16 is not recorded. A reader refuses any other value as `Corrupt(Malformed)`. These are format-1 constants, not settings, and a checked frame holding other values was not written by a format-1 writer. 631305fc already does this. | §2.6 |
| V6 | §3.8 names a failed write, `sync` or `sync_dir`, and §3.4 step 3 ignores only NotFound, but neither says what a failed `rename`, `open` or non-NotFound `remove` does | **one rule for every host call** | During a cut, any failed host call poisons, as §3.8 says for a write: `open`, `write`, `sync`, `rename`, `sync_dir`, and a `remove` failing with anything but NotFound. That includes the removals after the MANIFEST rename: the manifest is durable, the task ends, and the writer stores the error. In open's leftover step (§3.4 step 3), a `remove` failing with anything but NotFound ends the open with `Io(e)` and releases LOCK. "Best effort" means that missing leftovers are not an error. 631305fc already does this (`disk_commit`, `disk_worker`, `disk_cleanup`). | §3.4, §3.5, §3.8 |

## CHANGES-v2.11 (the transaction limit is one number, in COMMIT payload bytes)

| # | Item | Verdict | What v2.11 does | Where |
|---|---|---|---|---|
| U1 | 1010's FOR SPEC: §1.1 capped "one transaction's frame" at 16 MiB (frame bytes, 16,777,216), §2.7 capped the COMMIT payload at 16 MiB + 64 (16,777,280), and L29's boundary could not be both | **one limit: a transaction is refused iff its COMMIT payload would exceed 16,777,280 bytes; §1.1's frame figure was an error in the text** | **The limit:** the COMMIT **payload** length (the frame's `len` field, the 16-byte head excluded) is at most **16,777,280** (16 MiB + 64). Both modes refuse beyond it with `Limit(Transaction)`, computed during the body (Q2). The reader refuses a COMMIT whose `len` passes the same number. So the writer and the reader use one number in one unit, the writer never produces a frame its reader refuses, and the reader's bound is never the tighter. **Why payload units:** every per-kind limit in §2.7 is a payload limit, and the reader checks `len` before it allocates. **Why this number:** it is §2.7's, the reader's bound. Keeping it changes no reader. The producer cap moves up by 80 bytes from the 16,777,216-byte frame (16,777,200 payload) the text implied and S1 built: 1010's arithmetic subtracts the 16-byte head and compares with 16,777,280. **The other number:** 16 MiB as a frame size is withdrawn. Nothing else uses it. **Manifest:** `limits.transaction` is defined as this payload limit, 16,777,280. No store has written a manifest yet (S2 has not landed), so no stored byte changes meaning: no layout change. The models check no frame sizes, so none is rerun. **L29** now states the boundary with hand-worked byte counts. | §1.1, §2.6, §2.7, §3.1, L29 |

## CHANGES-v2.10 (S2's ticket and close seam; I4's old size)

| # | Item | Verdict | What v2.10 does | Where |
|---|---|---|---|---|
| T1 | 1010's S2-only ticket/close seam: S2 had public `open` and `close` but no journal, while `run` returns `Journal(seq)`, `durable(Journal)` promises survival through acks, and L21(a) says close then reopen gives the last commit | **the end state has no mode in which a checkpoint is the durability point of a disk store's commits, so the slice is redrawn; 1010's proposal is declined** | **Why not the proposal:** a disk store always has a journal. `memory` is the only store without one, and its `durable` is `Ok` at once and promises nothing. `durable(Cut(g))` covers the tickets of `checkpoint` and of operations made by a cut, never a `run` ticket. In the end state `close` never cuts: every commit's frame is sent at `run` (§3.1 step 5), and `close` waits for the ack of the last frame sent (§1.10 step 1). So "durable(Journal) and close checkpoint the tail" exists only in S2, and S3 would delete it: a shim. Making `durable(Journal)` an error in S2 is the same shim, since the end state has no such error. **The redraw:** S2 builds `open`, `close`, `checkpoint` and `durable` in final form but does not export disk `open`. It stays a non-`pub` function of the store module, and S2's laws reach it from inside the package. S3 adds the journal and makes `open` `pub`, which is an addition, not a deletion. Through S2 the only disk Db is the laws', and the laws claim durability only at a `Checkpointed` ack. That is already how L4 is worded ("M at the last durable version"), so it reads the same in S2 and S3. If a law entry cannot reach a non-`pub` function, S2 lands on its branch and goes to main together with S3. Either way nothing is built for S2 alone. **Laws that move from S2 to S3:** L21 (a) (close then reopen equals the last commit) and L21 (c) (a drop's reopen is a crash prefix, which is L3's journal claim). **Laws that stay in S2:** L4, L5, L6, L11 and L12 (packs and manifest), L19, L21 (d) (`Held`), L25, L28, and rows R6–R8. L21's text now says why (a) holds: close waits for the last frame's ack and makes no cut. **Refusal model:** it is unchanged. In the end state a crash between a commit and the next cut loses nothing that `durable` acked, because the journal holds it, and S2 makes no public promise about that window. | §7 S2, S3; L21 |
| T2 | 1010's zero-index attribution: I4's overwrite repeats the primary lookup that `put_impl` already made, and X3 can carry the old size | **confirmed: carrying it keeps §1.11's account exact** | The old size is exact if it is `entry(k, v_old)` over the **stored** bytes the put replaces, schema number included. Nothing runs between that lookup and the insert inside one `put`, so they see the same tree. It must not be re-encoded from the decoded V: a row stored under an older schema number would give a different length. X3 drops the lookup when a table has no indexes, so the size should come back from the insert itself (omap returning the replaced entry). That covers both cases with one traversal. | §1.11, I4 |

## CHANGES-v2.9 (a cut refused with Capacity; a memory store encodes no COMMIT; R1's baseline and the twins)

| # | Item | Verdict | What v2.9 does | Where |
|---|---|---|---|---|
| Q1 | 1010's S2 task-capacity gap: a cut's allocations can return `Capacity` after part of its graph, count or pack work, and §3.8 and §4 knew only `Failed(IoError)` | **a refused cut is not a failure: it is dropped whole, acked `Refused(g, c)`, and the next cut retries; nothing is poisoned** | **Rule.** (a) A cut makes every allocation before its MANIFEST rename: the pack buffers, the new records, the count updates, the manifest buffer, the removal names and the ack value. After the rename it makes only host calls and the ack send. (b) The per-node table is a persistent `OMap`. A cut builds a new version of it (sharing structure with the old) and the task installs that version only after the MANIFEST's `sync_dir`. (c) On `Capacity` the task drops the cut's table version, buffers and Arc roots. The packs it already wrote are leftovers at seq ≥ `next_pack`: never loaded, replaced by the next cut (H2). It syncs nothing more, and it keeps no counter of its own, so the next cut starts from the manifest's `next_pack` again. (d) It acks `Refused(g, c)` and waits for the next Cut. **State after a refusal:** the durable state is exactly the old manifest plus a rotated segment, which continues the chain through `prev_end`. Every commit stays durable through the journal, and the task's table is exactly as before. **What callers see:** `checkpoint`, `durable(Cut(g))`, schema commits, `migrate`, `pin`, `unpin`, `fork`, `adopt` and `delete_branch` return `Full(c)`. An operation made by a cut publishes only when its `Checkpointed` ack arrives, so a refused one changes nothing and leaves the version unchanged (K1). `run` waiting in its settle (I4) returns `Refused(c)` with the cut's `c`. A background cut (`checkpoint_bytes`) has no caller: the next background cut is triggered after another `checkpoint_bytes` of journal, while `checkpoint` and the operations above try a cut at once. **`close`** treats a `Refused` ack as success, because no data is at risk, and returns `Ok(())` unless an I/O error is stored. `Capacity` never poisons. A `memory` store's inline count follows the same rule (b), so a refused count leaves its account as it was. **Model** (`model_v29_refusal.py`, RUN): a refusal at each of the cut's 15 allocation points, a crash under every subset right after it, then a retried cut with a crash after every step: **7,394 crash cuts, 0 failures**. Four mutants are all CAUGHT: `inplace_counts` (the table is updated in place, and a refusal leaves the partial counts), `advance_next_pack` (the task advances its own counter past a refused cut's packs), `alloc_after_publish` (a refusal after the rename drops the table of a durable manifest) and `refusal_ends_task` (the task ends as it does on an IoError). | §1.1, §1.10, §3.5, §3.6, §3.8, §3.9, §4, L28 |
| Q1a | the "64 KiB stack" figure | **wrong; replaced by the two tasks' root allowances** | `task.spawn(tasks, bytes n, …)` sets the child's root limit to n, reserved from the opener's current limit (CONC-3, MEM-19). It does not set a stack size. **The checkpoint task's limit** is `Options.checkpoint_task_bytes`. `defaults()` gives max(`pack_bytes`, MANIFEST file at its limit) + 2⌈`resident_limit` / 11⌉ + 64 KiB = 67,108,952 + 195,225,788 + 65,536 = **262,400,276 bytes** (GUESS, calibrated by R8). The parts: one pack buffer or the manifest buffer, never both, because each pack buffer is released when its pack is written. Then the table twice, the installed version and the cut's new one at worst: records are 96 B per node (§1.11), a node holds at least 16 entries, and an entry is at least 70 accounted bytes, so the records are at most `resident_limit` / 11.67. Then the walk stack and names. A program that raises `resident_limit` or `pack_bytes` raises this by the same formula. A smaller value is legal: open builds the table from the packs inside the task, which gives `Full(c)` if it does not fit, and after that a cut that does not fit is refused, never wrong. **The journal task's limit** is max(`group_bytes`, one COMMIT frame at its limit, 16 MiB + 80) + 64 KiB = **16,842,832 bytes** with the defaults. It allocates its group buffer once, at spawn, and reuses it, so its only negative message stays `Failed(e)`. A refused spawn makes `open` return `Full(c)`. | §1.1, §1.10, §4 |
| Q2 | §3.1 step 3 encoded the COMMIT frame unconditionally, and a `memory` store dropped it: 8.1k of a 99k-instruction commit (1030 item 5, X2 RUN) | **a `memory` store encodes nothing** | No law needs the encode in a memory store. The transaction size limit (`Limit(Transaction)`) is enforced during the body by arithmetic on the frame's size (§2.4 layout), so both modes refuse the same transactions by size. `recorded` is None in a memory store (G3), streams and replies live in the streams tree, the account counts only frames sent and not acked (none without a journal), and `changes` walks trees. The only difference is that a memory store cannot be `Refused` by the encode's own allocation, which is a capacity event, not a semantic one. **The sentence** (§3.1 step 3): "If the body returns `Ok` and the store has a journal, the COMMIT frame is encoded; a `memory` store encodes no frame." **The law change:** new L29 tests the size arithmetic against the encoder directly, as the review's probes did, and pins the boundary that the parity rests on. | §3.1, §1.3, L29 |
| Q3 | §6's R1 baseline was an in-place `BTreeMap`, which keeps no old version | **the ruling is applied: a row's baseline does the work the spec requires of the store on that row** | **R1's twin** is a persistent path-copying B+ tree with the spec's fan-out of 16–32 entries per node (§1.2 "Limits": fan-out is in the spec, and L5's bound and §1.11's account rest on it). Nodes are `Arc`-shared, and entries are shared bytes, so a clone is a count increment: the spec requires the old version to be kept, not entries to be copied, and a twin that deep-copies entries would measure a representation choice. Each commit keeps the base version across the put and drops it at publish. It also does what §3.1 requires on R1's path: one wall-clock read (`at`), `catch_unwind` around the body (isolate), per put the I3 catalog check, the op-list append and the I4 account update (with the replaced entry's size from the insert), per fetch the I3 check, a lookup, the `Point` record and the decode. It encodes no COMMIT (Q2). **Columns:** that twin is the target. `store_im.rs` (`im::OrdMap`, base kept, 64-wide nodes: **109,213** per commit, RUN by 1030; the store is 0.91× it today) is a reference that flatters the store. The in-place twin (**4,992**; 19.8×) is the cost with no versions: a bound, never a target. **Target:** ≤ 3.0× the fan-out-matched twin LLVM, ≤ 6× Cranelift, **GUESS** until that twin is measured (1030's ESTIMATE for it with deep-copied entries was ~25k, so 4.0× today and 2.7× after X6; with shared entries it is lower). **R2 to R9** were checked against the same rule. R2 and R9 do their row's work. R3, R4, R5, R6, R7 and R8 do not; §6 lists what each lacks. | §6 |

## CHANGES-v2.8 (RUNNER-SPEC v2.2: schema commits and the version, L16's S1 case, law G)

| # | Item | Verdict | What v2.8 does | Where |
|---|---|---|---|---|
| K1 | N12: every schema commit advances the branch version, so the runner's rule 12 (`handles(mut db)` run twice at open, `HandlesChanged` if the second run moved `version(snapshot(db))`) can compare versions | **ACCEPTED as worded; no other compare is needed** | **Rule** (§1.2): every schema commit (`create_table`, `create_index`, `drop_table`, `migrate`, and a schema number appended by `table`) advances main's version by exactly one, so an unchanged version means nothing was committed. **Holds today**, BY READING `store.hirsel` at 1f9148af: all four S1 schema commits go through `schema_publish:1615`, which does `db.head += 1` before `publish` with no path that commits and skips it; `create_table:1644`, `table`'s append at `1707` (only when no stored schema matches), `create_index:1751` and `drop_table:1837` all call it. `migrate` (S4) installs at version + 1 (§1.2). **What leaves the version unchanged, and commits nothing:** `table` finding its schema already stored, `index` (a local registration), a schema commit refused with `Full` (the check precedes the increment), and a checkpoint (§1.5). So the second `handles` call of an unchanged program leaves the version equal (RUN by 1020: p17 step 1). Any `run` commit inside `handles` also moves it, which rule 12 rightly treats as a change. **Case:** in L17, `table` appending a schema number moves the version by exactly one, and a second `table` call with the same types leaves it equal. | §1.2, L17 |
| K2 | L16's red-on-S1 case: runner-spec-1020's p17 step 4a at 62f15988, where an H made before `drop_table` + `create_table` of the same name reads the new table and gets `Ok(None)`, not `Stale` | **CONFIRMED: L16 covers exactly this probe**; it is now named as L16's case | L16 already demands `Stale(old id)` "never None or the new table's rows" from the old handle after drop and recreate, and names the S1 source's store-only check as the wrong implementation; I3 (v2.6) is the fix. v2.8 adds the probe as L16's red-on-S1 case: `runner-spec-1020/programs/p17_h_store/probe.hirsel`, outcome in `runner-spec-1020/probes.txt` ("4a old H after drop+create: Ok(None), not Stale"). 1010 runs it as one case of L16 (red at 62f15988, green after I3). | L16 |
| K3 | law G's wording, which a runner-made reply-only commit contradicted | **ACCEPTED as 1020 worded it; no model change** | Law G now reads: a frame has a call record iff it has a reply written by a run that ran an entry; the runner's reply-only `Failed(Gone)` commit for a removed entry is a run with a reply and no record. The model (`model_v24_streams.py`) generates no gone replies, so its check is unchanged and still sound for what it generates. | G1, §3.9 |

## CHANGES-v2.7 (I2 withdrawn: `lookup` records a Point per returned row)

| # | Item | Verdict | What v2.7 does | Where |
|---|---|---|---|---|
| J1 | the gap I2 closed: a `lookup` whose rows change V at the same J must re-run | **the orchestrator's alternative wins on the figures; I2 is withdrawn** | **Rule.** `lookup` records its index `Span` when it is made, and the cursor records `Point(table id ‖ Key[K])` for each row it returns, when it returns it. A put that changes V at the same J hits that Point, and a row that enters or leaves the J range hits the Span. Index entries keep empty values. A `put` whose old and new index keys are byte-equal writes no index entry (the landed source removes and re-inserts it, `put_impl:1318–1325`). **What makes it possible:** the per-row Points must reach the Tx's read log, and today's `Rows` is `send` and outlives the Tx, which is why v2.6 called this impossible. So `lookup` now returns a scoped cursor, `IndexRows[K, V]`, anchored to the Tx (OWN-20) and holding the Tx's `View[Log[Read]]`, as `Tx` itself does. RUN `programs-v27/`: `ixrows_ok`, a body that iterates it and records each Point, is ACCEPTED, and so is `ixrows_generic`, a generic scoped cursor. `ixrows_escape`, a body that returns it, is REFUSED (LANG-13). `ixrows_write`, a `put` while the cursor lives, is REFUSED (OWN-10). **That last one is the cost:** a body must finish iterating a lookup, collecting what it needs, before it writes. `range`, `scan` and `find` keep their `send` `Rows`, because a range's Span covers every value in it up front, and a Snapshot read has no read set. | §1.3, §1.10, §2.4, §1.11, L26 |
| J1a | write side: instructions per put, one put per commit, J unchanged, ~100,000 rows | **ESTIMATE**, from RUN parts | Parts (callgrind of R1, LLVM release): the data-tree insert is 50,460 Ir per call (5,046,228,305 over 100,000 calls); `put_bytes` is 53,851 per call; one R1 commit is 98,800; one `get` with decode is 3,875 (R3); an index key build is about 1,000 (ESTIMATE). Rows: **0 indexes:** 98.8k either way (+3.9k for I4's old-row get, either way). **1 index:** I2 158.6k, Points **104.7k**, landed today 209k (remove plus insert). **3 indexes:** I2 270.3k, Points **108.7k**, landed today 422k. I2 adds one index-entry insert (about 54k) per index per put. Points adds only the key compare. | — |
| J1b | read side: lookup returning 1, 100 and 10,000 rows (8-byte K) | **ESTIMATE** | **Read-set bytes:** I2, one Span of about 100 B for any count. Points, 100 B + 48 B per row (a `Bytes` handle of 24, 16 bytes of key, an 8-byte tag): **148 B, 4.9 KB, 480 KB**. **Recording:** about 400 Ir per row (one key copy and an append), against a lookup's own per-row cost of about 4k (index step, primary get, decode), so about +10%. **Publisher:** each read-set entry is one more interval-index entry, so inserting a re-run's Points costs O(r log E), and each Change still costs O(log E). I2 instead adds one Change per index per put to every commit on an indexed table, which the publisher matches whether or not anyone subscribes, and `changes` walks them. **The runner's Cover does not hold these keys:** `cover(Shape, lo, hi)` holds table ranges a query only names, not rows it read (RUNNER-SPEC §4), so the Points are new state, charged to the session budget with the rest of the read set (§3). | — |
| J1c | correctness under L26 | **both sound; Points is exact** | **Keys-only lookup:** the API has none. Every lookup returns (K, V). Were one added, it would record no Points, since K is in the index key. **A row deleted and re-put at the same J in one commit:** the index key is unchanged, and the primary key's value differs between a and b only if V changed, which then hits the Point. Same V means no Change, and the answer is the same. I2 would re-run spuriously here. **"Unique" index:** the store has no unique kind; every entry is index id ‖ J ‖ K (§2.4), so an extractor that happens to give distinct J values is the same case. **Partial iteration:** Points cover only the rows returned, which are all the body saw. The rows after them are covered for membership by the Span, and their values were never read. | L26 |
| J1d | `changes` and compaction | **Points: no change. I2: more of both.** | Under Points, an unchanged-J put writes no index node. `changes`, the incremental checkpoint (L5's k(2h+1) per written key) and copy-forward all see fewer nodes than under the landed source. Under I2, every put rewrites one entry per index, so each index adds one Change to `changes` and 2h+1 nodes to the next checkpoint. | — |
| J1e | reversibility | — | No disk byte depends on it (index values stay empty). The API change is `lookup`'s result type: `IndexRows` scoped to the Tx, with `next_indexed`. Going back to I2 would restore a `send` cursor and add the stamp before S2 writes packs. After S2, the stamp would be a format change. | — |

## CHANGES-v2.6 (S1 review: read-set encoding, stale read handles, overwrite accounting)

| # | Item | Verdict | What v2.6 does | Where |
|---|---|---|---|---|
| I1 | read-set encoding (review: `fetch_bytes:436` records the bare user key, `range_bytes:523–527` and `lookup:1436–1439` record tree keys) | **one encoding: data-tree keys, the same bytes as `Change.tree_key`** | `Read = Point(Bytes) \| Span(Bytes, Bytes)`. Every key is a data-tree key (§2.4): table id u32 BE ‖ `Key[K]`, or index id u32 BE ‖ `Key[J]` ‖ `Key[K]`. A Span is [lo, hi) in byte order: `Unbounded` low is the id's 4-byte prefix, `Unbounded` high is the prefix of id + 1, an `Included` high bound is its key ‖ 0x00, and an `Excluded` low bound is its key ‖ 0x00 (the prefix-free key format makes these exact). The u32 id is dropped from `Read`: it is the key's first four bytes, so keeping it would give two copies of one fact. **The publisher's test is a byte compare:** a Change hits a read set when its `tree_key` equals a Point or lies in a Span. | §1.3, §1.5, L15, L26 |
| I2 | `lookup` read sets missed value changes (found while settling I1) | **WITHDRAWN in v2.7 (J1).** Was: index entries carry the row's write version | `lookup` records only the index Span, and it returns primary values. An index entry's value was empty, so a `put` that changed V without changing J changed no key in that Span, and a live lookup would never re-run. Now an index entry's value is the version (u64 BE) of the commit that last wrote its row. Every `put` rewrites the row's index entries, so any write of a row is a Change inside every index Span that holds it. A same-value rewrite also shows up, as `changes` already counts it in k (H4). Cost: 8 accounted bytes per index entry (+94 B/row in §1.11's table, was +86). The alternative, recording a Point per returned row, is impossible: `Rows` outlives the Tx. Recording the table's whole span would re-run every lookup on every write to the table. Nothing is on disk yet, so this changes no stored byte. | §1.3, §2.4, §1.11, L26 |
| I3 | stale read handles (review: writes check the epoch at `writable:1276–1290`; `fetch`, `get` and `find` check identity only) | **a read checks that its handle's id is live in the catalog of the state it reads** | A table or index **id is an incarnation**. Ids come from the one `next_table` counter and are never reused. `create_table` after `drop_table` gives a new id, and **`migrate` (row 13, S4) now gives the table and each of its indexes new ids**. Every read (`fetch`, `range`, `lookup`, `get`, `scan`, `find`, and all three inside `look`) first looks up the handle's catalog key in **the catalog tree of the roots it reads** (the Tx's, or the Snapshot's). If the entry is absent, or holds another id, the read is `Stale(id)`. So against a current Tx, a handle from before a drop is `Stale`, never an empty read. Against an old Snapshot that still holds the table, the same handle reads its rows, because that snapshot's catalog lists the id, and a handle made after the drop is `Stale` there. **Shape and Index carry no epoch.** The epoch guards only index completeness, which only writes need (§1.2). A Shape or Index carries its id and catalog key. A schema number appended after a handle was resolved remains `Stale` row by row, as before. Cost: one catalog lookup per read call, not per row (R1 measures it). | §1.2, §1.5, §1.12, L16 |
| I4 | admission on overwrite (review: `put_bytes:463–465` adds on overwrite, and a memory store has no checkpoint) | **exact where nothing shares the old row; settled before any refusal** | (a) While the store has no pin and no branch but main, an overwrite subtracts the old row's entry (the put already reaches it), and a delete subtracts its own. The account is then exact after every commit, in both modes. A memory store that overwrites one key forever keeps a constant account. (b) While a pin or another branch exists, overwrites and deletes subtract nothing, and the excess is settled at cuts. **A memory store makes cuts too:** `checkpoint` and the `checkpoint_bytes` trigger run the reference count and write no file. (c) **Before refusing a commit while the account holds unsettled charges, `run` settles it first.** A memory store counts inline. A disk store waits for a checkpoint's ack. Then `run` checks again, so a refusal always rests on the exact figure. That wait comes before publish and changes nothing, so a cancel there leaves the store as it was (§3.7). (a) is S1; (b) and (c) are S4 with pins and forks. | §1.3, §1.11, §3.7, L27 |
| I5 | the review's "What S2 must change" | **noted for S2's format review** | Checked against the S2 diff when 1010 stops before landing it: the placeholders `open = 0` and `sequence = 1` (`memory_at:169`, `snapshot:181`, tickets at 307 and 1626) are replaced by the persisted identity, the open number and restored node and table counters; `schema_publish:1613–1627` goes through the manifest protocol (§1.2 "Schema commits", §3.5); `close:350–365` follows §1.10's five steps and returns the first stored error, not ignoring join results; frame binding uses real file offsets and is recomputed on copy-forward. | §1.10, §3.5, §7 |

## CHANGES-v2.5 (two S2 gaps: an oversized NODE, unnamed packs after a crash)

| # | Item | Verdict | What v2.5 does | Where |
|---|---|---|---|---|
| H1 | a NODE larger than `Options.pack_bytes` (1010's gap 1: `pack_bytes = 1`, one legal row, a checkpoint) | **ALLOWED: the NODE goes in a pack alone; the option is not refused** | A NODE goes into the open pack unless it would take the pack past `pack_bytes` and the pack already holds a NODE; in that case the pack closes first. **Bound:** a pack file is at most max(`pack_bytes`, 72 + 16 + the NODE payload limit). At the fixed limits that is max(`pack_bytes`, **33,686,116 bytes**): 72 for the HEADER file, 16 for the frame head, and 33,686,028 for the payload (§2.7). The 64 MiB default is above it, so with the defaults every pack is at most `pack_bytes`. This is the exception `group_bytes` already has (§3.3), so like things are treated alike. Refusing the option would add an Error case and a validation that hang on the limits, and would protect nothing. **Recovery** reads a pack whole as before; open's transient peak is the account plus the largest pack, so the same max. **Compaction:** a one-NODE pack's live bytes are all or nothing, so it is never copied forward; it is retired when its NODE dies. A NODE is never split across packs. | §1.1, §1.11, §2.5, §3.5 |
| H2 | unnamed packs after a crash (1010's gap 2, with a second crash during cleanup leaving a gap) | **v2.4 was safe but stated the wrong reason; the rule is now naming, not cleanup** | **Safety rule:** a MANIFEST lists only packs that its own cut wrote. A cut writes its packs at seq `next_pack`, `next_pack + 1`, … of the manifest it starts from. Each pack is written as `p-<seq>.tmp` with `Write` (which truncates), synced, and renamed onto `p-<seq>`, replacing any file of that name (W2 rename over an existing name, landed 1070c6a3). So a stale `p-x` left by an unpublished cut always has x ≥ `next_pack`, and it is replaced before any manifest lists x. Open loads only listed packs. **Cleanup is best effort and only frees space:** after the O1 directory sync, open removes `MANIFEST.tmp`, then `p-x` and `p-x.tmp` for x = `next_pack`, `next_pack + 1`, … until both names are NotFound, then the retired packs. Cleanup itself issues no directory sync. A pack it misses because of a gap is replaced by a later cut, or removed by a later open once a gap closes. **Space bound:** the leftover p-files are at most the packs of the largest unpublished cut. **No directory listing is needed**, and none is asked for. | §2.6, §3.4, §3.5, §3.6, L25 |
| H3 | the model | **new, clean** | `model_v25_packs.py`. Its file system keeps **any subset** of the pending directory operations at a crash, which is weaker than the W2 provider's all-or-nothing restart and is the persistence 1010 named. Unsynced data is lost. The scenario: cut A writes three packs, one an oversized NODE alone, with a crash after every step; recovery's cleanup runs with a second crash after every one of its steps; then cut B (one pack) and cut C (three packs) run, with every subset of their unsynced removals. Checks: P1, each listed pack is the content its cut wrote; P2, nothing unlisted is loaded; P3, unlisted, unretired p-files only at or above `next_pack`; P4, the pack size bound. RUN: **155 states, 7,556 crash cuts, 0 failures.** Mutants, all CAUGHT: `adopt_by_probe` (open loads what it finds), `skip_existing` (a cut lists a stale `p-x` without rewriting it; this is 1010's collision), `no_dir_sync` (no `sync_dir` between the pack renames and the MANIFEST), `start_below` (a cut starts below `next_pack`), and `close_after` (the pack closes after the NODE that passes `pack_bytes`). | §3.9 |
| H4 | `changes` bound (1010, accepted by the orchestrator) | **corrected** | Equal node ids are skipped, so a key rewritten with the same value is still visited. The bound is O(k log n) node visits for k keys written between the two snapshots, same-value rewrites included; w ≤ k changes are returned. | §1.5, F6 |
| H5 | format | **no byte changes** | MANIFEST's `next_pack` keeps its field and meaning; only its comment changes. | §2.6 |

## CHANGES-v2.4 (N11, `info` in `look`, slices, ENDSTATE)

| # | Item | Verdict | What v2.4 does | Where |
|---|---|---|---|---|
| G1 | N11: keep a call's record with its reply when the body returns a typed error | **ACCEPTED** | v2.3 was wrong here. Under v2.3 a typed error's reply frame had no record, so `tapes check` read every typed error as `NotRecorded`, while a fault was re-executable through the runner's reply-only commit. That is backwards: a typed error is a decision the body made, and it is the more useful one to re-check. On `Aborted`, the call section is now kept in the reply's streams-only frame. With no `reply` call, the record is discarded with everything else. §2.4 already allows that frame, since a call section needs only its stream's op in the same frame, so the format does not change. Each outcome kind under this rule, as a tape check sees it, is in the table below. Model: law G added (a frame has a call record iff it has a reply written by a run that ran an entry; the runner's reply-only `Failed(Gone)` commit for a removed entry is a run with a reply and no record, K3). RUN: **1,851 states, 2,396 cuts, 0 failures**. Mutants `abort_drops_call` (v2.3's rule) and `call_without_reply` are CAUGHT, and so are the four from v2.3. | §1.3, §2.4, L24, §3.9 |
| G2 | `info` inside `look` (store-build-1010's question; the orchestrator's interim ruling: a fault) | **REFUSED AT CHECK TIME instead: `info(mut tx: Tx)`** | A program that cannot mean anything should not type-check; a run-time `Failed` would leave a check to keep and a law case to test. RUN `programs-v24/` at 1f9148af: `info(mut tx)` inside a `look` body is REFUSED, OWN-5 "`tx` is not writable" (`info_look_mut`). It is ACCEPTED in a `run` body (`info_run_mut`) and in a helper that takes `mut Tx` (`info_helper_mut`). The old `info(tx: Tx)` is ACCEPTED inside `look` (`info_look_view`), which is the hole. Nothing a view-Tx body needs is lost: only `look` hands out a view Tx, and it has no CallInfo to give. The one cost is that a read-only helper that wants CallInfo must take `mut Tx`. L22 gains no case, because the refusal is the checker's. | §1.3, §1.5, §1.7 |
| G3 | slices (store-build-1010's reading) | **1010 is right; §7 stands** | S1.1 carries only §7's S1 items: Shape reads, `Aborted(Undone)`, `look` and `changes`. `record`, `reply`, S7 and the call section stay in S5 and S3, and **L24 is an S3 law**. A `memory` store has no journal, so `recorded` there is always None and a tape check reports `Unknown(StateGone)`. No record is kept in memory, because nothing would bound it. | §1.3, §7 |
| G4 | ENDSTATE `storage` passages that this spec makes wrong | **written** | `STORE-ENDSTATE-TEXT.md`, in the runner's form. Later items (rows 11 and 12, authorities, levels, followers) are kept and marked later. | — |

**What a tape check reports, by outcome (G1).** Verdict names are RUNNER-SPEC §7's.

| Outcome of the position's run | What the store committed | Call record | `tapes check` |
|---|---|---|---|
| `Committed` | data, reply and call section, in one frame | yes | `Same`, or `Different(Reply)` / `Different(Writes)` |
| `Aborted(u)` with a `reply` call | reply and call section, one streams-only frame; no data | yes (G1) | `Same` when the re-run also returns the error and the same reply bytes, with no writes; otherwise `Different(Reply)` |
| `Aborted(u)` without a `reply` call | nothing | none | nothing to check: no version exists, and the position re-runs as `Abandoned` |
| `Failed(causes)` | nothing from the body; the runner then commits a reply-only `Failed` with its own `record` | yes, in the runner's commit | `Same` when the re-run faults the same way, else `Different(Reply)` |
| `Refused(c)` | nothing | none | nothing to check: the position re-runs |
| a gone entry's reply-only `Failed(Gone)` | reply only | none | `Unknown(NoEntry)` |
| a record whose version a cut taken by this Db covers (W2), or in a `memory` store | — | `recorded` is None | `Unknown(StateGone)` |

## CHANGES-v2.3 (reconciliation with RUNNER-SPEC v2, runner-spec-1020)

The runner's needs are followups.md "runner-spec-1020 -> store-spec-1001" N1–N6 (N7–N10 are not the store's). RUNNER-SPEC v2 §§1–4, 7 and 9 govern.

| # | Item | Verdict | What v2.3 does | Where |
|---|---|---|---|---|
| F1 | N1, rule S7: `admit(latest + 1)` while latest is admitted and unreplied | **ACCEPTED as written** | That admit is `Rejected(latest)` and commits nothing. A stream passes a position only after the position's reply is in the store, so `Rejected(l)` with `p < l` proves that p has a reply. RUNNER H4 and its `Superseded` answer rely on that proof. v2.2 committed `Admitted(latest + 1)` there; that is the hole the runner's mutant `store_v22_admit` shows (RUN at 1e866455: CAUGHT, "Superseded for p0 never replied"). With D6, an earlier open's unreplied latest p makes p+1 wait until p is re-sent and re-run (`Abandoned`). (v2.24, AH2: below latest is now `HistoryUnavailable`; `Rejected(l)` always has `p` > l.) | §1.8, L13 |
| F2 | N2, the runner's D3: a typed error in one commit | **ACCEPTED**, with what it is worth stated | `Aborted(A)` becomes `Aborted(Undone[A])`, where `Undone[A] { value: A, ticket: Option[Ticket] }`. On `Error(a)` the body's data writes are discarded. Its `reply` writes are committed as **one COMMIT frame with only a streams section**, and `ticket` names that commit. With no `reply` call, nothing is committed and `ticket` is None. That frame is already valid under §2.4: the section count was 1–2, so **no format change**. `Failed` and `Refused` keep nothing: a faulting body's ops are not trusted, and the runner commits a reply-only `Failed` itself. If the reply-only commit would pass `resident_limit`, the outcome is `Refused(c)` and nothing is kept. What it is worth: the runner's own control `no_d3_two_commits` (abort, then a separate reply-only commit) also has 0 failures (RUN: 4,441 states, 77 cuts). So D3 saves one commit per typed error and one round trip in the head; correctness does not depend on it. | §1.3, §1.8, L13 |
| F3 | N3: no wait for the admit ticket | **ACCEPTED (wording only)** | The journal is prefix-ordered (§3.3, and every model since v2 cuts only prefixes). A durable reply therefore implies a durable admit. `durable(admit ticket)` is needed only before an effect outside the store. | §1.8, L13 |
| F4 | `Superseded` and `Failed(Gone)` | **no store change: the smaller thing** | Superseded is a runner Outcome. Its proof is F1's `Rejected(l)` with `p < l`, so the store adds no `Admission` case. A gone position is answered by a `run` whose body only calls `reply(Failed(Gone))` and returns `Ok`. That is `Committed`, with a frame that has only a streams section. A re-send gets it back as `Duplicate`. The stream state holds no entry key, so nothing else is needed. (v2.24, AH2: the store no longer answers `Rejected(l)` with `p` < l, so it proves no `Superseded`.) | §1.8 |
| F5 | N4, `look` | **ACCEPTED with two changes** | (1) `look` runs the body in `isolate`, like `run`, so a faulting query cannot take the publisher down. It returns `Looked[R] = Seen(Seen[R]) \| Failed(Causes) \| Refused(Capacity)`, alike with `Outcome`. The runner's `Result[Looked[R], Error]` had no place for a fault. (2) **`fetch` and `range` take a `Shape`, not a `Table`.** `look` runs on the publisher task, and a `Table` is `send` but not shareable (it holds its index list). A query that must run under both `run` and `look` therefore reads through a `Shape` and an `Index`, both `shareable + send` (D1). Reads never needed a Table's index list or epoch; `put` and `delete` keep `Table`. RUN `programs-v23/`: a `scoped[view] fn(Tx) -> R` body over a view Tx is ACCEPTED (`look_view`). The same body calling `put(mut tx, …)` is REFUSED: OWN-5, "`tx` is not writable" (`look_put`). So "look cannot write" is the checker's rule; the store adds no run-time check. | §1.3, §1.5, §5 L22, §7 S1 |
| F6 | N5, `changes` | **ACCEPTED with three changes** | (1) Skips shared subtrees by **node id**, not Arc identity: within one open a node id names one immutable node, including nodes loaded from packs (§1.9), and `std.omap.node_id` is the landed view. (2) Returns `List[Change]` rather than `Rows`. `Rows` is one path with a `Key` and a `Codec`; a merge of two trees is neither. The runner asked for `Rows` but defined only `Change`. The list is charged to the caller's isolate, O(w). (3) `Foreign` only when the two Snapshots belong to different stores or **opens**; node ids are reissued after a reopen (§1.9). Different branches of one open are allowed: a fork shares nodes, so a diff across a fork costs O(w log n) too. Cost: O(k log n) node visits for k keys written between them, with w ≤ k returned (corrected in H4). The data tree only: a streams-only commit (F2, F4) has no change. | §1.5, §5 L23, §7 S1 |
| F7 | N6, a CALL section in COMMIT | **ACCEPTED, smaller** | **Format change, made before S2 lands:** COMMIT's section count becomes 1–3. Section tag 2 is `call`, at most one per COMMIT, and it has no ops count and no op tag, because it is not a tree. It carries: key (u32 length ‖ bytes), args (u32 length ‖ codec bytes) and stream (u32 length ‖ name). It drops the runner's `p` and `info`. `p` is the position in that stream's streams op in the same frame, which `record` requires (`Missing("reply")` otherwise). `CallInfo` is already derived from the frame's branch, version and `at` (§1.7). One fact, one place. **No new frame kind**: a separate kind could be torn away from its COMMIT; a section shares the frame's check. API: `CallRecord { key, args, stream }`; `record(mut tx, take c)`, where a second call is `Exists("call")` (no new Error case); `recorded(db, b, version) -> Option[CallRecord]`, served from live journal segments only. A commit whose segment a checkpoint has compacted reads as None, which RUNNER §7 reports as `Unknown(StateGone)`. Long retention is the tape's job, not the journal's. **Bytes:** the section is 1 + (4 + key) + (4 + args) + (4 + stream). With a 32-byte entry key (16 identity, 8 args_fp, 8 result_fp), 16 args bytes and an 8-byte stream name, that is **69 bytes**. A transact call that writes one row and replies 8 bytes is **205 bytes** (§2.4). The runner's form adds 4 (ops) + 1 (tag) + 8 (p) + 4 + CallInfo bytes (at least 28) on each recorded commit. Replay skips the section by its lengths and ignores it for state. **The model**: unchanged, because a section changes no frame boundary or cut. | §1.3, §2.4, §2.7, §5 L24, §7 S3, S5 |
| F8 | `follow` | **unchanged** | The runner releases only after `durable` (its G1), so `follow` need not be durable (RUNNER F8b). | — |
| F9 | the orchestrator's §2.4 initial values | **AGREED** | No conflict: index ids from `next_table` match §2.6's one counter and the catalog key prefix. Epoch 1 lets 0 mean "no Table" in `Stale`. Schema number 0 matches D1's u16 tag. | §2.4 |
| F10 | "the root's 32 bytes" | **already folded, re-verified** | §1.10's `Latest` row says 32 bytes. RUN at 1e866455: `crates/hirsel-core-layout/src/charge.rs:21` `LATEST_ROOT: u64 = 32`, asserted equal to `size_of::<Latest>()` in `hirsel-core-rt/src/latest.rs:21`. | §1.10 |
| F11 | the model (rules on streams and commits changed) | **re-run, clean** | `model_v23_streams.py` checks the store side alone: one stream, three positions, prefix-ordered crash cuts, and two crashes. **1,851 states, 2,396 crash cuts, 0 failures.** Its four mutants are all CAUGHT: `no_s7`, `abort_keeps_data`, `abort_drops_reply` and `abort_no_ticket`. The runner's calls model at this base gives 4,025 states, 63 cuts, 0 failures, and 6 of 6 mutants caught (5 plus `superseded_unchecked_store`). The frame model (`model_v22.py`: 122 cuts, 13 damage cases) is unchanged: F2's frame was already legal, and F7's section moves no boundary. | §3.9 |

## CHANGES-v2.2 (checksum ruling)

| # | Item | Verdict | What v2.2 does | Where |
|---|---|---|---|---|
| E1 | R9 measured by store-build-1010 on main: CRC-32C 266,801,271 instructions and 74.3 ms; xxh3-64 92,247,236 and 5.6 ms (10,000 × 4 KiB, LLVM release, both matching Rust). Orchestrator's ruling: xxh3-64 everywhere. | **ACCEPTED; my §2.1 guess was wrong** | xxh3-64 costs 2.25 instructions per byte against CRC-32C's 6.51, which is 2.9× fewer instructions and 13× less time. `int.mul_wide` exists, which removes the reason I gave. Every check in the format is now a u64 xxh3-64 (seed 0, default secret). It is the same checksum the compiler store and the tape use. The frame head is 16 bytes: kind u16, flags u16, len u32, check u64. | §2.1, §6 R9, §9 |
| E2 | what the check covers, and its binding | **bound to the store, the file and the offset** | `check = xxh3_64(identity[16] ‖ role u8 ‖ seq u64 ‖ offset u64 ‖ head[0,8) ‖ payload)`. That is the tape's rule (`xxh3(tape_id ‖ head ‖ payload)`) plus the frame's place. Role and seq are the file's, and offset is the frame's first byte in the file. A HEADER frame binds to the identity in its own payload; every other frame binds to its file's HEADER identity. | §2.1 |
| E3 | what the binding gains | — | **L12** gains a case: a whole, valid frame of this store at the wrong place (a misdirected write, a stale or duplicated sector, a NODE frame in the wrong pack) is `Corrupt(Checksum)`. Before, it decoded. For NODE frames this matters, because a duplicated node id is not otherwise refused (§2.8). **L19** gains frame granularity: a single frame spliced in from another store is refused at that frame, where v2 caught only whole files, by their HEADER. **L20 gains nothing**: its handles are in-memory values, not bytes. Cost: copy-forward recomputes the check of each node it moves (2.25 instructions per byte, measured). Journal frames, MANIFEST and END files are never moved. | §2.1, §3.5, §5 |
| E4 | detection is now probabilistic | **stated** | CRC-32C detects every error burst of 32 bits or less. A 64-bit hash misses any given damage with probability about 2⁻⁶⁴, single-byte flips included. L12's "gives `Corrupt`" now means "except with probability about 2⁻⁶⁴", as REPLAY-SPEC already states for the tape. | §2.8, L12 |
| E5 | byte figures | **recomputed** | A one-key COMMIT is **97 bytes** (was 93). A HEADER-only file is 72 bytes (was 68), so a segment's first `d` is 72. An END file is **96 bytes** (was 88). Each NODE frame on disk grows by 4 bytes. The resident row cost stays **118 B**: frame heads are not held in memory. | §2.3, §2.4, §3.2, §3.4, R4 |
| E6 | the model | **re-run, clean** | `model_v2.py` used a 12-byte head with CRC-32C. Its damage cases depend only on whether a check fails, not on its width. `model_v22.py` uses a 16-byte head with a 64-bit check (blake2b-8 standing in for xxh3, since Python here has no xxh3): **122 cuts, FAILED=0; 13 damage cases, FAILED=0** (RUN). It does not model the E2 binding, because no cut moves a frame; L12 and L19 test the binding on the real code. | §3.9 |
| E7 | library | **`std.check`** | `core/std/check.hirsel` holds one xxh3-64 implementation for all of std: the store, the tape, and anything later. Interface: a one-shot `xxh3(b: Bytes, lo: u64, hi: u64) -> u64`, and a streaming `Xxh3` state (`start`, `add(mut s, b, lo, hi)`, `finish`). A frame's check covers a prefix that is not in the file plus two separate ranges of the read buffer, so it needs the streaming form, and the ranges avoid a copy. `std.check.crc32c` is deleted. | §2.1, §7 S1 |

## CHANGES-v2.1 (reconciliation with what landed after v2)

| # | Item | Verdict | What v2.1 does | Where |
|---|---|---|---|---|
| D1 | **Shape and Index hold a resolved reader; a built `Reader[T]` owns lists, so they cannot be plain** (store-build-1010's stop) | **RULED: (a), with one change the question missed.** | v2 is wrong in a way the question did not name: `table` resolved an older stored schema but `put` then wrote the reader's schema, so a table would hold rows of two schemas under a catalog that records one. v2.1 tags each row value with its **schema number** (u16 LE, 2 bytes, before the codec bytes). The catalog keeps every schema number a table has used (append-only, never reused), and `table` adds the program's schema as a new number when it is not already there. Reads dispatch on the tag. `Shape`, `Index` and `Table` hold `Arc[Readers[V]]`: the plain `Codec[V]`, the current number, and one `Option[codec.Reader[V]]` per older number, each resolved **once** when the handle is made. So they are `shareable + send`, not plain. Old pins and branches read with today's Shape, because the numbers are append-only. `migrate` retires every older number. Why not (b): `codec` has no plain resolved form, and a plain handle cannot carry a cache (CONC-12). (b) would recompile the plan for each row, or test a fingerprint for each row and still need the plan somewhere. Why not eager rewriting at `table`: that is an O(state) stall per schema change, which is the cost v2 removed from checkpoints. | §1.2, §1.5, §2.4, §1.11, L17, L20 |
| D1a | what plain bought, and what (a) costs | — | The foreign check (L20) compares `StoreId` fields, which (a) keeps unchanged. `KeyScope` holds only ids and bounds, so it stays plain (computed). Sending a Shape to a reader task works: `shape_arc`, once `Reader` is declared `shareable + send` (followup to 1008). Nothing in the spec compares handles for equality. **Copy of a Shape** under (a): one Arc share and, later, one release. Each is a CONC-10 event; together GUESS ≤ 40 instructions. A plain Shape would be a copy of about 8 words. **Decode, GUESS:** a row in the current schema costs one 2-byte tag compare plus `codec.decode`, about decode + 5 instructions under (a). A row in an older schema costs `codec.read`: a transcode into chunks, then decode, about 2–3× decode. Under (b) every row costs a fingerprint compare and, on a mismatch, a `resolve` (schema scan, plan compile, a memo Map), about 20–100× a small row's decode. | §1.2 |
| D2 | `pub type Authority: plain = Directory()` (orchestrator's correction) | **FIXED again, RUN** | The checker at 41493a1a refuses it: `OWN-4 plain, packed and streamable are computed, never declared`. `pub type Authority = Directory()` is ACCEPTED (`programs-v21/authority`, copied twice by value). | §1.1 |
| D3 | **every `: plain` in v2** (Options, StoreId, Ticket, Mark, CallId, CallInfo, Branch, Shape, Index, KeyScope; §7's Codec and Key) | **v2 WRONG; fixed** | The same OWN-4 refusal (RUN) applies to all of them. Declarations drop `: plain`, and plainness is computed from the fields. Snapshot is declared `shareable + send` (D5). | §1 |
| D4 | §7 S0 sketch against `core/std/codec.hirsel` at 227d3382 | **superseded by the code** | S0 is the codec lane's (std-codec-1008); it is not a store slice. The store uses: `Codec[T]` (plain, computed: RUN `shape_plain`); `encode`; `decode` and `take_with` (offset form); `schema`; `fingerprint`; `resolve -> Reader[T]`; `read`; and `Key[T]` with `encode_key`, `decode_key` and `key_schema` (order-preserving and prefix-free, as §2.4 needs). The store compares a stored schema with `fingerprint` and then a bytes compare, so a 64-bit collision cannot merge two schemas. Missing, and filed to 1008 before its land: (1) `Reader[T]` declared `shareable + send` (RUN: `shape_arc` REFUSED, LANG-12 `std@1/codec/Reader[u64]` is not shareable + send); (2) `read` from an offset, so the 2-byte tag needs no copy; `decode` already has the offset form `take_with` plus a length check. | §7 |
| D5 | `Latest[Snapshot]` (CONC-13) | **ACCEPTED, opt-in** | `follow(mut db, b) -> Result[Latest[Snapshot], Error]` gives a handle whose root the writer republishes at every publish point of `b`. That covers `run`, `adopt`, schema commits and checkpoints. Readers call `latest.read`. A reader sees a version no older than one it has already seen, because publishes are in commit order on one task (CONC-10). It may see a version that is not durable. Cost per commit to a followed branch: one `arc.new` of the Snapshot record (about 72 bytes), three root shares and one publish (GUESS 150–300 instructions; about 5% of R1's GUESS budget). A branch that is not followed costs nothing. v2's "the store adds no publication mechanism" is replaced. A Snapshot is `shareable + send`. Nothing in §3.7 changes: `publish` is not a cancellation point. | §1.5, L2 |
| D6 | REPLAY-SPEC rule S3 (retired in REPLAY-SPEC v3; this row is now the store's own rule, see AO2): `Abandoned(u64)` → `Abandoned(Ticket)` | **ACCEPTED, and a hole in v2 filled** | When `p` was admitted by an earlier open and never replied, `admit` re-commits `Admitted(p)` under this open and returns `Abandoned(t)` with that commit's ticket. That ticket is needed because an earlier open's ticket is `Foreign` (L20). v2 gave no answer when `p` was admitted earlier in **this** open and has no reply (the run aborted or failed). v2.1 returns `Abandoned(t)` with the existing ticket and commits nothing. One supervisor per stream (REPLAY §7.3) makes re-running safe. | §1.8, L13 |
| D7 | §2.3 identity: the clock form | **dead; deleted** | The identity is `host.random(r, mut b)` (1951a976) with 16 bytes, drawn once at genesis; it neither waits nor fails. | §1.1, §2.3 |
| D8 | §7 needs from 998 and 907 | **met on main** | `std.omap` (6b87c486) gives `insert(mut ids, m, k, v)`, `remove`, `leaf`, `branch`, `from_root`, `node_id/level/count/len/key/value/child`, and a `send` cursor, all as S1 and S2 need. `try_receive` (1070c6a3, tasks.md CONC-5) has exactly §3.3's Empty/Closed rule. The crash provider (1070c6a3) has append, rename-over and remove, so C14 is closed. `host.random` is landed. | §7 |
| D9 | `Bound` names | **aligned** | The store's `Bound[K] = Unbounded \| Included(K) \| Excluded(K)` now uses std.omap's names (like things alike). It stays a separate type, because a table's K need not be `ordered`: order comes from `Key[K]`'s bytes. | §1.3 |
| D10 | sizes after the tag | **recomputed** | A one-key COMMIT is **93 bytes** (was 91). A row is `62 + r8(12) + r8(34)` = **118 B**: **112.5 MiB at 1 M rows, 1.10 GiB at 10 M**; with one index, 204 B/row (194.5 MiB, 1.90 GiB). The 1 GiB default admits about 9.1 M rows. | §1.11, §2.4, R4, R8 |
| D11 | `std.tally` (landed) | **not used** | The resident account is one u64 that only the writer's task touches, so it has no cross-task counter to replace. | — |

## CHANGES-v2

Each review item and each orchestrator item has a verdict: ACCEPTED (with the fix and where it is), or REFUTED (with the evidence).

| # | Item (review row / orchestrator item) | Verdict | What v2 does | Where |
|---|---|---|---|---|
| C1 | Q1 / item 4: a byte flip in a fully synced last journal frame reads as a torn tail, but L12 says Corrupt | **ACCEPTED** | Each segment gets a durable end file, `j-<n>.end`. It holds HEADER (role 4) and one END frame (kind 0x0012) recording `d`, the synced byte end. The journal task rewrites it after a sync when the queue is idle, and at least once every 64 groups. A bad frame starting below `d` is `Corrupt`; at or above `d` it is the torn tail. A damaged END file only loses detection. I rejected the other rule, "a complete bad frame is corruption", with evidence: an ordinary power cut can leave a full-length frame with zeroed sectors (damage case D3), and that rule would refuse to open such a store. | §2.4, §3.2, §3.4, §3.9 |
| C2 | item 4: rerun the model on the v2 rules | **ACCEPTED** | I copied the reviewer's model and gave it an oracle: the reopened state must equal one of the states from the last acked one onward. I changed it to the v2 rules. **122 cuts** (a superset of all 69 of the reviewer's) **FAILED=0**; **13 damage cases FAILED=0**. Four seeded mutants are each caught. | §3.9 |
| C3 | Q2 / item 5: unknown MUST=0 frames have no payload bound | **ACCEPTED** | `UNKNOWN_LIMIT = 64 KiB`. A larger unknown frame below `d`, or anywhere in a pack or the manifest, is `Corrupt`; in the torn region it is torn. A new kind that needs more must be MUST=1. | §2.7 |
| C4 | Q2 / ruling 3: identity from the clock is not unique | **ACCEPTED** | The identity's final source is the `Random` host operation (907). `open` takes the `Random` capability now (the type exists, core.hirsel:77), so the signature never changes. Until 907 lands the operation, the body uses the clock. The 16-byte field is unchanged. | §1.1, §2.3 |
| C5 | Q3 / item 6: `Error` has no `Cancelled` case | **REFUTED, with the per-wait contract added** | A cancellation is never a value a store call can return. CONC-7 (tasks.md:111-122) makes observing it "a failure with cause `Cancelled` … which runs the task's cleanup". FAIL-16 (failure.md:260-265) says a failure whose primary cause is `Cancelled` "is not caught" by `isolate`. So no caller can see a store call return after a cancellation, and an `Error.Cancelled` case would be dead code that a reader must still handle. v2 instead states, for every waiting call, what the cancellation leaves: the task ends, the `Db` is cleaned (§1.10), and the store equals a named crash cut. v1's "open ends `Cancelled`" was wrong and is gone. | §3.7 |
| C6 | Q3 / item 6: a poisoned `Db` cannot be closed | **ACCEPTED** | `close(take db)` never refuses. It drops what is queued, closes the channels, joins both tasks, releases LOCK, and returns `Error(Poisoned(e))` carrying the stored first error. A close that fails part way still finishes the cleanup. | §1.1, §1.10, L21 |
| C7 | Q5 / items 1 and 7: try_receive group membership; the try_send fallback | **ACCEPTED** | The writer sends each commit's frame on one FIFO channel (capacity 128), with a waiting `send`. The journal task drains: one `receive`, then `try_receive` until the queue is empty, 64 frames or 4 MiB. Then one write, one sync, one ack. Close rule: `Closed` arrives only after the queue is empty, and the journal then writes, syncs, acks and ends. Which commits share a sync affects only I/O: frames are written in commit order, no group boundary is written, and replay reads frame order alone. The one-slot fallback and `flush` are deleted. | §3.1, §3.3 |
| C8 | Q4: rerun `codec_bound` on current main | **DONE** | It is still REFUSED on 78e07cb1 (LANG-5 unknown bound). The bound is specified (beff7ef2) but 913 has not landed it. This changes nothing: the API takes dictionary values (`tx_body` ACCEPTED). All other probes have the v1 outcomes. | §8, `probes-v2.txt` |
| C9 | Q6 / item 8: no law checks CallInfo, identity, foreign handles or cleanup (`seed = 0` passes all 17) | **ACCEPTED** | Four laws added: **L18** CallInfo (seed against a known splitmix64 vector, `at` non-decreasing under a backward clock, CallInfo equal after reopen); **L19** identity; **L20** foreign handles; **L21** close, drop and poison cleanup. Every handle now carries the store identity, and Snapshot and Ticket also carry the open number, so `Foreign` is decidable. | §1.12, §5 |
| C10 | Q6: the L5 bound is not a worst-case bound (998's note: occupancy 16–32) | **ACCEPTED** | L5 now uses 998's occupancy: a commit makes at most 2h+1 nodes, with h ≤ 1 + ⌈log16 ⌈n/16⌉⌉. | §5 |
| C11 | Q6 / item 9: measurements without fixtures or independent baselines | **ACCEPTED** | Each of R1–R9 has a workload, a build (LLVM release and Cranelift, core/MEASURE.md protocol), a host (turbo2, x86-64), and a baseline that is not the store: a W6 Rust twin, or the old store's recorded figure. Every target that is not a plan target is marked GUESS, and the first measurement replaces it. R3 no longer compares the store with itself. | §6 |
| C12 | Q7 / ruling 2: all state in memory has no admission bound | **ACCEPTED** | New `Options.resident_limit` (default 1 GiB, GUESS). An accounted resident figure is exact at each checkpoint and conservative between them. A commit beyond the limit is `Refused(Capacity)`; an open or schema commit beyond it is `Full(Capacity)`. Resident cost by node arithmetic: **104 MiB at 1 M rows and 1.02 GiB at 10 M rows** (12-byte keys, 32-byte values, no index); 186 MiB and 1.82 GiB with one index. R8 calibrates the constants. Paging needs no change to node bytes. | §1.11, §6 R8 |
| C13 | Q8 / item 10: five or six slices; codec dependency unnamed; S3 not final | **ACCEPTED** | **Six slices, S0–S5.** S0 is the codec dictionaries and schema (`Codec[T]`, `Key[T]`, `schema`, `resolve`), with the exact surface S1 uses. It needs nothing and starts now. S1 needs only 998's map and S0's signatures, so **S1 starts the day 998's map lands**. A table gives what each slice needs from 998, 907, 880 and the derive. S3 is final: Q1 is ruled and C1 fixes the last segment. v1's S6 (P1's switch to `codec.of`) changes no store code, so it is the derive lane's step, not a store slice. | §7 |
| C14 | review §2: W2 as landed (78e07cb1) cannot express the store's barriers | **NEW, from my rerun** | `test_provider.rs::crash` has no remove, no append, and `Rename` asserts that the target is absent. The store needs rename over an existing name (MANIFEST), remove, append, and Write that truncates. Sent to 907 as S2/S3 needs. | §7, followups |
| C15 | API consequences of C6, C7, C9 and C12 | — | `flush` is deleted. `run` is `waits`: its only wait is the handoff after publish. `memory` takes `Options`. `main_branch(db)` takes the Db. `Ticket`, `Branch` and `CallId` carry a `StoreId`. | §1 |

---

## 0. Summary

**Scope** (Sam, 2026-11-15 23:05):
- Rows 1–10 of M4-PLAN §2.3 are built now:
  1. typed tables, schemas and migrate;
  2. read sets;
  3. tickets and group commit;
  4. snapshots read on any task;
  5. CallInfo;
  6. call streams;
  7. KeyScope narrow (deleted in v2.30 AN1);
  8. pins;
  9. forks;
  10. incremental checkpoints.
- Rows 11 (claims and epoch fencing) and 12 (object-backed authorities and durability levels) get frame kinds, header fields and API slots now. §2.9 says what each waits for.
- Rows 13 and 14 are never built:
  - a rename is a migrate;
  - internal state lives in its own trees and frame kinds, never in a reserved key space.
- There is one format. Its version is 1 and stays 1, and there is no migration from the old store.
- Checkpoints are incremental only.
- Rulings built in (orchestrator, 2026-11-15):
  - `channel.try_receive` is in core (907 builds it);
  - all state stays in memory for M4, under an admission bound;
  - the identity comes from `Random` (907).

**Shape in one paragraph.**
- A store is one in-memory persistent B-tree of bytes per branch, in three trees: `data`, `streams` and `catalog`. The tree is 998's ordered map with `K = V = Bytes`, and its nodes are shared through `Arc`.
- A transaction body runs inside `isolate` on a path-copied working tree. Commit installs the new roots, so the commit is visible at once, and sends one `COMMIT` frame to a journal task.
- The journal task drains whatever is queued, writes it, syncs it and acknowledges it.
- A checkpoint task writes, to a new immutable pack, only the nodes no earlier pack holds. It then replaces `MANIFEST`, which names the packs, every branch and pin root, and the first live journal segment.
- Opening a store reads the manifest, the packs and the journal suffix, then starts a new journal segment. Nothing is ever appended to a file an earlier process wrote.

**The format in one diagram.**
```
<store dir>/
  LOCK                   empty; host.try_lock held by the one writer for the Db's life
  MANIFEST               HEADER(role 1) MANIFEST            replaced only by MANIFEST.tmp + sync + rename + sync_dir
  p-<seq:16 hex>         HEADER(role 3) NODE NODE ...       immutable pack; written .tmp, synced, renamed
  j-<seq:16 hex>         HEADER(role 2) COMMIT COMMIT ...   journal segment; append-only, one writer process
  j-<seq:16 hex>.end     HEADER(role 4) END(d)              the segment's synced end; rewritten whole by the journal task

every frame:  kind u16 | flags u16 (bit 0 = MUST) | len u32 | check u64 | payload[len]      (16-byte head, LE)
              check = xxh3_64(identity ‖ role ‖ seq ‖ offset ‖ head[0,8) ‖ payload)       (§2.1)

MANIFEST ── packs ─────────► p-*   ── NODE(id, level, entries) ── children by node id
         ── branches, pins ► (data, streams, catalog) root node ids + version
         ── first_live ────► j-<first_live>, j-<first_live+1>, ...   replayed in order on open
```

**Build order** (§7). There are six slices.

| Slice | Delivers | Needs | Starts |
|---|---|---|---|
| S0 | `std.codec`: `Codec[T]`, `Key[T]`, schema bytes, `resolve`, hand-written constructors | SEM-10 as landed | now |
| S1 | the in-memory store: `memory`, tables, indexes, `run`, read sets, cursors, CallInfo, snapshots, resident account | 998's map; S0's signatures | the day 998's map lands |
| S2 | on disk: packs, manifest, incremental checkpoint, GC, open and recovery, schema commits | S1; 998's constructors; 907 W1 (landed), W2 (extended), Random | after S1 |
| S3 | the journal: segments, END files, the journal task, tickets, group commit, poison, cancellation | S2; 907 `try_receive`, W2, W4 | after S2 |
| S4 | pins, forks, `adopt`, `migrate` | S2, S3 | after S3 |
| S5 | call streams (KeyScope `narrow` deleted in v2.30 AN1) | S3 | after S3, beside S4 |

**Open questions for Sam:** none. Q1 is ruled (try_receive), and so are memory and identity.

---

## 1. API

**Conventions.**
- Module `store`, in package `store`.
- Every declaration below has the shape of a probe the checker accepted (RUN, `programs/`, on 78e07cb1), unless it is marked otherwise.
- **Dictionaries are values.** A table takes `codec.Key[K]` and `codec.Codec[V]` values (S0, §7). It takes no `codec` bound.
  - RUN on 78e07cb1: the bound itself is still refused, `LANG-5 unknown bound` (`codec_bound`).
  - RUN: the same API taking dictionary values is accepted (`tx_body`).
  - Once the bound lands, a program passes `codec.of()` where it passes a hand-written value today. So the store's API is final from S1 and never waits for the derive.
  - Explicit type arguments at a call are refused (M4-PLAN §2.1), so a dictionary comes by annotation: `let c: codec.Codec[Account] = codec.of()`.
- **Every handle names its store.** `Table`, `Index`, `Branch`, `Snapshot`, `Ticket` and `CallId` carry the 16-byte `StoreId`. `Snapshot` and `Ticket` also carry the number of the Db's open (§1.12). A handle from another Db is `Foreign`. That is checked before any effect.

### 1.1 Opening and closing

```hirsel
pub type Options {
    create: bool,
    lock_wait_ms: u64,
    group_commits: u32,       // default 64: the most frames one journal write takes; 0 is Missing("group_commits") at open (W1)
    group_bytes: u32,         // default 4 MiB: the most bytes one journal write takes; 0 is Missing("group_bytes") at open (W1)
    checkpoint_bytes: u64,    // default 64 MiB of journal since the last cut starts a checkpoint
    pack_bytes: u64,          // default 64 MiB: a pack closes before a NODE that would take it past this many bytes, unless it holds no NODE yet (H1)
    resident_limit: u64,      // default 1 GiB (GUESS, R8): the accounted resident bytes a Db admits (§1.11)
    checkpoint_task_bytes: u64, // default 262,400,276: the checkpoint task's root limit, max(pack_bytes, 64 MiB + 88) + 2⌈resident_limit / 11⌉ + 64 KiB (Q1a)
    authority: Authority,
}
pub type Authority = Directory()   // one variant today; row 12 adds a variant (§2.9). defaults() gives Directory(). RUN programs-v21/authority: ACCEPTED; `: plain` is refused (OWN-4: computed, never declared)
pub type StoreId { hi: u64, lo: u64 }
pub fn defaults() -> Options

pub opaque type Db                         // not send: holds Task handles (CONC-1/2). RUN db_task.
pub fn open(tasks: Tasks, d: Dir, c: Clock, r: Random, o: Options) -> Result[Db, Error] waits
pub fn memory(c: Clock, r: Random, o: Options) -> Db   // no directory and no tasks; durable(t) is Ok at once
pub fn close(take db: Db) -> Result[(), Error] waits   // §1.10: always consumes, always releases and joins
pub fn identity(db: Db) -> StoreId
```

`open` takes `Random` for the identity: 16 bytes from `host.random(r, mut b)` (host.hirsel:31, landed 1951a976), drawn once at genesis (D7).

```hirsel
pub type Error =
      Held                         // another writer holds LOCK
    | NotFound                     // no MANIFEST and create is false
    | Corrupt(Fault)               // bytes from disk failed a check (§2.8)
    | Unsupported(u64)             // a format number other than 1, or an unknown MUST kind (value = kind)
    | Io(IoError)                  // a host operation failed before anything was published
    | Poisoned(IoError)            // a write or a sync failed earlier; the Db accepts only close
    | Full(Capacity)               // resident_limit: an open, a schema commit or a committing admit would pass it (§1.11, AE3)
    | Limit(Limit)
    | Schema(u64)                  // codec.resolve: the stored writer schema does not resolve; offset into it (SEM-10)
    | Stale(u32)                   // a handle's id is not live in the catalog it reads (I3), or a Table value lacks an index the catalog has
    | Decode(codec.DecodeError)    // a stored row failed its codec: only a schema bug in user code
    | Exists(String) | Missing(String)
    | Diverged(u32)                // adopt: main moved after the branch forked (branch id)
    | Foreign                      // a handle of another store, or a Snapshot or Ticket of another open (§1.12)
pub type Limit = Key | Value | Transaction | Name
pub type Fault { file: String, offset: u64, why: Why }
pub type Why = Checksum | Truncated | Flags | Kind(u64) | Header | Gap(u64) | Order | Dangling(u64) | Malformed
```

There is **no `Cancelled` case.** A cancellation is a failure that `isolate` does not catch, so it never reaches a caller as a value (C5, §3.7).

**Limits.** These are fixed at creation and recorded in the manifest. The values are the old store's (STORE-RECORD.md, BY READING):

| Limit | Value |
|---|---|
| tree key (table prefix included) | 4096 bytes |
| value | 1 MiB |
| one transaction's COMMIT payload (the frame's `len`, head excluded) | 16 MiB + 64 = 16,777,280 bytes; the one transaction limit, the same as §2.7's COMMIT row (U1) |
| branch, pin and table names | 255 bytes |
| fan-out | 16–32 entries per node (998's occupancy) |

The old limit of 10,000 rows per range is dropped: a cursor needs no limit.

### 1.2 Tables and indexes

```hirsel
pub opaque type Readers[V]: shareable + send   // Codec[V], current schema number, List[Option[codec.Reader[V]]] by number
pub opaque type Table[K, V]: send              // StoreId, id, schema epoch, Key[K], Arc[Readers[V]], List[IndexOf[V]]
pub opaque type Shape[K, V]: shareable + send  // StoreId, id, catalog key, Key[K], Arc[Readers[V]]: what a reader needs, no indexes, no epoch (D1, I3)
pub opaque type Index[J, K, V]: shareable + send // StoreId, index id, its catalog key, owning table id, Key[J], Key[K], Arc[Readers[V]]

pub opaque type TableSpec[K, V]: send          // name, Key[K], Codec[V], and per index: name, `code.identity` of `by`, key schema, erased extractor (AB1)
pub fn spec[K, V](name: String, take key: codec.Key[K], take value: codec.Codec[V]) -> TableSpec[K, V]
pub fn with_index[J, K, V](mut s: TableSpec[K, V], name: String,
        by: StaticFn[fn(V) -> Result[J, Capacity]], take key: codec.Key[J]) -> Result[(), Error]   // erased as `index` does (RUN index_erase); Exists(name) for a repeated name
pub fn create_table[K, V](mut db: Db, take s: TableSpec[K, V]) -> Result[Ticket, Error] waits   // the table and every index in s, one schema commit (AB1)
pub fn table[K, V](mut db: Db, name: String, take key: codec.Key[K], take value: codec.Codec[V]) -> Result[Table[K, V], Error] waits
pub fn create_index[J, K, V](mut db: Db, t: Table[K, V], name: String,
        by: StaticFn[fn(V) -> Result[J, Capacity]], take key: codec.Key[J]) -> Result[Ticket, Error] waits
pub fn index[J, K, V](db: Db, mut t: Table[K, V], name: String,
        by: StaticFn[fn(V) -> Result[J, Capacity]], take key: codec.Key[J]) -> Result[Index[J, K, V], Error]
pub fn migrate[K, V, K2, V2](mut db: Db, take from: Table[K, V], take to: TableSpec[K2, V2],   // AB1: the target's name, codecs and indexes
        f: fn(K, V) -> Result[Option[(K2, V2)], Capacity]) -> Result[Ticket, Error] waits
pub fn drop_table[K, V](mut db: Db, take t: Table[K, V]) -> Result[Ticket, Error] waits
pub fn shape[K, V](t: Table[K, V]) -> Shape[K, V]
```

**`table` resolves the stored schemas against the reader's codecs (D1).**
- **Schema numbers.** Each table's catalog entry lists the value schemas its rows were written in, as (number u16, schema bytes, fingerprint). The list is append-only, and a number is never reused. Every row value starts with its schema number (§2.4).
- `table` computes the program's value schema (`codec.schema`) and finds it in the list (fingerprint, then bytes):
  - **found** as number n: n becomes the Table's current number, and new rows are written as n;
  - **not found:** it is appended as the next number, with a schema commit (§1.2 "Schema commits"; this is why `table` is `mut db … waits`), and becomes current. A program that never changes its types never makes this commit.
- Every other live number m is resolved once: `codec.resolve(value, schema_m)`, which applies SEM-10 (fields matched by name at every depth; a field the writer lacks takes its literal default; a removed field's tombstone checked).
  - Any `Schema(i)` makes `table` return `Schema(i)` and commits nothing. `i` is the offset into schema m.
  - The Readers are shared by one `Arc`, so a copy of a Table, Shape or Index never resolves again.
- **Reading a row:** tag = current number, then `codec.decode`. Tag = an older number, then `codec.read` with that number's Reader. A tag the Readers do not hold (a number appended by a later `table` call after this handle was resolved) is `Stale(table id)`.
- The key schema must be byte-equal to the stored one. A change of key order is always `Schema`, and only `migrate` fixes it.
- `migrate` writes every row as one new number and retires all older ones. It is the only way to drop an old schema. It gives the table, and each of its indexes, a new id (I3), so a handle from before it is `Stale` against later state and still reads pins and snapshots older than it.

**Read handles (I3).** A table or index id names one incarnation: ids come from the `next_table` counter and are never reused, so a table recreated after `drop_table`, or migrated, has a new id.
- Every read (`fetch`, `range`, `lookup`, `get`, `scan`, `find`, also inside `look`) first looks up the handle's catalog key in the catalog tree of the roots it reads: the Tx's roots, or the Snapshot's. Absent, or holding another id, is `Stale(id)`, with nothing read and nothing recorded.
- So a handle from before a drop is `Stale` against a current Tx or a later Snapshot, and reads its rows from a Snapshot or pin older than the drop. A handle made after the drop is `Stale` against that older Snapshot.
- The schema epoch is a write check only: it says whether a Table holds every index the catalog lists (below). A Shape or Index carries no epoch, because a read maintains no index.
- Cost: one catalog lookup per read call, not per row.

**`index` registers the index on the Table value.**
- It adds the index to `t` with the key type erased: an owning `own[send] fn(V) -> Result[Bytes, Capacity]` that captures `by` and the key codec. RUN: `index_erase`.
- It checks the catalog's entry: the index name, the identity bytes of `by` (`code.identity`) and the key schema. Any mismatch is `Schema`.
- `put` and `delete` keep every registered index up to date.
- A Table value missing an index the catalog lists makes `put` return `Stale(id)`. It never skips the index silently. The check compares the Table's schema epoch with the catalog's, which is one u64.
- PROG-9 gives `code.identity` only: a body edit to `by` with its identity unchanged is not seen. That waits for `code.revision` (M4-PLAN §2.5). Until then the catalog stores the revision as absent.

**Schema commits.** `create_table`, `create_index`, `drop_table` and `migrate` are made by a checkpoint (§3.5), never by a journal frame.
- Every schema commit (`create_table`, `create_index`, `drop_table`, `migrate`, and a schema number appended by `table`) advances the branch version by one, so an unchanged version means nothing was committed (K1). `table` finding its schema stored, `index`, a schema commit refused with `Full`, and a checkpoint alone commit nothing and leave the version unchanged. The runner's rule 12 relies on this.
- The catalog therefore changes only at a manifest swap, and every journal suffix is decoded under one catalog. A schema number is added the same way (D1).
- `migrate` builds the new rows as a new data tree in memory, applying `f` to each row. It drops every index of `from` and builds exactly the indexes `to` declares, each from the V2 rows with its own extractor. No extractor of `from` is ever applied to a V2 value (AB1). If any row, key or index cannot be built, it refuses and changes nothing: `Full(c)`, `Limit(Key)` or `Exists(name)`. The swap installs it with the new catalog entry at version + 1. It is atomic at the manifest rename. A rename is a migrate with the identity `f` (row 13).
- `migrate` admits the new tree against `resident_limit` before building it, using its accounted size (§1.11), else `Full`.
- Cost: a schema commit costs one checkpoint, which is 7 syncs (§3.2). Schema commits are rare.

### 1.3 Transactions

```hirsel
pub opaque scoped type Tx                // VERIFY-5 B2: opaque (PROG-2). Base roots (views), working roots, op list, read Log view, scope, CallInfo
pub type Outcome[R, A] = Committed(Done[R]) | Aborted(Undone[A]) | Failed(Causes) | Refused(Capacity) | Stopped(core.OutOfTime)   // AC1
pub type Undone[A] { value: A, ticket: Option[Ticket] }   // F2: the ticket of the kept reply commit, if any
pub type Done[R] { value: R, ticket: Ticket, info: CallInfo, reads: List[Read] }
pub type Read = Point(Bytes) | Span(Bytes, Bytes)   // data-tree keys (§2.4), the bytes of Change.tree_key; span [lo, hi) (I1)
pub type Bound[K] = Unbounded | Included(K) | Excluded(K)   // std.omap's names (D9); K need not be ordered

pub fn run[R, A](mut db: Db, steps: Option[u64], body: scoped[view] fn(mut Tx) -> Result[R, A]) -> Result[Outcome[R, A], Error] waits   // None: no step scope (AC1)
pub fn run_on[R, A](mut db: Db, b: Branch, steps: Option[u64], body: scoped[view] fn(mut Tx) -> Result[R, A]) -> Result[Outcome[R, A], Error] waits

pub fn fetch[K, V](tx: Tx, t: Shape[K, V], k: K) -> Result[Option[V], Error]       // a fresh value; Shape, F5
pub fn put[K, V](mut tx: Tx, t: Table[K, V], k: K, v: V) -> Result[(), Error]
pub fn delete[K, V](mut tx: Tx, t: Table[K, V], k: K) -> Result[bool, Error]
pub fn range[K, V](tx: Tx, t: Shape[K, V], lo: Bound[K], hi: Bound[K]) -> Result[Rows[K, V], Error]
pub fn lookup[J, K, V](tx: Tx, ix: Index[J, K, V], lo: Bound[J], hi: Bound[J]) -> Result[IndexRows[K, V], Error] in tx   // J1: scoped to the Tx
pub fn info(mut tx: Tx) -> CallInfo      // mut: only a transaction body has CallInfo; a look body cannot call it (G2, OWN-5)
pub type CallRecord: codec { key: Bytes, args: Bytes, stream: Bytes, revision: Option[Bytes], code: Bytes }   // F7; p and CallInfo come from the frame; revision: code.revision of the entry that ran; v2.30 AN1: code = u64 n ‖ u64 s (BE) ‖ replay identity (0 or 32 B)
pub type Plan { n_e: u64, s_e: u64, reply_max: u64 }   // v2.30 AN1: the body's byte and step allowances, and the longest reply
pub type Stop = Raised(u32) | OutOfSteps(u64) | OutOfBytes(Capacity)   // v2.30 AN1: Policy deleted
pub type Served = Committed(Receipt) | Kept(Receipt) | Failed(Pending, Stop) | NoRoom(Pending, Capacity)   // v2.29 AM1, AM3; ATTACK-4 B7: the refusal's Capacity
pub type Receipt { ticket: Ticket, reply: Bytes }   // v2.29 AM3: the first delivery, no read-back; v2.30 AN1
pub type ServeError = Before(Error) | After(Error)   // v2.29 AM4: Before leaves the position as it was; After is poison
pub opaque type Pending   // v2.29 AM1: not copy, codec or send; the Completion, the record, the run's CallInfo and base, and on NoRoom the outcome it would have stored
pub fn serve(mut db: Db, take c: Completion, plan: Plan, take rec: Option[CallRecord],
        body: scoped[view] fn(mut Tx) -> Result[Bytes, Bytes]) -> Result[Served, ServeError] waits   // v2.30 AN1: one isolated execution, the body's; with `conclude`, the only writer of a reply and a record
pub fn conclude(mut db: Db, take p: Pending, take r: Bytes, reserve: Reserve) -> Result[Served, ServeError] waits   // v2.29 AM1, AM6: r with p's record, CallInfo and base, one streams-only commit, no code run; Reserve = Resident | Claim
pub fn untried(take c: Completion, take rec: Option[CallRecord], info: CallInfo) -> Pending   // v2.29 AM1: for a position that runs nothing (a gone key)
pub fn recorded(db: Db, b: Branch, version: u64) -> Result[Option[Recorded], Error] waits   // write barrier, then live segments; None once a taken cut covers the version (W2); v2.29 AM6 (ATTACK-4 M3): with its settle context
pub type Recorded { record: CallRecord, settle: u8, branch: Branch, base: u64, at: u64, seed: u64 }   // settle 0: the frame's own context; 1 a body fault, 2 an admission refusal: the attempted run's

pub opaque type Rows[K, V]: send         // Arc shares of a path + bounds + Key[K] + Codec[V]
pub fn next[K, V](mut r: Rows[K, V]) -> Result[Option[(K, V)], Error]
pub scoped type IndexRows[K, V]          // index cursor, primary Shape, View[Log[Read]] of its Tx (J1)
pub fn next_indexed[K, V](mut r: IndexRows[K, V]) -> Result[Option[(K, V)], Error]   // records Point(table id ‖ Key[K]) per row returned
```

**What `run` does.**
- It runs `body` inside `isolate` in the caller's task. Reads append to a `Log[Read]` through a view. RUN: `tx_body` is the whole shape, with Tx, isolate, Log and `scoped[view]`, and it is ACCEPTED.
- `fetch` copies the row out, so the transaction is never frozen.
- `run` is `waits` for two reasons. After publishing, it hands the frame to the journal with a waiting `send` (§3.3), which waits only when 128 frames are already queued. And a commit that would pass `resident_limit` while the account holds unsettled charges waits for them to be settled before it is refused (§1.11, I4). The second wait comes before publish and the first after it, so a transaction is never cancelled halfway (§3.7).

**What each outcome means:**

| Outcome | Cause | Effect on the store |
|---|---|---|
| `Committed` | the body returned `Ok` | the commit is visible to the next transaction and to `snapshot` |
| `Aborted(u)` | the body returned `Error(a)`; `u.value` is `a` | the data writes are discarded. The reply writes (§1.8) are kept and committed as one frame with only a streams section, and `u.ticket` is that commit's ticket. With no reply nothing is committed and `u.ticket` is None. (v2.28 AL1: under `serve` a reply is always written, and this outcome is `Served.Kept`.) If that commit would pass `resident_limit`, the outcome is `Refused(c)` instead and nothing is kept (F2). |
| `Failed(causes)` | the body faulted (never `Cancelled`, which `isolate` does not catch, FAIL-16) | none |
| `Refused(c)` | isolate or, in a store with a journal, the commit encode ran out of capacity, or the commit would pass `resident_limit` (§1.11; `c.requested` is the account after it, `c.available` the limit) | none |

- `fetch` and `range` take a `Shape` (F5): a read needs only the key codec and the Readers, and a Shape is `shareable + send`, so one query body runs under `run` and under `look` on any task. `put` and `delete` take the `Table`, because they maintain its indexes. `lookup` takes the `Index`.
- The record (v2.28 AL1: `serve`'s `rec`, attached after the bound) adds a CALL section (§2.4) to this transaction's commit. It is kept when the outcome is `Committed`, and on `Aborted` it is kept with the body's `reply` writes in their streams-only frame (F2, G1); with no `reply` call it is discarded. `Failed` and `Refused` keep nothing.
- `recorded` reads live journal segments only. In a `memory` store it is always None (G3).
- **What `recorded` waits for (W2).** It does not depend on when the journal task writes. Its steps:
  1. A poisoned Db is `Poisoned(e)`. A handle from another store is `Foreign`. A `memory` store gives None.
  2. A version above the branch head gives None, because it was never committed. A version at or below the head of the latest cut this Db has taken gives None, unless the Db has learned that this cut was refused; then the cut before it counts. This needs no read.
  3. Unless `journaled` ≥ the sequence of the last frame this Db has sent, so that nothing is outstanding (Z1; the same condition as §3.5 step 0), it sends `Barrier` on the journal channel and receives from `written`. `Written` means every frame queued before the call is in its segment. `Failed(e)` means the journal task failed. `recorded` then puts `Failed(e)` back and returns `Poisoned(e)`. A `send` that returns `Closed` goes on to receive, and finds `Failed(e)`.
  4. It reads segments from the `first_live` of the cut in step 2 upward, and returns the CALL section of the COMMIT at (`b`, version), or None.
- So a call whose `run` returned `Committed` on this Db, or `Aborted` with a reply, gives Some until this Db takes a cut covering it, with no `durable` call needed. `recorded` takes a view of the Db: it consumes no ack and changes no counter, account or poison.
- `info` takes `mut Tx` (G2). Its value does not change, but `mut` limits it to a transaction body: `look` gives its body a view Tx, so `info` there is refused at check time.

**Cursors.**
- A cursor reads the transaction's state as it was at `range`. Writes the body makes after that are not seen by that cursor.
- `range` records `Span(lo, hi)` when it is made. The span is the whole requested interval, which is a superset of what the cursor reads.
- **Read keys are data-tree keys (I1).** `fetch(t, k)` and each row `next_indexed` returns record `Point(id BE ‖ Key(k))`. A span's `Unbounded` low is the 4-byte id prefix and its `Unbounded` high is the prefix of id + 1; `Included(k)` low is `id ‖ Key(k)` and high is `id ‖ Key(k) ‖ 0x00`; `Excluded(k)` low is `id ‖ Key(k) ‖ 0x00` and high is `id ‖ Key(k)`. A Change hits a read set when its `tree_key` equals a Point or lies in a Span, by byte compare.
- Iteration is `while let Ok(Some((k, v))) = store.next(mut rows) { ... }`:
  - RUN: `for` over a user cursor is refused, `LANG-16` (`cursor_for`).
  - RUN: `while let` is accepted, both with owned rows (`cursor_owned`) and with a view-returning `next` (`cursor_view`).
- `Rows` (from `range`, `scan` and `find`) holds Arc shares, not views, so it is `send` and can outlive the transaction. Reading a dropped transaction's frozen state is harmless. A range's Span is recorded whole up front, so later reads need no log. `lookup`'s `IndexRows` is the exception (J1).

**`lookup`** walks index entries in `J` order and fetches each primary row (J1).
- It records `Span` over the index keys when it is made, and `next_indexed` records `Point(table id ‖ Key[K])` for each row it returns, when it returns it. The Span covers rows entering or leaving the range. The Points cover the values returned, so a put that changes V at the same J hits a Point.
- `IndexRows` is scoped to the Tx (OWN-20): it holds the Tx's read log view, as `Tx` does. It cannot leave the body (LANG-13, RUN `ixrows_escape`), and the Tx cannot be written while it lives (OWN-10, RUN `ixrows_write`). A body that updates the rows it finds collects them first, then writes.
- `find` on a Snapshot has no read set and keeps a `send` `Rows`.

### 1.4 No key scopes (v2.30 AN1)

The store does no authorization. A Tx may read and write every key of its store, and a program decides access itself from the Caller the runner gives it. A `transact` or `granted` body receives the calling session's Caller. A query receives one only when it is registered as caller-dependent (`runner.query_as`, body `fn(Tx, H, Caller, take A)`, RUNNER-SPEC v2.25 rule 2). The runner then gives the body the subscribing session's bound Caller at its first run, at every rerun and at every repair. A caller-independent query (`runner.query`, body `fn(Tx, H, take A)`) receives no Caller. In both kinds each subscription keeps its own Cover and read set, so no query result passes between subscriptions. Nothing in the store changes: the Tx, `look` and the read set are the same for both kinds. A handle from another store is still `Foreign` (§1.12).

### 1.5 Snapshots and queries on any task (row 4)

```hirsel
pub opaque type Snapshot: shareable + send   // StoreId, open number, branch, version, three Arc roots. RUN snapshot_task.
pub fn snapshot(db: Db) -> Snapshot                  // main branch head
pub fn snapshot_of(db: Db, b: Branch) -> Result[Snapshot, Error]
pub fn version(s: Snapshot) -> u64
pub fn get[K, V](s: Snapshot, t: Shape[K, V], k: K) -> Result[Option[V], Error]
pub fn scan[K, V](s: Snapshot, t: Shape[K, V], lo: Bound[K], hi: Bound[K]) -> Result[Rows[K, V], Error]
pub fn find[J, K, V](s: Snapshot, ix: Index[J, K, V], lo: Bound[J], hi: Bound[J]) -> Result[Rows[K, V], Error]
pub fn follow(mut db: Db, b: Branch) -> Result[Latest[Snapshot], Error]   // D5; Full(Capacity) when the root is refused

pub type Looked[R] = Seen(Seen[R]) | Failed(Causes) | Refused(Capacity) | Stopped(core.OutOfTime)   // F5, alike with Outcome; AC1
pub type Seen[R] { value: R, version: u64, reads: List[Read] }
pub fn look[R](s: Snapshot, steps: Option[u64], body: scoped[view] fn(Tx) -> R) -> Result[Looked[R], Error]   // None: no step scope (AC1)
pub type Change { tree_key: Bytes, value: Option[Bytes] }   // the key domain of Read (I1); value is the stored row: u16-LE schema number ‖ codec bytes
pub fn published_value[K, V](t: Shape[K, V], c: Change) -> Result[Option[Bytes], Error]   // the row's value in t's current codec, prefix removed (AG3)
pub fn span_changes(s: Snapshot, lo: Bytes, hi: Bytes) -> Result[List[Change], Error]     // the data-tree rows of s in [lo, hi), key order, each with Some(stored bytes) (AG3)
pub fn changes(a: Snapshot, b: Snapshot) -> Result[List[Change], Error]   // F6
```

**The published value of a row (AG3).** `Change.value` is the stored row: a u16-LE schema number followed by the codec bytes (§1.2). It is not a wire value. `published_value(t, c)` gives the value a publisher sends. Its checks run in this order:
- A `tree_key` whose first 4 bytes are not `t`'s id (BE) is `Missing("table")`. This is checked first, so an index row's Change, deleted or not, is always `Missing("table")`; index rows live in the data tree under the index's id with empty values (`put_impl` :1893-1905).
- `c.value` = None (the key was deleted) gives `Ok(None)`.
- The stored number is `t`'s current number: the stored bytes after the 2-byte prefix, which are the bytes `put` wrote with the current codec.
- An older number that `t`'s Readers hold: the row is read with that number's Reader, then encoded with the current codec. Dropping two bytes is wrong here, because the old encoding can differ in length and layout.
- A value shorter than 2 bytes is `Decode(Truncated(0))`, and a number the Readers do not hold is `Stale(t's id)`, as in a typed read.
- A refused allocation is `Full(c)`, as for every store allocation (`capacity`, :845).
- The key is not touched: a published key is `c.tree_key` as it is (table id BE ‖ key codec bytes).
- `look`, `get`, `scan` and `find` give typed values, never stored bytes.
- A Cover's initial rows come from `span_changes(s, lo, hi)`: the data-tree rows of `s` with key in [lo, hi), in key order, each as `Change{tree_key, Some(stored bytes)}`. A refused allocation is `Full(c)`. So `published_value` normalizes initial rows and changed rows alike, and there is one path.
- "Current" is the number of the Shape built at open (RUNNER-SPEC rule 12).
- The function is typed. RUNNER-SPEC v2.11 §3 "Rows" asks for the function and does not say where it is erased. This spec proposes, and has put to runner-spec-1020, that a publisher seeing Changes for many tables erases it per Cover in the runner's `cover`, which already takes a `Shape[K, V]`, as `erase` (:1753) erases an index extractor.

- A Snapshot moves to another task through `task.spawn` or a channel.
  - RUN: a reader task took a `Snapshot` over `Arc[Node]` roots and read it while the spawner went on (`snapshot_task`).
  - Readers use `Shape`, which is `shareable + send` (an Arc share of its Readers, D1), so a reader needs no Table.
- **Following a branch (D5, CONC-13).** `follow(db, b)` makes, on its first call for `b`, a `Latest[Snapshot]` root holding `b`'s current head, keeps one handle in the Db, and returns another (`latest.handle`). After that, the writer `publish`es a new `Arc[Snapshot]` at every publish point of `b`: `run` step 4, `adopt` and schema commits. On main, an authority commit's register `publish` comes first (AM8, ATTACK-4 B1), so a follower that sees the commit's roots and then loads a reader sees its `seq`. A checkpoint changes no version, so it publishes nothing.
  - A reader calls `latest.read(h)` and gets an `Arc[Snapshot]`, which is never torn. In one reader's program order, the versions it reads never decrease.
  - The version read may not be durable yet, as with `snapshot`.
  - If `arc.new` is refused for the record, the commit is still made and the root keeps the previous version. The next publish catches up. Readers are never wrong, only behind.
  - `close` cleans the Db's handles. Readers' handles keep the last version readable. `delete_branch` publishes nothing more.
  - A program can still pass Snapshots on its own channels.
- `get`, `scan` and `find` are `Foreign` when the Shape or Index names another store than the Snapshot, and `Stale(id)` when its id is not in the Snapshot's catalog (I3).
- **`look(s, steps, body)`** (F5, RUNNER G2–G3; `steps` per AC1: `Some(n)` puts `bound steps n` around the query call inside the isolate, and a stop is `Stopped(OutOfTime { steps: n })`) runs `body` inside `isolate` over a Tx whose base and working roots are `s`'s, with scope everything, an empty op list and no CallInfo. The body receives a view `Tx`, so `put`, `delete`, `reply`, `record` and `info` (which take `mut Tx`) cannot be called: OWN-5 (RUN `look_put`, `info_look_mut`). `fetch`, `range` and `lookup` record `Read`s exactly as in `run`. Nothing is committed and no ticket exists. It needs no Db, so it runs on any task that holds a Snapshot. A fault is `Failed(causes)`; isolate's capacity is `Refused`. `Foreign` when a Shape or Index names another store.
- **`changes(a, b)`** (F6, RUNNER G2) gives every data-tree key (table id u32 BE ‖ Key bytes, or index id ‖ J ‖ K) whose value differs between `a` and `b`, ascending, with `b`'s value, or None when deleted. It descends both trees together and skips a pair of subtrees with the same node id: within one open a node id names one immutable node. Cost: O(k log n) node visits for k keys written between the two snapshots, same-value rewrites included, because a rewritten key's path has new node ids; w ≤ k changes are returned (H4). It is `Foreign` when `a` and `b` belong to different stores or different opens, because node ids are reissued after a reopen (§1.9). Different branches of one open are allowed. The streams and catalog trees are not compared.

**Keyset pages (v2.25, AI1).** A page is a bounded read of a table or an index on a Snapshot, plus an opaque continuation that resumes it exactly.

```hirsel
pub type Limits: shareable + send { rows: u64, bytes: u64 }
pub opaque type Resume: send
pub type Page[K, V]: send { rows: List[(K, V)], bytes: u64, next: Option[Resume] }
pub type Resumed[K, V]: send = Paged(Page[K, V]) | Moved(u64)
pub fn scan_page[K, V](s: Snapshot, t: Shape[K, V], lo: Bound[K], hi: Bound[K], l: Limits) -> Result[Page[K, V], Error]
pub fn find_page[J, K, V](s: Snapshot, ix: Index[J, K, V], lo: Bound[J], hi: Bound[J], l: Limits) -> Result[Page[K, V], Error]
pub fn scan_next[K, V](s: Snapshot, t: Shape[K, V], take r: Resume, l: Limits) -> Result[Resumed[K, V], Error]
pub fn find_next[J, K, V](s: Snapshot, ix: Index[J, K, V], take r: Resume, l: Limits) -> Result[Resumed[K, V], Error]
```

- **From the cursor that exists.** A page reads the same byte cursor as `scan` or `find`, over the same logical key bounds. It is not a second read path. A row's **logical key** is its data-tree key: table id (u32 BE) ‖ key codec bytes for a table, and index id ‖ J ‖ K for an index entry. An index orders entries with equal J by K, so the logical key orders every entry totally.
- **Limits.** `rows` = 0 is `Missing("rows")` and `bytes` = 0 is `Missing("bytes")`, checked before anything is read (W1's rule for zero bounds). A row's bytes are its data-tree key length plus its stored value length (the 2-byte schema prefix included), for the table row that is returned, also when it was found through an index. A page takes rows in key order while it has fewer than `rows` rows and the next row fits in what is left of `bytes`. **A page holds at least one row when one remains**, even when that row alone is larger than `bytes`. So a page never ends empty while rows remain, a caller can never be stuck, and a page's bytes are at most max(`bytes`, one row). `Page.bytes` is the sum it counted.
- **The end.** A page looks at one more key after its last row. When there is none within the bounds, `next` is None. So the last page has `next` = None, and an empty page occurs only when the bounds hold no row at all.
- **The continuation.** `Resume` holds the Snapshot's identity (StoreId, open number, branch and version), the source (table id or index id), the upper bound on the logical key with its inclusiveness, and the full logical key of the page's last row. It holds no Snapshot and no row. It is `send`, not `codec`: it never leaves the process. A served token (http.md B-11, one-shot reads) is a separate form that also binds the query, the caller and the scope and re-checks authority when it is used, and it waits on caller-bound row authority (gap 1117 #3).
- **Resuming.** `scan_next` and `find_next` check, in this order:
  1. the Shape or Index names another store than `s`: `Foreign`, as `scan` does;
  2. `s`'s identity is not the one the continuation was cut from: `Ok(Moved(version(s)))`, with nothing read;
  3. the Shape's or Index's id is not the continuation's source: `Stale(source id)`. A dropped and recreated table or index has a new id (I3), so a continuation cut from the old incarnation never reads the new one;
  4. the source is not in `s`'s catalog: `Stale(id)`, as for `scan`.

  It then opens the cursor at **Excluded(the full logical key)** and the continuation's upper bound, and makes a page by the same rule. So equal index keys are neither repeated nor skipped, and the page after an index row with J = j and K = k starts at the next (j, k') or the next j.
- **Moved is the stale result.** In process, the caller holds the Snapshot, so the version a continuation names cannot be gone while the caller still has it. What can happen is that a caller passes another Snapshot, for example a newer `snapshot(db)`. Continuing on it would mix two versions in one listing, so it is refused with `Moved`, which carries the version that was passed, and the caller starts again with `scan_page` or `find_page`. A continuation does not keep a Snapshot alive. The caller decides how long its Snapshot lives, and a retained version across restarts is a pin (§1.9).
- **Not in a transaction.** Pages read a Snapshot only. A transaction's `range` and `lookup` record their whole Span (I1), so a page in a Tx would record reads beyond the rows it returned. A Tx form, if a use appears, is a later addition with its own read-set rule.

### 1.6 Tickets and durability (row 3)

```hirsel
pub type Mark = Journal(u64) | Cut(u64)     // journal frame sequence, or checkpoint generation
pub type Ticket { store: StoreId, open: u64, mark: Mark }
pub fn durable(mut db: Db, t: Ticket) -> Result[(), Error] waits
pub fn checkpoint(mut db: Db) -> Result[Ticket, Error] waits
```

**What a ticket promises.**
- Once `durable(t)` returns `Ok`, the commit, and every commit before it on any branch, survives any later crash.
- Before then the commit is visible in this process, but a crash may lose it. If it is lost, every later commit is lost too: the durable state is always a prefix of the commit order.
- `durable` returns `Poisoned(e)` if the write or the sync covering `t` failed.
- `durable` returns `Foreign` for a ticket of another store, or of another open of this store. What an earlier open's commit became is decided by the reopened state, not by its ticket.
- In a `memory` store, `durable` is `Ok` at once and promises nothing.
- `flush` is gone (C7): every frame is in the journal's queue as soon as `run` returns, so there is nothing for a flush to hand over.

### 1.7 CallInfo (row 5)

```hirsel
pub type CallId { store: StoreId, branch: u32, version: u64 }
pub type CallInfo { id: CallId, at: u64, seed: u64 }
```

- `id` is the store identity, the branch and the version this commit will have. It is unique across stores (given a Random identity, C4) and across forks, because a branch id is never reused (§2.6).
- A transaction body reads it with `info(mut tx)` (G2); a `look` body has none and cannot call `info`.
- `at` is the wall time in milliseconds at `run`, raised to the branch's previous `at` if it is lower, so it never decreases on a branch. It is stored in the COMMIT frame, so a reopen gives the same `at`.
- `seed` is derived and never stored:
  - `seed = sm(sm(sm(sm(store.hi) ^ store.lo) ^ branch) ^ version)`, where `sm` is the splitmix64 output function: `z += 0x9E3779B97F4A7C15; z = (z ^ (z >> 30)) * 0xBF58476D1CE4E5B9; z = (z ^ (z >> 27)) * 0x94D049BB133111EB; z ^ (z >> 31)`, all wrapping.
  - Check value: `sm(0) = 0xE220A8397B1DCDAF`, the published first output of splitmix64 seeded with 0. With `store = {1, 2}`, branch 0 and version 1, `seed = 0xC817A9BF84F9A191` (RUN, by a separate Python implementation; L18 uses both values).
- v2.26 AJ4: the caller needs no frame kind. Its key is the prefix of the stream key, so `CallRecord.stream` names it (RUNNER-SPEC v2.15 item 7).

### 1.8 Call streams (row 6)

```hirsel
pub opaque type RequestDigest: shareable + send   // 32 bytes; made only by request_digest or read from a streams value; compared with == (AH1)
pub fn request_digest(key: Bytes, args: Bytes) -> RequestDigest   // SHA-256 (std, std-sha256-1071) of u8 17 ‖ "hirsel.request.v1" ‖ u32 len ‖ key ‖ u32 len ‖ args
pub type Admission = Admitted(Completion) | Duplicate(Stored) | Rejected(u64) | Abandoned(Completion)   // store rule (D6): re-admission under a new open; v2.28 AL1, AL2
                   | RequestMismatch | HistoryUnavailable   // AH1, AH2: RUNNER-SPEC v2.11.3 Refusal tags 6 and 7, mapped 1:1
pub fn admit(mut db: Db, stream: Bytes, p: u64, d: RequestDigest) -> Result[Admission, Error] waits
pub type Stored { reply: Bytes }   // v2.30 AN2: the saved reply, unchanged
pub fn ticket(c: Completion) -> Ticket   // v2.28 AL1: the admit's ticket, which the rows below call `t`
// v2.27's `reply` is deleted (v2.28 AL1): `serve` writes the reply
```

**v2.28 AL1:** in the table below, `Admitted(t)` and `Abandoned(t)` carry the position's `Completion`, and `t` is its `ticket`.

Stream state lives in the `streams` tree: key = the stream name, value = (position, digest, Admitted | Replied(reply)) (AH1, v2.30 AN2).

**The request digest (AH1).** `request_digest(key, args)` is SHA-256, std's (std-sha256-1071), over `u8 17 ‖ "hirsel.request.v1" ‖ u32 len(key) ‖ key ‖ u32 len(args) ‖ args` (integers little-endian, §2). The 17 ASCII bytes name the encoding and its version 1. `key` is the immutable entry Key, 32 bytes (16 identity ‖ 8 args_fp ‖ 8 result_fp; for a gone entry, the key the slot requested). `args` are the exact argument bytes.
- Not inputs: the connection's slot, the deadline and the server's executable revision. A reconnect remaps slots, a retry changes its deadline, and deployed code may change, so an unchanged retry has an unchanged digest.
- The runner computes it at ingress (RUNNER-SPEC v2.11.3, row 1) and passes it to `admit`. A digest from a client is never taken.
- The two runner join paths, H1 and V2, compare it before sharing an answer. They are RUNNER-SPEC v2.11.3's rows 2 and 3, not restated here.
- Journal replay and a pack load apply the stored value's bytes as they are; nothing is rehashed. The store's xxh3 frame check is a corruption check, not an identity hash.

**`admit(s, p, d)`** (a commit, so it hands off like `run`):

| Request | Result |
|---|---|
| `p` is the latest position + 1 and the latest position has a reply, or the stream is new and `p` = 0 (AE1) | commits (`p`, `d`, Admitted), binding `d` to `p`, and returns its ticket. The runner calls `durable` on it before an effect outside the store; a transact body need not wait, because the journal is prefix-ordered (§3.3) and its reply shares a later frame with its writes (RUNNER-SPEC v2 §2 H2, F3). |
| `p` is the latest position + 1 and the latest position is admitted and not replied | `Rejected(latest)`; nothing is committed. A stream passes a position only after its reply is in the store (rule S7, F1; RUNNER-SPEC v2 §2 H4) |
| `p` is the latest and its stored digest is not `d`, whatever its state | `RequestMismatch`; nothing is committed and nothing runs (AH1). This row is checked before the three below. |
| `p` is the latest, with digest `d`, and has a reply | `Duplicate(Stored{reply})` (v2.30 AN2) |
| `p` is the latest, with digest `d`, admitted by an earlier open and never replied | re-commits (`p`, `d`, Admitted) under this open, keeping the digest, and returns `Abandoned(t)` with its ticket (an earlier open's ticket is `Foreign`, L20). The caller runs `p` again (`t` need be durable first only before an effect outside the store); nothing committed at `p`, because a reply shares its commit with the effect. |
| `p` is the latest, with digest `d`, admitted in this open and never replied (its run aborted without a `reply` call, failed or was refused) | `Abandoned(t)` with the existing ticket; nothing is committed |
| the stream is new and `p` ≠ 0 | `Rejected(0)`; nothing is committed (AE1) |
| `p` < the latest | `HistoryUnavailable`; nothing is committed (AH2). The store keeps one state per stream, so it holds no digest for `p` and cannot say whether `p` ran these bytes. |
| `p` > the latest + 1 | `Rejected(latest)` |

**An admit past `resident_limit` (AE3).** The two rows that commit, `Admitted(p)` and the `Abandoned` re-commit for an earlier open, are admitted against the account as every commit is (§1.11): `admit` first settles unsettled charges, exactly as `run` does (AF1). If the account after it would still pass `resident_limit`, the result is `Error(Full(c))`, with `c` = `Capacity{requested: the account after it, available: resident_limit}`, and nothing is committed: the stream keeps its state. `Duplicate`, `Rejected`, `RequestMismatch`, `HistoryUnavailable` and the same-open `Abandoned` commit nothing, so they answer whatever the account holds. The digest is compared before the settle and the limit check; a `Full` binds nothing.

**Client positions (AG1, AH1, AH2).** `admit` takes the request's digest. At the latest position the store tells two requests apart and refuses the second (`RequestMismatch`). Below latest it keeps no digest, so it answers `HistoryUnavailable`. Only `Admitted` binds a digest to a position.

| Answer | The stream |
|---|---|
| `Admitted(t)`; `Abandoned(t)` for an earlier open's admit | changes: (`p`, digest, Admitted) is committed; the re-commit keeps the digest already bound |
| `Duplicate(r)`, `Rejected(l)`, `RequestMismatch`, `HistoryUnavailable`, `Abandoned(t)` within this open, `Error(Full(c))` (AE3) | is left as it was; no digest is bound |
| a refusal before admission (the runner's V1 refusals; RUNNER-SPEC §2 V1 owns their list; `admit` is never called) | is left as it was, and nothing is recorded |

- **P1.** A client advances `p` only after a reply for `p` (`Returned`, or a reply-only `Failed`).
  - A refusal before admission does not consume `p`.
  - The client gives that `p` to its next request only when no admitted attempt of the refused request can remain, which means the refusal answered the request's only send. Otherwise it keeps the request and sends it again.
  - `Abandoned` and the runner's retryable refusals (RUNNER-SPEC owns their list) keep the request and `p`.
  - Advancing after a refusal sends `p` + 1 while latest is `p` − 1. The "`p` > the latest + 1" row answers that `Rejected(latest)` for ever.
- **P2.** A client never sends `p` with bytes other than its committed request's. The store enforces it at the latest position: those bytes get `RequestMismatch`, and nothing commits or runs. The runner enforces it before admit on its join paths (RUNNER-SPEC v2.11.3 H1, V2). `RequestMismatch` means: fix the request, and never send these bytes at `p` again.
- **P3.** `Duplicate(r)` proves that `p` completed with these request bytes, because the digests are equal. `HistoryUnavailable` proves nothing about the request sent.
  - Recovery re-sends the original principal, stream, `p` and request bytes. Slot and deadline may differ; the digest does not.
  - `HistoryUnavailable` is terminal only when it answers the client's pending request at that `p`: the client then retires the stream and starts no replacement work automatically. An answer for a position the client has already advanced past (a delayed retry of a completed `p`) is not for any pending request and is dropped, as any stale answer is. Work whose bytes or outcome were lost stays unknown (WIRE-SPEC §4 rules 1–4 report it `LocallyUnknown`; WIRE W4c fences the generation).
- WIRE-SPEC cites this paragraph for its SDK rules, so the rules have one source.

**The reply** is written only by `serve` or `conclude` (v2.28 AL1): (`p`, the digest bound at `p`, Replied(r)) for its `Completion`'s stream and position (v2.27 AK1, AK3) in the same commit as the handler's writes, so the effect and the reply are one frame. It fails with `Missing("reply")` unless the stream's state is `Admitted(p)` (AE2). A reply written by a body that returns `Error` is kept, alone, in one frame (§1.3, F2).

- **A reply with no effect** (a gone entry, RUNNER §6; a `Failed` call) is a `conclude` (v2.28 AL1: of a `Pending` from `serve`'s `Failed`, or from `untried` for a gone key): `Committed`, with a frame that has only a streams section (F4). A re-send of `p` gets it back as `Duplicate`.
- **Below latest** the store answers `HistoryUnavailable` (AH2), so it gives no basis for the runner's `Superseded`; v2.24 withdraws F4's proof.

**Retention** is one state per stream (M4-PLAN §2.4).

### 1.9 Pins and forks (rows 8 and 9)

```hirsel
pub type Branch { store: StoreId, id: u32 }
pub fn main_branch(db: Db) -> Branch                            // id 0 of this store
pub fn fork(mut db: Db, name: String, from: Snapshot) -> Result[(Branch, Ticket), Error] waits
pub fn branch(db: Db, name: String) -> Option[Branch]
pub fn adopt(mut db: Db, b: Branch) -> Result[Ticket, Error] waits     // fast-forward main to b, then delete b
pub fn delete_branch(mut db: Db, b: Branch) -> Result[Ticket, Error] waits
pub fn pin(mut db: Db, name: String, s: Snapshot) -> Result[Ticket, Error] waits
pub fn unpin(mut db: Db, name: String) -> Result[Ticket, Error] waits
pub fn pinned(db: Db, name: String) -> Option[Snapshot]
```

**Pins and forks are manifest state.**
- `pin`, `unpin`, `fork`, `delete_branch` and `adopt` each make a checkpoint cut, and their ticket is that cut's generation.
- Reason: a pinned or forked version older than the last checkpoint may have nodes that no pack holds. Writing them at the cut is the only way the pin survives a crash, and the incremental checkpoint writes only those nodes.
- `fork` and `pin` take only a Snapshot of this open (`Foreign` otherwise). A Snapshot of an earlier open may hold node ids that this open has handed out again, because ids past the manifest's `next_node` are reissued after a reopen.

**What each does:**

| Operation | Precondition | Effect |
|---|---|---|
| `fork` from a snapshot of branch `p` at version `v` | — | the new branch's head is at `v`; it records `parent` = `p` and `forked_at` = `v` (AA1) |
| `adopt(b)` | `b` is not main, `b`'s parent is main, and main's version still equals `b`'s `forked_at`; else `Diverged(b)` (Noms FastForward; AA1: a version is compared only on the branch it belongs to) | main takes `b`'s roots and version, and `b` is deleted |
| `pinned` | — | gives the Snapshot back. It is the same value after a reopen, because the manifest holds its roots. |

**Reader pins.**
- The old store's reader pins (holders, tokens and principals, STORE-RECORD.md) are not rebuilt as such.
- In-process, a held `Snapshot` is the reader pin.
- Across processes, the named pin is the reader pin. There is one writer process, and no other process reads the directory.

### 1.10 Ownership, close, drop and poison

| Value | Owner and kind | Held cost | On `close`, drop or poison |
|---|---|---|---|
| `Db` | the opening task; own, not send (Task handles inside, RUN `db_task`) | its heads, the LOCK file, the channel ends (including both ends of `written`, W2), and two tasks whose root limits are reserved from the opener's limit at open (CONC-3): the journal task's 16,842,832 bytes and the checkpoint task's `checkpoint_task_bytes` (Q1a) | see the rules below |
| `Snapshot` | whoever holds it; send; three Arc shares | memory for the nodes it reaches that no head reaches: up to 2h+1 nodes per later one-key commit, at most the whole tree | unaffected by close, drop and poison: it is an immutable value. It may contain commits that a poisoned Db never made durable. |
| named pin | the manifest | memory as for a Snapshot, plus disk for those nodes until `unpin` and the next checkpoint | survives every crash after its ticket is durable |
| branch | the manifest, plus the Db's heads | the same as a pin, plus its journal frames until a checkpoint | as for a pin |
| `Table` | the program; own, send | its codecs and erased index closures | still a value; every use goes through a Db and fails if that Db is poisoned |
| `Ticket`, `Branch`, `CallInfo` | plain values (computed) | none | inert values |
| `Shape`, `Index` | shareable + send; a copy is an Arc share of the Readers (D1) | the Readers: one resolved plan per older schema number, shared by every copy | inert values; the Readers are freed with the last share |
| `Latest[Snapshot]` handle | each holder; send resource (CONC-13) | the root (32 bytes, CONC-13 as landed in a4f6cc07; was 16) and the version it holds | the Db's handles are cleaned at close or drop; readers' handles keep the last version |
| `Rows` | its holder; send | Arc shares along one path (depth ≤ 1 + ⌈log16 ⌈n/16⌉⌉) | unaffected |
| `IndexRows` | the body; scoped to its Tx (J1) | as `Rows`, plus one view of the read log | does not exist outside `run` or `look` |
| `Tx` | `run`'s frame; scoped | the working trees' path copies and the op list | does not exist outside `run` |

**`close(take db)` is an unconditional cleanup path (C6).** It always consumes the Db, and its steps are fixed:
1. If the Db is not poisoned, wait for the ack of the last frame sent, and for any checkpoint in flight. A failure here records the poison and goes on. A `Refused` checkpoint ack is not a failure and records nothing (Q1).
2. Drop both Senders. The journal task gets `Closed` after draining its queue (§3.3), writes, syncs, acks and writes the final END. The checkpoint task finishes or abandons its cut at a crash-equivalent step (§3.6) and ends.
3. Join both tasks. A task that already ended on a failure joins at once.
4. Drop the LOCK `HostFile`, which releases the flock.
5. Return `Ok(())`, or `Error(Poisoned(e))` where `e` is the **first** stored write or sync error, whether it happened before `close` or during it.

When the Db is poisoned, step 1 is skipped: anything still queued is dropped by CONC-9 when the channel goes, exactly as a crash would drop it.

**Drop without close.** Cleaning the Db cleans its Task handles. CONC-8 cancels each task and waits for it with cancellation shielded. An issued write or sync completes (CAP-8), queued frames are dropped (CONC-9), and the LOCK file closes. The state on disk is the crash cut at that moment (§3.6).

**A poisoned Db** returns `Poisoned(e)` from every call except `close`, `identity` and `snapshot`. Snapshots of it stay readable.

### 1.11 Resident memory and its admission bound (ruling 2)

**All state is in memory for M4.** Files are read whole (finding 10), so the store's resident size grows with the data. `resident_limit` bounds it.

**The account.** The writer keeps one u64, `resident`, in accounted bytes:
- Each row of the trees counts `entry(k, v) = 62 + r8(|k|) + r8(|v|)`, where `r8` rounds up to 8 and a table row's `|v|` includes its 2-byte schema number (D1).
  - 48 is the two `Bytes` handles (24 each, core.md, `List` row).
  - `r8` is each handle's storage (budgets.md:105).
  - 14 is the node share per entry at 998's minimum occupancy of 16. That is the node itself (72), its parent entry (48) and the checkpoint task's record for it (96), all divided by 16.
- **While the store has no pin and no branch but main, the account is exact after every commit (I4):** a put adds its `entry` and, over an existing key, subtracts the old row's `entry`; a delete subtracts its own. A store that overwrites one key forever has a constant account.
- **While a pin or another branch exists, it is conservative between cuts:** each commit adds `entry` for every put, and a delete or overwrite subtracts nothing, because the old row may stay reachable from the fork point or the pin.
- The frames sent but not yet acked also count, by their bytes (at most 128 frames, §3.3).
- At each checkpoint the checkpoint task's reference counts give the exact accounted bytes of every live node, and the ack replaces the conservative figure with that one. **A `memory` store makes the same cuts** (on `checkpoint` and at `checkpoint_bytes`): it runs the count and writes no file.
- A Snapshot that only a reader holds is the program's value: it is not counted.
- A `streams` row is `entry(name, value)`. An admitted value is 41 B: position 8, digest 32, state 1 (AH3). A replied value is 41 + R: those 41 and the reply R (v2.30 AN2); a replied row costs 62 + r8(S) + r8(41 + R). For an 8-byte name an admitted row (R = 0) is 62 + 8 + 48 = **118** accounted bytes, and a replied row with an 8-byte reply is 62 + 8 + r8(49) = **126**.

**The bound.**
- A commit whose account after it would pass `resident_limit` is `Refused(Capacity{requested: after, available: limit})`, with no effect. A delete-only commit is never refused.
- **Settle before refusing (I4, AF1).** If the account holds charges added since the last cut under a pin or branch, each commit settles them before checking `resident_limit`. This applies to `run`, a committing `admit`, every schema commit (`create_table`, `create_index`, `drop_table`, `migrate`, a schema number appended by `table`) and `fork`. A fork settles its admission account by counting the prospective union described below. The other paths settle the current account inline for a `memory` store or by requesting a checkpoint and waiting for its ack for a disk store.
- `open` computes the account from the decoded packs and the replayed journal. If it passes the limit, the result is `Full(Capacity)` and the LOCK is released.
- `migrate` adds its new tree's accounted size before building it, after the settle, and returns `Full` if the sum exceeds the limit.
- Before making a cut, `fork` uses `state_account` to count the exact union of nodes reachable from the proposed heads and the existing named pins. Each shared node counts once. A node previously held only by a reader counts if the new head reaches it. If the count exceeds `resident_limit`, `fork` returns `Full(Capacity{requested: count, available: limit})` without making a cut or installing a new head.
- A committing `admit` whose account after it, after the settle, would pass the limit is `Full` and commits nothing, as a schema commit is (AE3, AF1, §1.8).
- Open's transient peak is the account plus the largest listed pack. A pack is at most max(`pack_bytes` of the open that wrote it, 33,686,116 bytes), which is a HEADER file plus one NODE at the payload limit (H1), so 64 MiB with the defaults. The reader's bound for a pack comes from its manifest entry, never from this open's `Options` (V2). A pack beyond the checkpoint task's root allowance makes the open `Full(c)`.

**What it costs**, from the node sizes. This is arithmetic, with the occupancy taken at its worst, 16; R8 calibrates the constants:

| Rows (12-byte keys, 32-byte values + 2-byte schema number) | No index (118 B/row) | One index (+86 B/row, 20-byte index keys, empty values) |
|---|---|---|
| 1 M | 112.5 MiB | 194.5 MiB |
| 10 M | 1.10 GiB | 1.90 GiB |

- At the best occupancy, 32, it is about 6% less.
- The old store held 34.3 MB RSS at 100 k rows, which is 343 B/row (STORE-RECORD.md, store-prior-art figures).
- The default limit of 1 GiB (GUESS) admits about 9.1 M rows of this shape without an index.

**Paging later needs no change to node bytes.** A NODE frame is self-contained (§2.5), and the manifest names every pack. A store with a positioned read (finding 10) can keep the node table's pack and offset in place of the Arc and load a node on first touch. Only the in-memory record changes.

### 1.12 Handles and their store

- `StoreId` is the 16-byte identity in every file header (§2.3).
- `open` is the number of the first journal segment this open created. Segments are never reused, so it is unique per open of a store.
- A handle is checked against the Db or Snapshot it is used with before anything is read or written:
  - `Table`, `Index`, `Shape` and `Branch` must name the same `StoreId`. A Table, Shape or Index from an earlier open of the same store is valid, because every use checks its id against the catalog it reads, and writes check the epoch too (`Stale`, I3).
  - A `Snapshot` passed to `fork` or `pin`, and every `Ticket`, must also carry this open's number.
- Any mismatch is `Foreign`, and nothing changes.
- Cost: one 16-byte compare per call (GUESS: below the noise of R1).

### 1.13 Trigger rows: the attempt fence and the hold mark (v2.31 AO3)

Triggers and the step wrapper are a library over transact entries (ENDSTATE storage::triggers, storage::steps). The store adds no API, frame kind or tree for them: a trigger row is a row of the library's own table, and the trigger build implements both names below. They are named here because each is a store-level fact about which commits land.

- **`StaleAttempt`** (a refusal). A granted trigger's attempt n begins with a commit that raises the trigger row's attempt from n − 1 to n. Every transact call the run then makes names (trigger identity, n), and the same transaction checks that the row's attempt is still n. If it is not, the call is refused `StaleAttempt` and commits nothing: no write of the body, no step row, no outcome. The run's terminal commit is fenced the same way. So a run of attempt n whose result arrives after attempt n + 1 has started (a same-process store reopen allows it) changes nothing (ENDSTATE steps, law 7).
- **Held** (a run state). A trigger row that carries a hold mark is held. The step wrapper commits the mark when a later attempt's step request differs from the step row at its index, holding the attempt, the step index and both request fingerprints (steps, law 4), or when the place's revision has changed since attempt 1's start, holding both revisions (steps, law 9). The commit that writes the mark ends the attempt with no outcome. The place starts no held trigger. Only the program or an operator ends the hold: releasing it lets the next attempt run, and cancelling it ends the run with `Cancelled` (steps, law 7).

---

## 2. On-disk format

All integers are little-endian unless marked BE. "str" means a u16 length followed by that many UTF-8 bytes; the decoder checks the UTF-8 (`Malformed`). "bytes" means a u32 length followed by the bytes.

### 2.1 Frame (every file is a sequence of frames)

| Offset | Size | Field | Rule |
|---|---|---|---|
| 0 | 2 | kind u16 | 0 is invalid |
| 2 | 2 | flags u16 | bit 0 = MUST; bits 1–15 must be zero, else `Flags` |
| 4 | 4 | len u32 | payload length; above the kind's limit (§2.7), or above `UNKNOWN_LIMIT` for an unknown kind, is refused before any allocation |
| 8 | 8 | check u64 | xxh3-64 (seed 0, default secret) of the 33-byte binding `identity[16] ‖ role u8 ‖ seq u64 LE ‖ offset u64 LE`, then bytes [0, 8) of this head, then the payload. A mismatch is `Checksum`. |
| 16 | len | payload | decoded exactly; bytes left over are `Malformed` |

**The binding.**
- `identity` comes from the file's HEADER. For the HEADER frame itself, it comes from that HEADER's own payload, which is then compared with the manifest's (L19).
- `role` and `seq` are the file's: from its name for segments, packs and END files, and the HEADER's `seq` (the generation) for MANIFEST.
- `offset` is the frame's first byte in its file.
- So a frame is valid only in the one place its writer put it: a copy from another store, another file or another offset fails its check.
- Copy-forward (§3.5) recomputes the check of each node it moves.

**Why xxh3-64** (E1, ruled 2026-11-16):
- R9 measured it at 2.25 instructions per byte, against 6.51 for table-driven CRC-32C, in Hirsel on main (`int.mul_wide` gives the 64×64→128 multiply).
- It is 64 bits.
- It is the checksum of the compiler store and the tape, so the project has one checksum.
- Detection is probabilistic: about 2⁻⁶⁴ per damaged frame (E4).
- No hardware CRC is available to Hirsel code.
- It is library code: `std.check` (E7), not a core operation.

### 2.2 Files and their roles

| File | Role | Contents | Written |
|---|---|---|---|
| `MANIFEST` | 1 | HEADER, then exactly one MANIFEST frame; unknown kinds per §2.7 may come before or after it, within `UNKNOWN_ROOM` (V1) | `.tmp`, sync, rename, sync_dir |
| `j-<seq>` | 2 | HEADER, then COMMIT frames | created at open and at each cut; appended by the journal task |
| `p-<seq>` | 3 | HEADER, then NODE frames, children first | `.tmp`, sync, rename, sync_dir |
| `j-<seq>.end` | 4 | HEADER, END | created with its segment; rewritten whole (open `Write` truncates) and synced by the journal task |

### 2.3 HEADER (kind 0x0001, MUST): the first frame of every file, 56-byte payload

| Off | Size | Field | Rule |
|---|---|---|---|
| 0 | 8 | magic | `HIRSTORE`, else `Header` |
| 8 | 4 | format u32 | 2 (v2.30 AN2); any other value is `Unsupported(format)` |
| 12 | 1 | role u8 | 1 manifest, 2 journal segment, 3 pack, 4 end file; must match the file's name |
| 13 | 1 | authority u8 | 0 = local directory. 1–255 reserved for row 12; non-zero is `Unsupported` today |
| 14 | 2 | zero | |
| 16 | 16 | identity | equal to the manifest's, else `Header` (a file from another store) |
| 32 | 8 | epoch u64 | row 11: the claim epoch of the writer. 0 today; non-zero is `Unsupported` |
| 40 | 8 | seq u64 | manifest generation, segment number or pack number (role 4: its segment's); must match the file name |
| 48 | 8 | prev_end u64 | segment only: the valid byte length of segment seq − 1, or 0 when the manifest's cut covers it. 0 for other roles. |

**Writer schema.** The writer schema of each table is in the catalog tree, which the manifest roots, and is matched by name at `table` (SEM-10). The catalog changes only at a manifest swap, so one copy is exact.

**Identity.** 16 bytes from `host.random(r, mut b)`, drawn once at genesis and never again (D7). §1.7's uniqueness claim rests on it. The field holds those bytes in draw order. `StoreId.hi` is bytes [0, 8) as u64 LE, and `lo` is bytes [8, 16) as u64 LE (V3).

### 2.4 Journal segment `j-<seq>` and its end file

**COMMIT (kind 0x0011, MUST)**, payload:

| Field | Encoding |
|---|---|
| branch | u32 |
| version | u64: must equal the branch head + 1 during replay, else `Gap(version)` |
| at | u64 milliseconds |
| sections | u8 count, 1–3; each section is: tag u8 (0 data, 1 streams, 2 call; the catalog never appears). Tags 0 and 1: ops u32, then ops. Tag 2 (F7, at most one, written by `serve` or `conclude` from its record, v2.28 AL1): key (u32 length ‖ bytes), args (u32 length ‖ codec bytes), stream (u32 length ‖ name), revision (u32 length ‖ bytes, length 0 for None or 32 for Some, any other length `Corrupt(Malformed)`, AD1), code (u32 length ‖ bytes, v2.30 AN1: u64-BE n ‖ u64-BE s ‖ the replay identity, 16 or 48 bytes; a section that ends before it is `Corrupt(Malformed)`), settle (u8, v2.29 AM6: 0, or 1 (a body fault) or 2 (an admission refusal, ATTACK-4 M3) followed by base u64-BE ‖ at u64-BE ‖ seed u64-BE; any other value `Corrupt(Malformed)`); its position is that stream's op in the same frame. Its CallInfo is the frame's when settle is 0 (§1.7), and the recorded run's when settle is 1 or 2 |
| op | tag u8 (1 put, 2 delete), key bytes, value bytes only when tag = 1 |

- Ops are in ascending key order with no duplicates; the writer emits them from the op list sorted and deduplicated.
- The decoder does not check order (§2.8): applying a checksummed frame in order gives the state the writer had either way.
- No frame records which commits shared a sync, so replay cannot depend on it (C7).
- Replay applies sections 0 and 1 and skips a call section by its lengths; the call section changes no state. A call section whose stream has no streams op in the same frame is `Corrupt(Malformed)`.
- A typed error with a reply (§1.3, F2) and a reply with no effect (F4) are COMMIT frames with a streams section and no data section. The typed error's frame also carries the call section when the body called `record` (G1).

**Size.** A one-key put on the plan's workload (8-byte user key, 4-byte table prefix, 32-byte value) is 16 + 4 + 8 + 8 + 1 + 1 + 4 + 1 + 4 + 12 + 4 + 34 = **97 bytes** (the value is 2 bytes of schema number and 32 codec bytes, D1). The old store wrote 2,528 log bytes for the same commit (STORE-RECORD.md, historical cost targets). BY READING arithmetic.

**Size with a call section (F7, AD1).** The section is 1 + (4 + key) + (4 + args) + (4 + stream) + (4 + revision). With a 32-byte entry key (16 identity ‖ 8 args_fp ‖ 8 result_fp), 16 args bytes, an 8-byte stream name and a revision, that is 1 + 36 + 20 + 12 + 36 = **105 bytes** (73 with no revision). **v2.28 AL5, AL6:** the section adds 4 + |authority| = 68 + L (L the launch digest's length), and the replied value is 73 + 8 = 81, so the call below is 97 + (1 + 4 + 1 + 4 + 8 + 4 + 81) + 105 + 68 + L = **373 + L bytes** (305 without the authority field). **v2.29 AM6:** `settle` adds 1 byte, giving **374 + L** (406 for L = 32, the replay identity's length, AM7). A concluded record (settle 1 or 2) adds 24 more. The v2.27 figure: a transact call that writes one row and replies 8 bytes on that stream: 97 + (1 + 4 + 1 + 4 + 8 + 4 + 49) + 105 = **273 bytes** (241 before the digest, AH3) (the streams section: tag, ops count, op tag 1, an 8-byte name, and the value position 8 ‖ digest 32 ‖ state 1 ‖ reply 8). **v2.30 AN1, AN2:** the authority field becomes `code`, which adds 4 + 16 + L, and the replied value is 41 + 8 = 49 again, so the same call is 273 + (20 + L) + 1 (settle) = **294 + L bytes**, 326 for L = 32. BY READING arithmetic.

**Size of an admission (AH3).** A committing `admit` of one stream with an S-byte name is a COMMIT of 16 (frame head) + 21 (branch, version, at, section count) + 5 (streams tag, ops count) + 1 + 4 + S + 4 + 41 = **92 + S bytes**: 100 for S = 8 (68 before v2.24). A reply adds 32 + R, its scope digest and its bytes (v2.29 AM7). In a pack, an admitted stream row's leaf entry is (4 + S) + (4 + 41) = 49 + S, and a replied one (4 + S) + (4 + 73 + R) = 81 + S + R (v2.28 AL6). BY READING arithmetic.

**END (kind 0x0012, MUST)**, in `j-<seq>.end` after its HEADER, payload 8 bytes:

| Field | Encoding |
|---|---|
| d | u64: a byte length of segment `seq` whose every byte was synced before this frame was written; always a frame boundary |

- The whole END file is 72 + 24 = **96 bytes**.
- It is created beside its segment with `d` = 72 (the HEADER alone: 16 + 56), synced, and named by the same `sync_dir` as the segment (§3.2).
- After that the journal task rewrites it, open `Write` (which truncates, core.md:240-241) then `sync`, with `d` = the segment's synced end:
  - after a group's sync when the queue was found empty (idle);
  - else after every 64th group since the last END write;
  - and when the segment is rotated or the journal closes.
- An END write is never on an ack's path: the ack is sent before it. It costs one extra sync when idle, or one per 64 groups under load.

**Keys in trees.**

| Tree | Key | Value |
|---|---|---|
| data, table rows | table id u32 BE ‖ `Key[K]` bytes | schema number u16 LE ‖ the `Codec[V]` bytes under that schema (D1) |
| data, index entries | index id u32 BE ‖ `Key[J]` bytes ‖ `Key[K]` bytes | empty. A `put` whose old and new index keys are byte-equal writes no entry (J1). |
| streams | stream name | position u64, digest 32 bytes (the `RequestDigest`, fixed size, no length; AH1), state u8 (1 admitted, 2 replied), reply bytes when replied |
| catalog | 1 ‖ table name, or 2 ‖ table id u32 BE ‖ index name | kind u8, id u32, owner u32, schema epoch u64, key schema bytes, value schemas (u16 count; each: number u16, fingerprint u64, schema bytes; ascending, retired numbers absent, next number u16), by-identity bytes, revision bytes (empty = absent) |

**Initial values (orchestrator, 2026-11-16 02:31, from store-build-1010's question; reversible until S2 lands).**
- Catalog value `kind`: 1 = table, 2 = index, the same numbers as the catalog key prefix.
- Table and index ids come from the one `next_table` counter; the first id issued is 1, and 0 is never issued.
- A table's first value schema has number 0, and its next number starts at 1.
- A new table's schema epoch is 1; 0 is never a valid epoch.


The key codec must be order-preserving and prefix-free. That is part of S0's `Key[T]` contract (§7).

### 2.5 Pack `p-<seq>`: HEADER(role 3), then NODE frames, children before parents

**NODE (kind 0x0020, MUST)**, payload:

| Field | Encoding |
|---|---|
| id | u64, non-zero |
| level | u8 (0 = leaf) |
| zero | u8 |
| count | u16, 1–32 |
| leaf entry | key bytes, value bytes |
| branch entry | child id u64, low key bytes (entry 0's low key is empty) |

**Node ids.**
- Ids come from one writer-owned counter that every branch shares, so ids never collide across forks. This is 998's "one last-issued u64 counter across branches" (followups, 998 → store-spec-1001).
- A parent is always made after its children, so every child id is lower than its parent's id. A branch entry naming an id that is not lower, or not present in any listed pack, is `Dangling(id)`.
- Consumed ids are not rolled back on `Capacity`. A gap in ids is harmless.

**Pack size (H1).** A NODE goes into the open pack unless it would take the pack past `pack_bytes` and the pack already holds a NODE; then the pack closes first. A NODE is never split. So a pack file is at most max(`pack_bytes`, 72 + 16 + NODE payload limit), which is max(`pack_bytes`, 33,686,116) at the fixed limits. A pack holding one NODE larger than `pack_bytes` is legal; compaction treats it as any other pack (its live bytes are all or nothing, so it is retired, never copied forward).

**Recovery builds nodes in ascending id order.** That makes the decode total and acyclic, and it lets copy-forward (§3.5) move a child into a newer pack than its parent.

### 2.6 MANIFEST: HEADER(role 1), MANIFEST and FENCE

A reader walks every frame from offset 72 to the end of the file. Exactly one is MANIFEST. At most one other frame is FENCE. Unknown frames follow §2.7 rule 4: an unknown MUST = 0 frame is skipped, before or after the MANIFEST frame; an unknown MUST = 1 frame is `Unsupported(kind)`; a second MANIFEST, a known kind of another role, or no MANIFEST is `Corrupt` (V1). Every role reads its file the same way. The file is read with a bound of 68,157,548 bytes: HEADER, one MANIFEST frame at its limit, one FENCE frame, and `UNKNOWN_ROOM` (1 MiB) for frames of unknown kinds.

**MANIFEST (kind 0x0030, MUST)**, payload:

| Field | Encoding |
|---|---|
| generation | u64 (equals the HEADER's seq) |
| limits | key u32 = 4096, value u32 = 1,048,576, transaction u32 = 16,777,280 (the COMMIT payload limit, U1), fan-out u16 = 32 (the most entries per node). Fixed in format 1; any other value is `Corrupt(Malformed)` (V5). |
| next_node | u64: every node in a listed pack has a lower id |
| next_branch | u32: branch ids are never reused |
| next_table | u32 |
| next_pack | u64: every listed pack has a lower seq; the next cut writes from here. Any `p-x` or `p-x.tmp` with x ≥ next_pack is a leftover of an unpublished cut (H2) |
| first_live | u64: the journal resumes at `j-<first_live>` |
| prev_first | u64: segments `[prev_first, first_live)` and their `.end` files may remain and are removed at open |
| packs | u32 count; each: seq u64, nodes u32 |
| retired | u32 count; each: pack seq u64. Packs the previous manifest named and this one does not; removed at open. |
| branches | u32 count; each: id u32, name str, parent u32 (the branch the fork's Snapshot was taken on; 0 for main; may name a deleted branch, and is compared, never resolved, AA1), forked_at u64, version u64, at u64, data root u64, streams root u64, catalog root u64 (root 0 = empty tree). Ascending by id; main is id 0 and always present. |
| pins | u32 count; each: name str, branch u32, version u64, three roots. Ascending by name. |

- Lists out of order are `Order`.
- A root naming no decoded node is `Dangling`.

**Nothing needs a directory listing.** The host has none (BY READING host.hirsel). The manifest names every live file. Safety does not depend on finding leftovers (H2): a leftover pack has seq ≥ `next_pack`, open never loads it, and the next cut that reaches its seq replaces it before any manifest lists it. Open removes what it can find from `next_pack` upward, and the segments and packs named by `prev_first` and `retired`; a pack beyond a gap waits for a later cut or open. That costs at most the packs of one unpublished cut in disk space.

### 2.7 Kinds, limits and the rule for new kinds

| Kind | Name | Where | MUST | Payload limit | Status |
|---|---|---|---|---|---|
| 0x0001 | HEADER | first frame of every file | 1 | 56 | v1 |
| 0x0011 | COMMIT | j-* | 1 | 16 MiB + 64 = 16,777,280, the transaction limit itself (U1) | v1 |
| 0x0012 | END | j-*.end, after HEADER | 1 | 8 | v1 |
| 0x0020 | NODE | p-* | 1 | 32 × (4096 + 1 MiB + 16) + 12 | v1 |
| 0x0030 | MANIFEST | MANIFEST | 1 | 64 MiB | v1 |
| 0x0040 | CLAIM | j-*, right after HEADER | 1 | 64 | reserved, row 11 |
| 0x0050 | AUTHORITY | MANIFEST, after MANIFEST | 1 | 64 KiB | reserved, row 12 |
| 0x0060 | PRINCIPAL | — | 0 | — | retired by v2.26 AJ4 and never reused: the caller is the stream key's prefix |
| 0x0070 | FENCE | MANIFEST | 1 | 4 | v1 |
| any other | unknown | anywhere after HEADER | 0 | **`UNKNOWN_LIMIT` = 64 KiB** (C3) | skipped |
| any other | unknown | — | 1 | — | `Unsupported(kind)` |

**FENCE.** Its payload is one little-endian `u32`, the cumulative opener
obligation level. Absence identifies an unfenced store. Zero, a duplicate, a
wrong length or MUST = 0 is `Corrupt(Malformed)`. A level above the supported
level returns `Unsupported(0x0070)` at open step 2. The first runner installation
sets level 1 in its activation cut. Later cuts keep or raise the level, including
reverse migrations. `fence_level(db)` returns the selected level, zero when
absent.

**The rule for adding a kind without a version.**
1. A new kind takes the next unused number. 0x0002–0x00FF stays v1 space; nothing is ever renumbered.
2. Its MUST bit is 1 exactly when a reader that skips it would produce a different state or a different answer to any API call.
3. A MUST = 0 kind's payload is at most `UNKNOWN_LIMIT`, because that is all an old reader will accept. A kind that needs more must be MUST = 1. In MANIFEST, pack and END files, all frames of unknown kinds together take at most `UNKNOWN_ROOM` = 1 MiB, heads included, because those files are read with a fixed bound (V1).
4. A reader that meets an unknown kind:
   - with MUST = 0, checks the length against `UNKNOWN_LIMIT` before allocating, checks its check, and skips it. Over the limit, it is a bad frame (§3.4: `Corrupt`, or the torn tail at or above `d`).
   - with MUST = 1, refuses the open with `Unsupported(kind)`. An old library can never misread a newer store.
5. A known kind's payload never grows: decode is exact. Adding fields means adding a kind.
6. The format number changes only for a change no kind can express, such as the frame head itself. The scope ruling allows no such change, so it stays 1.

### 2.8 What is checked on read, and what is not

**Checked** (outside input: disk bytes, host rule):
- every frame's check (xxh3-64 bound to identity, file and offset), length and flags;
- exact payload decode;
- header fields against the file name and the manifest's identity;
- version continuity per branch;
- that every node id a branch entry or root names exists and is lower than its parent's;
- the manifest's list order;
- UTF-8 in names;
- that each segment's bytes reach its END `d`.

**Not checked:**
- B-tree invariants, meaning key order inside and across nodes, separator bounds and fan-out minimums;
- duplicate ops in a COMMIT.

Reason: these bytes passed a 64-bit check bound to their place, so they are the bytes our writer produced there (up to the 2⁻⁶⁴ of E4). A wrong order would be our own writer's bug, and the host rule forbids guarding against that. This is Goose's line, "safety, not integrity" (§10). Safety still holds: decoding is total, a wrong order gives a wrong but well-formed tree, and nothing reads out of bounds.

### 2.9 Rows 11 and 12: reserved now, and what each waits for

| Row | Reserved now | Waits for |
|---|---|---|
| 11 claims and epoch fencing | HEADER `epoch` (0); kind CLAIM (epoch u64, holder 16 bytes, lease_ms u64, MUST); `Error` gains `Fenced(u64)` when built | A second writer that can take over while the first is alive but cut off. Locally `try_lock` on LOCK is released when the process dies, so there is nothing to fence. It needs a remote authority (Net, M5a) or object storage with conditional writes, and a lease clock. Neither exists in host.hirsel (BY READING). |
| 12 object authorities and durability levels | HEADER `authority` (0); kind AUTHORITY (authority u8, level u8, locator bytes); `Options.authority` with the single variant `Directory` | Core object-storage operations: put-if-absent, get and list. A durability level above a local sync, such as replicated, needs a second authority. The output gate is already covered locally: replies are released after `durable` (§1.8). |

---

## 3. Durability and recovery

### 3.1 Commit sequence (one transaction)

1. `run` drains the journal ack channel with `try_receive` until `Empty`, applying each ack (Y1), checks for poison, takes `at`, and builds `Tx` over the branch head: views of the three roots, empty working copies that share them, an empty op list, and a Log view.
2. The body runs inside `isolate`. With `steps = Some(n)`, the isolate's argument is `bound steps n { make the Tx (step 1's build); body(mut tx); step 3's encode }`. A stop returns `Stopped(OutOfTime { steps: n })` with nothing written, published or sent; a mixed chain leaves the scope and is `Failed` (AC1). `put` and `delete` path-copy the working tree, using new node ids from the Db's counter, append to the op list and maintain the indexes. Limit checks happen here.
3. If the body returns `Ok` and the store has a journal, the COMMIT frame is encoded; a `memory` store encodes no frame (Q2). Then the resident account is checked (§1.11). A `Capacity` failure or a passed limit gives `Refused(c)`, and the frame is dropped. The size limit `Limit(Transaction)` was already enforced during the body by arithmetic on the COMMIT payload size against 16,777,280 (U1), so both modes refuse the same transactions by size (L29).
4. **Publish point:** the branch head becomes the working roots at version + 1. The commit is now visible. If the branch is followed (§1.5), `latest.publish` of its new Snapshot follows at once; it never waits and never fails.
5. **Handoff:** `send(frame, seq)` on the journal channel (capacity 128). It waits only when the queue is full (§3.3). If the commit triggers a cut, that cut's step 0 then waits for this frame's ack (§3.5, X1).
6. `run` returns `Committed(Done{value, ticket: {store, open, Journal(seq)}, info, reads})`.

**The journal task, per group** (§3.3):
1. Drain: one `receive`, then `try_receive` while the group is under 64 frames and 4 MiB.
2. `host.write` the frames, in the order received.
3. `host.sync` the segment.
4. Send `Synced(last seq, end)`.
5. **Barrier: no ack before the sync returns `Ok`.**
6. END write when due (§2.4).

`durable(Journal(s))` receives acks until the acked sequence reaches `s`. Every frame up to `s` is already queued, so it hands nothing over.

### 3.2 Barrier table

| Step | Barrier | Reason |
|---|---|---|
| group commit | write, then `sync(segment)`, then ack | a ticket is durable only after the bytes are |
| END write | ack first; then write END(d) with `Write`, then `sync(end file)` | `d` never names an unsynced byte; it never delays an ack |
| segment creation (at open and at each cut) | write HEADER, `sync(segment)`, write END(72), `sync(end file)`, `sync_dir`, *then* the first frame is sent | a lost directory entry would lose acked frames (ALICE, §10); a durable END name implies a durable header |
| checkpoint | pack: write `.tmp`, `sync`, rename, `sync_dir`; manifest: write `.tmp`, `sync`, rename, `sync_dir`; *then* remove retired files | the manifest must never name a missing file, and a removed file must never be named by a durable manifest |
| open | `sync_dir` after selecting or creating MANIFEST (O1), before any removal. `sync` each live segment before reading it | O1 makes the selected manifest durable before cleanup can remove files the previous manifest needs. The segment sync prevents recording an unsynced page-cache tail as `prev_end`. |

**Syncs:**

| Operation | Syncs |
|---|---|
| a group of commits | 1, plus 1 for END when the queue is idle or on every 64th group |
| a checkpoint | 7: segment file, END file, directory, pack, directory, manifest, directory |
| a checkpoint with no new nodes | 5: there is no pack |
| an open | one extra `sync_dir` for O1, in addition to the other open barriers |

### 3.3 Group commit with `try_receive` (C7)

**One FIFO queue.**
- The channel carries `Msg = Frame(Bytes, u64) | Rotate(HostFile, HostFile) | Barrier`, with capacity 2 × `group_commits` (128 with the default; W1).
- A second channel, `written` (capacity 2), carries `Written | Failed(IoError)` from the journal task to the Db, for `recorded` only (W2). The Db holds its Receiver and one Sender, and the journal task holds the other Sender.
- The writer is its only sender, and it sends in commit order. A `Rotate` is sent in the same order, after the last frame of the old segment.

**The journal task's loop:**
```
loop {
  let first = receive(rx)                 // Closed: nothing queued and the sender is gone → finish
  group = [first]
  while group.frames < group_commits and group.bytes < group_bytes {
    match try_receive(rx) {
      Ok(Frame(..)) => push
      Ok(Rotate(..)) => stop draining; rotate after this group
      Ok(Barrier) => stop draining; answer after this group's write
      Empty => idle = true; stop draining
      Closed => closing = true; stop draining
    }
  }
  write(group)                                          // an empty group writes nothing
  if answering { try_send(written, Written) }           // write barrier, before the sync (W2)
  if group has frames { sync; send Synced(last seq, end) }   // nothing is acked before its sync; a frameless group syncs and acks nothing (X1)
  if idle or groups_since_end == 64 { write END(end); sync END }
  if rotating { write END(end); sync END; close the old segment; switch to the new one }
  if closing { write END(end); sync END; end the task }
}
```

**The rules:**
- **Bounded drain.** One write takes at most `group_commits` frames and `group_bytes` bytes, except that a single frame larger than `group_bytes` goes alone. Both bounds are at least 1 (W1). `Barrier` and `Rotate` are not frames and count against neither bound.
- **Barrier (W2).** The journal task answers a `Barrier` with `Written` once every frame queued before it has been written to its segment. It answers before the sync, so `Written` means written, not durable. On any failure, before it ends, it also does `try_send(written, Failed(e))`.
- **Close rule.** 907's `try_receive` must return `Empty` when nothing is queued and a sender lives, and `Closed` only when nothing is queued and every sender is dropped. So every frame sent before `close` drops the Sender is written, synced and acked before the task ends. A `receive` that returns `Closed` ends the task the same way, with nothing left to write.
- **Group membership affects only I/O.** Which frames share a write and a sync depends on the schedule, and on nothing else:
  - the frames are written in the order sent, which is commit order;
  - no group boundary is written;
  - an ack covers a prefix;
  - replay reads frames in file order and checks each branch's version continuity.

  So the reopened state and every API answer are the same whatever the grouping, and the model checks this at every cut (§3.9, families `commit` and `group`).
- **The ack channel (Y1).** The journal task sends `Synced(seq, end)` and `Failed(e)` on its own channel, which has capacity 3 × `group_commits` + 3 (195 with the default). If `channel.new` refuses it, `open` returns `Full(c)`. Before every `send` of a `Frame`, the writer takes acks with `try_receive` until `Empty`. With that rule the journal task's ack `send` never waits (CHANGES-v2.15 Y1), so a `recorded` call waiting on `Written` always gets an answer.
- **Backpressure, not a second buffer.** When 128 frames are queued, the writer's `send` waits. There is no writer-side group buffer, so no frame is ever stranded waiting for the writer's next call.
- The one-slot `try_send` fallback of v1 is deleted.

### 3.4 Recovery, step by step (`open`)

0. **Options.** `group_commits` = 0 is `Missing("group_commits")`, and `group_bytes` = 0 is `Missing("group_bytes")`. Nothing is opened or created (W1).
1. **Lock.** Open `LOCK` (Write) and `try_lock` it. If held, poll the same handle up to the monotonic deadline given by `Options.lock_wait_ms`, then return `Held`. The default zero performs one attempt.
2. **Manifest.**
   - Open `MANIFEST` and read the whole file.
   - If it is missing and `create` is set, run genesis:
     - draw the identity (§2.3);
     - write `j-1` with HEADER and `prev_end` 0, sync it;
     - write `j-1.end` with `d` = 72, sync it;
     - `sync_dir`;
     - write MANIFEST generation 1, then the rename and its two syncs from the barrier table. Its fields: limits as §2.6; `next_node`, `next_branch`, `next_table`, `next_pack`, `first_live` and `prev_first` all 1; no packs, retired packs or pins; one branch, id 0, name `main`, parent, forked_at, version, at and the three roots all 0. CHANGES-v2.12 V4 gives its bytes for one identity.
   - If it is missing otherwise, the result is `NotFound`.
   - Decode HEADER, MANIFEST and any FENCE. A requirement the opener does not support ends the open before O1 or cleanup. A malformed frame is `Corrupt`.

**Step 2b. Directory barrier (O1).** After selecting or creating MANIFEST, call `sync_dir` while holding LOCK. If it fails, return `Io(e)` before any removal. A process crash after a cut's rename can leave the new MANIFEST visible in the page cache while the old one is still durable. O1 makes the selected MANIFEST durable before cleanup can remove any file needed by the old one.

3. **Leftovers** (best effort; H2). Remove `MANIFEST.tmp`. For x = `next_pack`, `next_pack + 1`, …, remove `p-x` and `p-x.tmp`, stopping at the first x where both are NotFound. Then remove every retired pack and the segments `[prev_first, first_live)` with their `.end` files. NotFound is ignored. Any other failed `remove` ends the open with `Io(e)` (V6), and no `sync_dir` follows as part of cleanup. A power cut here can keep any subset of the removals. O1 has already made the selected MANIFEST durable, and a leftover with seq ≥ `next_pack` is replaced before any cut lists it.
4. **Packs.**
   - Read each listed pack whole (there is no seek, BY READING host.hirsel), bounded at 72 + nodes × 33,686,044 + 1,048,576 bytes from its manifest entry (V2, with `UNKNOWN_ROOM` from V1), never by `pack_bytes`. Check its HEADER, then decode every NODE into a map from id to node bytes.
   - The node count must equal the manifest's entry for that pack.
   - Build `Arc[Node]` values in ascending id order. Then attach the branch and pin roots.
5. **Journal segments.** For `n = first_live, first_live+1, …`, open `j-<n>` until NotFound. Sync each, then read it whole. A **bad frame** is one that is short, has a bad head (kind 0, flags, a length over its limit or over `UNKNOWN_LIMIT`), or has a bad check.
   - **Middle segments.** For each segment except the last, the next segment's HEADER gives its valid end, `prev_end`. A bad frame starting below `prev_end` is `Corrupt`. Bytes from `prev_end` on are ignored: they are the torn tail an earlier open found.
   - **The last segment.** Read `j-<n>.end`. If its HEADER (role 4, seq n, identity) and END frame are both good, `d` is known; otherwise there is no `d`.
     - A segment shorter than `d` is `Corrupt(Truncated)`: synced bytes are missing.
     - A bad frame starting below `d`, or a frame starting below `d` and ending past it, is `Corrupt`.
     - From `d` on, or from the HEADER on when there is no `d`, recovery reads until the first bad frame. That is the torn tail, and recovery stops there.
   - **A bad last header.** If the last segment's HEADER is short or bad:
     - and its END file is good, the result is `Corrupt(Header)`. A durable END name implies a durable header (§3.2).
     - and its END file is missing or bad, recovery treats the segment as absent: it was a crash during segment creation. The segment before it then becomes the last one and follows the rule above. Its own END file gives its `d`.
   - **What a damaged END file costs.** Only detection. A flip in the END file, or an END not yet rewritten, falls back to the torn-tail rule over a longer region. It never makes a good store refuse to open.
6. **Apply.** Apply COMMIT frames in order. The branch must exist, else `Corrupt(Gap)`. The version must be the head + 1. New nodes take ids from `next_node` upward.
7. **Account.** Compute the resident account (§1.11). Past `resident_limit`, the result is `Full(Capacity)`.
8. **Rotate.** Create the next segment, `j-<last+1>`, and its END file, following the barrier table (`Write` replaces a torn-header file). Its HEADER's `prev_end` is the valid end found in step 5.
   - All new frames go there. No process ever appends after a byte it did not write, so there is no truncate operation and none is needed.
   - SQLite resets a reused WAL with salts. Rotating instead makes salts unnecessary.
   - The new segment's number is this open's number (§1.12).
9. **Streams.** Every stream in state `Admitted` is marked, in memory, as admitted by an earlier open. A later `admit` of that position re-commits it and gets `Abandoned(t)` (§1.8).
10. **Tasks.** Spawn the journal task, owning the new segment's and END file's `HostFile`s (RUN `send_kinds`, `db_task`), and the checkpoint task, owning `Dir` and the on-disk node table rebuilt in step 4.

**The residual this rule leaves** (stated, two faults; damage case D6): suppose a byte flips in a frame that was synced during a busy window not yet covered by an END write, and then the process crashes before the next END write. Recovery reads that frame as the torn tail, and it may drop acked commits after the flip. The window is at most 64 groups, and it closes at the next idle moment. A single fault, a flip or a crash alone, is always handled exactly.

### 3.5 Checkpoints, compaction and GC (incremental only)

**The cut is taken by the writer, inline.** This is the only part of a checkpoint that waits in the writer's task.
0. **Wait for the old segment's last ack (X1).** Until `journaled` ≥ the sequence of the last frame the writer has sent, receive acks as `durable(Journal(s))` does. This is a condition, so it holds at once when nothing is outstanding. At open both numbers are the recovered head, so a cut right after open waits for nothing. On `Failed(e)`, poison (§3.8) and stop: nothing is created, no `Rotate` is sent, no Cut is taken, and the call ends as it does for a failed host call in step 1 (V6).
   - **Why:** creating `n+1` makes its `prev_end` durable, and §3.4 step 5 then requires every byte of `n` below it. A frame still queued is not on disk, so a crash would leave `n` shorter than `prev_end`, which is `Corrupt(Truncated)`.
1. Create segment `n+1` and its END file, following the barrier table. Segment `n+1`'s `prev_end` is segment `n`'s end as the writer counts it. Step 0 made that end synced. Nothing has been sent since, because the writer is the only sender.
2. Send `Rotate(segment, end file)` on the journal channel, after the last frame of segment `n`. The journal finishes its group, writes segment `n`'s final END, closes it, and switches. A `send` that returns `Closed` means the journal task has failed after step 0's ack: receive its `Failed(e)`, poison, and take no Cut. `n+1` already exists and its `prev_end` is synced, so the store reopens exactly.
3. Take the Cut, **at this moment**: the generation, `first_live = n+1`, the previous `first_live`, the Arc roots of every branch and pin, `next_node`, the counters, and any schema commit's new roots. Send it to the checkpoint task.
   - The model found that a Cut taken later, at manifest-write time, replays the commits made after the rotation twice (§3.9). That is a real ordering rule, not a convenience.
4. Commits continue into `n+1` while the checkpoint runs.

**The checkpoint task's per-node record.** For every node in a listed pack it holds the pack sequence, the byte size, the parent-reference count and an Arc share of the node. The shares keep no extra memory alive: every live on-disk node is reachable from a root the Db holds. A dead node's share is dropped when its count reaches 0.

**Per cut:**
1. **Write.** Walk each root. Skip any subtree whose root id is already in the table: that subtree is already on disk, because its children are older. Emit the remaining nodes children first into packs numbered `next_pack`, `next_pack + 1`, … of the manifest this cut starts from, each written as `p-<seq>.tmp` with `Write` (which truncates any leftover). A NODE that would take the open pack past `pack_bytes` closes it first, unless the pack holds no NODE yet (H1). As each pack closes it is synced and renamed onto `p-<seq>`, replacing any leftover of that name (H2).
   - **Incremental cost:** after k one-key commits, at most k(2h+1) nodes are new, where h ≤ 1 + ⌈log16 ⌈n/16⌉⌉ (L5). The old store rebuilt the whole tree, at 372.1M instructions and 13.2 MB for 100k rows (STORE-RECORD.md, store-prior-art figures).
2. **Count.**
   - Increment the children of each new node and the new roots.
   - Decrement the old roots and cascade every count that reaches 0 to that node's children. This is the reference count of the on-disk graph.
   - Each pack keeps a total of its live bytes.
   - The sum of live accounted bytes goes to the writer with the ack (§1.11).
3. **Compact.**
   - A pack whose live bytes are 0 is retired.
   - A pack whose live bytes are below half its bytes is copied forward: its live nodes are added to the new pack with the same ids, and the old pack is retired.
   - Disk stays at most about 2 × the live bytes (GUESS), against Noms' conjoin, which is triggered by table count (§10).
4. **Publish.** One `sync_dir` makes every pack rename durable (each pack was synced before its rename). Then the manifest, whose `next_pack` is one past the last pack this cut wrote: `retired` lists the packs dropped, `prev_first` is the previous `first_live`, and its `sync`, rename and `sync_dir` follow.
   - The commit point is the rename. After a crash the directory holds the old manifest or the new one, and each names only files that exist.
5. **Remove** the retired packs, and the segments with their END files, ignoring NotFound. A crash before this step leaves them named in `retired` and `prev_first`, and the next open removes them.
6. **Ack** `Checkpointed(generation, live)` to the writer on the checkpoint channel. `durable(Cut(g))` waits for it. The ack makes durable exactly the Cut's version; later commits become durable through their own groups.

**A cut refused with `Capacity` (Q1).**
- Every allocation of a cut comes before its MANIFEST rename: pack buffers, records, count updates, the manifest buffer, the removal names and the ack value. After the rename the task makes only host calls and the ack send.
- The per-node table is a persistent `OMap`. A cut builds a new version of it and the task installs that version only after the MANIFEST's `sync_dir`.
- On `Capacity` before the rename, the task drops the cut's table version, its buffers and its Arc roots. Packs already written stay as leftovers at seq ≥ `next_pack` (H2). It syncs nothing more and keeps no pack counter of its own, so the next cut starts from the manifest's `next_pack`. It acks `Refused(g, c)` and waits for the next Cut.
- The durable state is the old manifest plus the rotated segment, which `prev_end` chains, so every commit stays durable through the journal.
- An operation made by a cut (`checkpoint`, schema commits, `migrate`, `pin`, `unpin`, `fork`, `adopt`, `delete_branch`) publishes only on its `Checkpointed` ack. On `Refused` it returns `Full(c)` and changes nothing, and `durable(Cut(g))` returns `Full(c)`. `run` settling for I4 returns `Refused(c)`.
- `Capacity` never poisons.

**Triggers:**
- the journal since the last cut exceeds `checkpoint_bytes`, counted from the last cut taken, refused or not, so a refused background cut is retried after another `checkpoint_bytes`;
- `checkpoint`;
- any schema, pin or fork operation.

At most one cut is in flight. A trigger that comes during one waits for it.

### 3.5a Install a prepared release

**E1–E3.** Under LOCK, settle the checkpoint and journal, then capture the
MANIFEST bytes, the durable main version, next table id, and the acked end of the
terminal segment. Lease the listed packs, the journal prefix and its END files
for the attempt. A task writes a fresh scratch directory, copies the captured
bytes through that terminal offset, then writes and syncs `EXPORT` and syncs the
scratch directory. Scratch errors refuse the attempt and leave the live store
healthy. A cut carries forward leased removals in `retired` and `prev_first`.
Once the lease ends, the next cut or open can remove them.

**R1 and R3.** A normal open refuses a directory containing `EXPORT`. The
prepare opener reads the captured MANIFEST and packs without LOCK, cleanup,
rotation, sync or workers. It recovers the journal prefix through `EXPORT.end`,
without using the terminal END. Its recovered version and identity must match
the export. The `PREPARED` file contains the same attempt and base, the target
Describe and release, target limits, catalog delta and replacement inventory.
The inventory lists sorted ids, including empty ranges and dropped ranges,
with a row count and digest. A schema-number append on an existing id keeps its
data range. Frame checks use §2.1. The scratch file roles are 5 for EXPORT and 6
for PREPARED, with kinds 0x0080 for the export stamp and 0x0081 through 0x0084
for the preparation metadata, catalog delta, inventory and rows.

**M1–M3.** Validate frame bounds, checksum, order, schema numbers, allocated
ids, and the exact correspondence between inventory, catalog delta, and rows.
Bind their SHA-256 to the attempt. Reserve an id block from the shared counter
before building trees and admit their exact row charge against OLD's account.
Build off-root and hold the latest import for that attempt. The cut names that
import's digest.

**V3–V6 and C1–C7.** After the runner pauses and settles admitted work, validate
attempt and digest, identity and main branch, head version, next table id, and
activation. Construct the final catalog and graft replacement ranges, issuing
parent ids after the reserved block. The target and its release row come from
PREPARED. Check both resident limits and the deadline. Use the existing
checkpoint order with the advanced counters, all current streams, branches
and pins. After the packs are durable and before writing `MANIFEST.tmp`, check
NEW's bounds against the exact resident account and actual listed packs.
Before the MANIFEST rename, a refused cut leaves OLD's roots. The rename is
the point of no return. Publish the new head and report its version only on
`Checkpointed`. A drift retry takes a fresh export after all writers settle.

### 3.6 What each crash cut leaves

| Cut | State after reopen |
|---|---|
| genesis, before the MANIFEST rename is durable | no store: `NotFound`, or create again (`j-1` and `j-1.end` are rewritten with `Write`) |
| commit, before `write` | the frame is lost. It was never acked; the prefix is kept. |
| commit, during `write` | torn tail above `d`. Recovery stops before it; later segments come after a rotation. |
| commit, after `write`, before `sync` | present or lost, never half: the check decides. Not acked. |
| after `sync`, before the ack is received | present. `durable` had not returned. |
| after `durable` returned | present (law L3) |
| END write or END sync | the old or the new `d`, or a bad END file (= no `d`). Each is at most the synced end, so every case reopens to the prefix. |
| segment creation, any step | the segment is missing, or has a bad header with no good END file, so it is treated as absent. No frame was sent into it yet. Every frame of the segment before it was synced before creation began (§3.5 step 0, X1), so if the new segment survives, its `prev_end` is covered. |
| pack `.tmp` written or synced, not renamed | a leftover with seq ≥ `next_pack`: never loaded; removed at open if it is found, else replaced by the next cut (H2) |
| pack renamed, before the cut's `sync_dir` | any subset of the cut's packs may survive under their names. No manifest names them; the same as the row above. |
| several packs written, crash before the MANIFEST rename | the old manifest, with leftovers `p-next_pack` … `p-(next_pack+m-1)`; the same as the rows above |
| power cut during open's leftover removal, keeping any subset of the removals (a gap is possible) | the manifest selected at open, made durable by O1. Leftovers beyond the gap stay. They are replaced before any cut lists them. Model H3 cuts every subset. |
| `MANIFEST.tmp` partial | removed at open. The old manifest stands. |
| power cut after MANIFEST rename, before the cut's `sync_dir` | either the old manifest (its files are still present) or the new one (its files were dir-synced first). Both are complete. |
| process crash after MANIFEST rename, before the cut's `sync_dir`, then power cut during reopen's cleanup | the manifest selected by the reopen, made durable by O1 before any removal. |
| O1's `sync_dir` fails | open returns `Io(e)` with both generations' files intact. |
| after the manifest's `sync_dir`, before removals | the new manifest; the leftovers are removed at open from `retired` and `prev_first` |
| a cut refused with `Capacity` at any allocation point, then a crash, keeping any subset of its pending directory operations | the old manifest; its packs are leftovers as in the rows above. Model Q1 cuts it. |
| `migrate`, `pin`, `fork` or `adopt` at any of the cuts above | exactly the old manifest or the new one, so the operation happened completely or not at all |
| first activation install at any cut above | the old generation, or the new catalog and activation with its FENCE and release row. This also holds at empty-activation genesis. |
| export, prepare, import, or a refused install before MANIFEST rename | the live MANIFEST and journal still select OLD, and scratch is outside recovery |
| during a prepared install MANIFEST rename and directory sync | a complete old or new generation with its own catalog, activation and counters |
| prepared install loses its ack after directory sync | NEW's durable generation selects NEW |

All of these cuts, and the damage cases, run in the model (§3.9).

### 3.7 Cancellation at each wait (C5)

**Settle wait in `run` (I4).** It comes before publish. A cancel there drops the transaction and changes nothing, like a cancel before `run`.

**What a cancellation is.**
- CONC-7 (tasks.md:111-122): a cancellation is observed only at a core `waits` operation's entry or its wait, and observing it "is a failure with cause `Cancelled` …, which runs the task's cleanup".
- FAIL-16 (failure.md:260-265): `isolate` does not catch it.

**What follows for the store:**
- No store call returns after a cancellation, so no `Error` case is needed.
- The cancelled task's cleanup cleans the `Db` (§1.10): CONC-8 cancels both store tasks and waits, shielded; issued operations complete (CAP-8); queued frames are dropped (CONC-9); LOCK closes.
- The store is then exactly at a crash cut of §3.6, and reopen recovers it.

| Waiting call | Wait inside it | What the cancellation leaves |
|---|---|---|
| `open` | host operations of steps 1–8 | nothing published. LOCK closes in cleanup. A partly created segment or END file is treated as absent, or as genesis not yet done, at the next open. |
| `run`, `run_on`, `admit` | the handoff `send`, when 128 frames are queued; when the commit triggers a cut, the waits of the cut row below | the commit was published in memory but never queued. It dies with the Db: crash cut "commit, before write". It was never acked. |
| `durable` | `receive` of acks | every frame up to the ticket was queued. An issued write or sync completes; the rest are dropped. Crash cut "after write, before sync" or earlier. |
| `checkpoint`, `pin`, `unpin`, `fork`, `adopt`, `delete_branch`, schema commits | `receive` of journal acks in step 0 (X1); segment and END creation, sync and `sync_dir`; the `send` of `Rotate`; `receive` of the checkpoint ack | crash cut "segment creation", or the checkpoint cut the checkpoint task stood at: the old manifest or the new one |
| `close` | the ack wait of step 1, the joins of step 3 | the remaining steps happen in the Db's cleanup: the tasks are cancelled and joined shielded, and LOCK closes. The cancelled close returns nothing; the state is the crash cut at that moment. |
| journal task: `receive`, `try_receive` (never waits), `write`, `sync` | — | reached only through the Db's cleanup: the crash cut at that step |
| checkpoint task: any wait | — | the crash cut it stood at, in §3.6 |

The body of `run` never waits, so a transaction cannot be cancelled halfway through its body or its publish.

### 3.8 A failed write or sync

- **No retry.** A failed `fsync` may already have dropped the dirty pages. PostgreSQL's 2018 fsync failure is known to me, not opened. SpacetimeDB panics (commitlog.rs:123-140).
- **Poisoning.** On a `WriteError` or a failed `sync` or `sync_dir`, and during a cut on any failed host call: `open`, `rename`, and a `remove` failing with anything but NotFound, including the removals after a MANIFEST rename (V6). Open's own host calls, even when the checkpoint task runs them, end the open with `Io(e)` instead:
  - The journal or checkpoint task sends `Failed(e)` and ends, which drops its receiver.
  - The writer learns of it either from that message, or from `Closed` on its next `send` (CONC-4).
  - It stores the first `e`. From then on every Db call except `close`, `identity` and `snapshot` returns `Poisoned(e)` (ruling 20:12). Snapshots stay readable.
  - A failed END write poisons too. Its bytes are not needed for safety, but a sync failure means the file system can no longer be trusted, and one rule for every failed sync is simpler than two.
- **`close`** still runs every cleanup step and returns `Poisoned(e)` (§1.10).
- **`Capacity` is not an I/O failure.** A cut refused with `Capacity` is dropped and acked `Refused(g, c)`. The task stays alive and nothing is poisoned (§3.5, Q1). The journal task allocates nothing after spawn (Q1a), so it has no `Capacity` case.
- **Reopening** goes through §3.4, which syncs each segment before reading it.
- **Known limit, GUESS:** on Linux, pages whose writeback failed may stay readable in the page cache after the error. A reopen in the same process could then read commits that never reached the disk. The protection is that a poisoned Db's own acks never claimed them durable. Reopening in a new process after a host reboot is exact.

### 3.9 The executable model (C2), RUN

**Packs (v2.5, H3).** `model/model_v25_packs.py` covers pack naming, open's leftover removal and the one-NODE oversized pack. A process crash leaves pending directory operations visible. A power cut can keep **any subset** of them. L25(b) runs both crashes in sequence. RUN: **155 states, 7,937 crash cuts, 0 failures.** All six mutants are caught, including `no_open_sync`. [O1 run](../store-o1-1405/REPORT.md)

**A refused cut (v2.9, Q1).** `model/model_v29_refusal.py` adds the node graph, its reference counts and the checkpoint task's table to the pack model's file system, which keeps any subset of pending directory operations at a crash. A cut is refused at each of its 15 allocation points. A crash is cut right after the refusal, under every subset, and then a retried cut runs with a crash after every step. Checks: listed packs hold their cut's content (P1); leftovers only at seq ≥ `next_pack` (P3); every node reachable from the manifest's root is in a listed pack (P5); after every cut, refused or completed, the task's table equals the one rebuilt from the manifest (P6); and the retried cut completes (L). RUN: **7,394 crash cuts, 0 failures.** Mutants `inplace_counts`, `advance_next_pack`, `alloc_after_publish` and `refusal_ends_task` are all CAUGHT (`model-v29-refusal-run.txt`).

**What the model is.**
- `model/model_v2.py` is the reviewer's `model.py` changed to the v2 rules.
- The file system keeps written, synced and directory-synced state apart. The W2 power-cut cases keep only synced bytes under synced names (`test_provider.rs::crash`). The checkpoint family also crashes the process after MANIFEST rename, reopens with the rename still visible and pending, then cuts power during cleanup with each subset of its removals persisted. The `no_open_sync` mutant makes the old MANIFEST name a missing pack.
- Each scenario is a list of the primitive steps of §3 (write, sync, END write, rotate, rename, sync_dir, remove, ack), and a crash is cut after every step.
- **Oracle:** the reopened state must equal one of the states the history produced from the last acknowledged one onward (L3). The reviewer's cut checks always returned True; this one can fail.

**Cuts** (EXIT 0, [O1 run](../store-o1-1405/REPORT.md)):

| Family | Cuts | Superset of the reviewer's |
|---|---|---|
| genesis | 9 | new; NotFound is allowed until the first manifest is durable |
| commit | 7 | 5 |
| group | 6 | 4 |
| checkpoint | 37 | 12; includes a commit landing in the new segment while the checkpoint runs and 15 power cuts during cleanup after a process crash |
| compaction | 16 | 9; retires a pack |
| pin | 16 | 10 |
| fork | 16 | 10 |
| schema | 16 | 10 |
| open | 15 | 9; O1, a torn synced tail, rotation with `prev_end`, then commit X written, synced and acked (L10) |
| **total** | **138, FAILED=0** | 69 |

The `install` family exercises prepared deploy cuts and requests, including
drift, omitted inventory, candidate binding, advanced counters, and target
open limits. The compiled S3 crash sweep includes the `install` scenario.

**Damage cases** (13, FAILED=0):

| Case | Damage | Result | Rule |
|---|---|---|---|
| D1 | flip in the synced, END-covered last frame | `Corrupt` | below `d` (the review's disagreement, now fixed) |
| D2 | short last frame | ok, prefix | torn tail |
| D3 | full-length last frame with a zeroed payload above `d` | ok, prefix | why "a complete bad frame is corruption" was rejected |
| D4 | flip in a middle segment | `Corrupt` | below `prev_end` |
| D5 | flip in the END file | ok, prefix | detection lost only |
| D6 | flip in a frame synced in a busy window, not END-covered | ok, shorter prefix | the stated two-fault residual (§3.4) |
| D7 | synced bytes truncated below `d` | `Corrupt` | file shorter than `d` |
| D8 | unknown MUST=0 frame, 100 bytes | skipped, ok | §2.7 |
| D9 | unknown MUST=0 frame over 64 KiB | `Corrupt` | `UNKNOWN_LIMIT` |
| D10 | unknown MUST=1 frame | `Unsupported` | §2.7 |
| D11 | flip in a pack | `Corrupt` | packs are always synced |
| D12 | flip in MANIFEST | `Corrupt` | |
| L10 | commit X after a torn tail and reopen | X present | rotation |

**The oracle has teeth** (`model-v2-mutants.txt`, RUN). Each mutant is the model with one rule broken:

| Mutant | Caught by |
|---|---|
| M1 ack before sync | 2 cuts fail |
| M2 manifest renamed with no pack `sync_dir` | 5 cuts fail |
| M3 v1's rule: no END, a bad last frame is torn | 3 damage cases fail (D1, D7, D9) |
| M4 retired files removed (and dir-synced) before the manifest rename | 5 cuts fail |

**The model found one real bug in my first draft:** a Cut taken at manifest-write time instead of at rotation replays post-rotation commits twice. §3.5 step 3 now states the rule.

**What the model does not cover:**
- the in-memory tree;
- codecs;
- cancellation (the CONC-8 ≡ crash argument is by reading);
- multiple branches' version interleaving in one segment (§2.4 continuity, by reading).

L3, L4, L9 and L10 cover these on the real code with W2 and W4.

**Streams (v2.3, F11).** `model/model_v23_streams.py` models §1.8 and §1.3's abort rule on their own. It has one stream, positions 0–2, every interleaving of `admit`, `run` (ok, err with a reply, err without a reply, fault, refused), `sync`, and up to two crashes. Each crash keeps the synced prefix plus any longer prefix of the written frames. Its oracle is the rule text: S7 (latest q implies every p < q replied); an effect present iff the reply is ok; an acknowledged reply never lost; `Rejected(l)` for p < l only when p has a reply; `Abandoned`'s ticket of this open; and F2's one-frame abort with its ticket. RUN: **1,851 states, 2,396 crash cuts, 0 failures**. Mutants `no_s7`, `abort_keeps_data`, `abort_drops_reply` and `abort_no_ticket` are all CAUGHT (`model-v23-streams-run.txt`).
v2.4 (`model_v24_streams.py`) adds law G: every body records its call, and a frame has a call record iff it has a reply written by a run that ran an entry; the runner's reply-only `Failed(Gone)` commit for a removed entry is a run with a reply and no record (K3). The model does not generate gone replies, so its check needs no change. RUN: **1,851 states, 2,396 cuts, 0 failures**. `abort_drops_call` and `call_without_reply` are CAUGHT, with the four above (`model-v24-streams-run.txt`).
v2.21 (`model_v221_streams.py`) is `model_v24_streams.py` plus AE1–AE3: a new stream admits only 0 and answers `Rejected(0)` otherwise; a reply without its admit is `Missing("reply")`; and every admit move has a twin past `resident_limit` that answers `Full` and commits nothing. RUN: **1,851 states, 2,396 cuts, 0 failures** (the twins add no state). `new_any_p` (law A) and `full_after_commit` (law E) are CAUGHT, with the six above (`model-v221-streams-run.txt`). **Scopes (v2.21, AE4).** `model_v221_scope.py` is new. The store side compares encoded bounds; the oracle enumerates keys. Its key domain (every byte string of length 0, 1 or 2 over {0, 1, 2}) has a key in every byte gap, so it cannot see the gap case of (b); L14 holds that as a hand case. It covers two tables of 13 keys each, every scope of up to two entries, every span, `lookup` on one index, `union` of every pair, and `narrow` with a two-entry outer scope and one inner entry, or a one-entry outer scope and up to two inner entries. RUN: **112 entries, 2,686,600 checks, 0 failures, 4,620 narrows sound by key but refused by (e)**. Ten mutants are CAUGHT (`model-v221-scope-run.txt`).

---

## 4. Concurrency

| Task | Owns | Receives | Sends |
|---|---|---|---|
| caller (the writer) | `Db`: heads, node id counter, resident account, LOCK file, both Task handles and channel ends | acks, `Failed` (drained before every `Frame` send, Y1) | `Frame` and `Rotate` (one FIFO channel), `Cut` |
| journal | the current segment's and END file's `HostFile`s, one group buffer allocated at spawn; root limit 16,842,832 bytes | `Frame`, `Rotate` | `Synced(seq, end)`, `Failed(e)` |
| checkpoint | `Dir`, the on-disk node table (a persistent `OMap`), pack and manifest counters; root limit `checkpoint_task_bytes` | `Cut` | `Checkpointed(g, live)`, `Refused(g, c)` (Q1), `Failed(e)` |
| any reader | `Snapshot` and `Rows` values | — | — |

**One writer.**
- Within a process, a `Db` is not `send`, so it cannot leave its task. RUN: `db_task` holds `Task[u64]` in `Db`, and CONC-2 says task handles are never send.
- Across processes, there is `try_lock` on LOCK.
- A second `open` in the same process gets `Held`, from flock on a second descriptor (GUESS: Linux flock locks per open file description, so this holds on Linux; S2 tests it under L21).

**What is shared through `Arc`.** Every tree node (`Node: shareable + send`, RUN `snapshot_task`) and nothing else. There is no shared mutable state (CONC-12).

**What is sent:**
- `Bytes` frames (RUN `send_kinds`);
- `HostFile` (RUN);
- `Dir` (RUN);
- `Cut`, made of Arc roots and integers;
- plain acks;
- `Snapshot` and `Rows`, which the program sends itself.

**Order.**
- One channel carries frames and rotations in commit order.
- The journal writes in receive order, so file order is commit order.
- Group membership, which depends on the schedule, changes only how many syncs happen (§3.3).
- The checkpoint channel is separate, and nothing in replay depends on its timing: a Cut names its own `first_live`.

**What a reader can observe:**
- exactly one committed version of one branch;
- never part of a transaction, because publication is a single root swap in the writer's own state and the reader holds earlier roots;
- possibly a version that is not yet durable;
- nothing of later commits.

Readers and the writer never wait for each other. A reader's held Snapshot is a memory pin (§1.10), and it is not counted against `resident_limit` (§1.11).

---

## 5. Laws and red cases

**Harnesses:**

| Harness | Use |
|---|---|
| **M** | a model store in Hirsel: a sorted `List[(Bytes, Bytes)]` per branch, plus the list of acked tickets |
| **W2** | 907's crash provider, which enumerates every cut and loses unsynced data and unsynced directory entries. Has the ops of C14 since 1070c6a3. |
| **W4** | 907's scheduler |
| **F** | the frame corpus. Cases go into that existing harness, not into a test per frame kind. |
| **P** | the Python model (§3.9). It is the design check, not a test of the code. |

Every expected value comes from M, from hand-derived arithmetic, or from a published vector, never from the store under test.

| Law | Statement | Harness | The wrong implementation it catches |
|---|---|---|---|
| L1 model | for random sequences of `run`, `put`, `delete`, abort, fault, `range` and `lookup`, every read equals M | M | an off-by-one in `Excluded`/`Included` bounds; an aborted body's writes leaking into the head; an index entry not removed on overwrite |
| L2 snapshot | a Snapshot taken at version v, then 1,000 commits and a checkpoint: every `get` and `scan` on it equals M at v, on the writer's task and on a reader task; a reader on `follow(db, main)` under every W4 schedule reads versions that never decrease, each equal to M at that version, and reads the last commit's version after the writer's last publish | M, W4 | a path copy that mutates a shared node in place; a GC that frees nodes a Snapshot holds; a publish before the head swap (a reader sees a version the writer then refuses) or a skipped publish |
| L3 prefix | at every W2 cut, the state after reopen equals M at some version w, where w ≥ the last ticket whose `durable` returned, and w is a real commit boundary. The same holds under every W4 schedule of the journal's drain. | W2, W4, M | acking before the sync; replaying a torn or unacked frame as if whole; applying frames out of order; losing an acked commit; a replay that depends on group boundaries |
| L4 checkpoint cuts | at every W2 cut inside a checkpoint, the state after reopen equals M at the last durable version, with every pin and branch intact. X1 adds: the cut is taken while frames are still queued for the old segment (one frame larger than `group_bytes`, and a full group), and the store opens at every cut; a cut right after open, two cuts in a row, and a cut after only `recorded` calls each complete without waiting on an ack | W2, M | creating the next segment before the old segment's last ack (`rotate_before_ack`, caught by 1010's cut 20 as `Corrupt(Truncated)`); a step 0 that always receives (hangs on a cut right after open); renaming the manifest before the pack's `sync_dir`; removing a segment before the manifest is durable; leaking a `.tmp` below `next_pack` (the next open's file set must equal the manifest's, plus only `p-x`/`p-x.tmp` with x ≥ `next_pack`); a Cut taken after rotation's commits (§3.5) |
| L5 incremental | after k one-key commits on n rows, the nodes the checkpoint writes number ≤ k(2h+1), where h = 1 + ⌈log16 ⌈n/16⌉⌉ (hand-derived from 998's occupancy 16–32: a path copy per level plus one split sibling per level plus one new root) | count of NODE frames | a checkpoint that rewrites the whole tree, which is the old store's O(state) cost |
| L6 GC | after `unpin`, `delete_branch` or overwrites, and two checkpoints, every pack whose nodes are all dead is gone, and reopening gives M for every branch and pin | W2, M | a reference count that underflows on a subtree shared by a pin and the head; copy-forward dropping a live child |
| L7 held across compaction | pin at v, fork b at v, 10,000 commits on main and on b, 5 checkpoints with copy-forward, reopen: the pin equals M at v and each branch equals its M | W2, M | a mark from main only; a branch id mix-up in COMMIT frames; per-branch node counters that collide |
| L8 poison | inject a failed `sync` (of a segment, an END file or a pack) at step g: `durable` of tickets after it returns `Poisoned`, every later call except `close`/`identity`/`snapshot` returns `Poisoned`, and reopen gives a prefix ending before g, or including g if its bytes reached the disk | W1 injection, W2 | retrying the fsync, which is the fsyncgate bug; acking on failure; accepting a commit after the failure |
| L9 cancellation | cancelling the owning task at each wait of §3.7, then reopening, gives the same state as a crash at the same cut | W2, W4 | a lock left held after a cancelled open; a cancelled `durable` that leaves the Db usable; a handoff placed before publish |
| L10 torn tail, then reopen and write | crash mid-write, reopen, commit X, `durable`, crash, reopen: X is present | W2, P | appending to the torn segment, where X would then sit after garbage and be lost; a wrong `prev_end` |
| L11 unknown kinds | an injected unknown kind with MUST = 0 and payload ≤ 64 KiB anywhere is skipped and the state equals M; over 64 KiB it is `Corrupt` below `d`; with MUST = 1 the open is `Unsupported(kind)` | F, P | a reader that refuses forward-compatible kinds; a reader that ignores MUST; a reader that allocates an unknown length |
| L12 damaged bytes | flipping any one byte of any file gives `Corrupt` (except with probability about 2⁻⁶⁴, E4), and so does a valid frame of this store copied to another offset or another file (E3), except a flip in the last segment at or above its END `d`, or in an END file, which gives a state equal to M at a prefix. Never a different state, a crash or an allocation above a kind's limit. | F, M, P | a frame kind whose check is not verified; a check computed without the binding (a moved frame decodes); a length used before its bound check; a dangling child id that is not refused; a last-segment rule that ignores `d` (M3) |
| L13 streams | (the `admit` digest `d` is any fixed `request_digest` value unless a case names two) duplicate, gap, a reply without admit, and a crash after admit: exactly one effect per position when the effect is the transaction; `Abandoned(t)` after a crash between admit and reply, with `t` of this open, and again on a second `admit` after a run that aborted without a reply, failed or was refused, with no new commit; an admit of latest+1 while latest is unreplied is `Rejected(latest)` with no commit (S7); a body that replies then returns `Error` leaves `Replied(p)` and no data change, in one frame, and `Undone.ticket` names it; AE1: on a new stream `admit(s, 1, d)` is `Rejected(0)` with no commit, then `admit(s, 0, d)` is `Admitted`; AE2: `reply` at a position not admitted is `Missing("reply")`; AE3: with `resident_limit` one byte below the account after the admit, `admit(s, 0, d)` is `Full(c)` with `c.available` the limit, no frame is written and the `streams` tree is unchanged, while a `Duplicate` admit at the same limit still answers; **AH1, AH2 (v2.24), with digests d ≠ e:** after `admit(s, 0, d)`, `admit(s, 0, e)` is `RequestMismatch`, and again after `reply` at 0 and again after a close and reopen: each time no frame is written and the `streams` tree is unchanged; after the reopen, `admit(s, 0, d)` is `Duplicate` (replied) or `Abandoned` with a re-commit (unreplied), and `admit(s, 0, e)` still mismatches; with latest = 1 replied, `admit(s, 0, d)` and `admit(s, 0, e)` are both `HistoryUnavailable` with no frame; a `Full` admit of `e` at a new `p` binds nothing, so a later `admit` of `d` there is `Admitted`. The below-latest answer of `model_v23_streams.py` and `model_v221_streams.py` is v2.23's `Rejected`; v2.24's is checked by L33's model | M, W2; `model_v23_streams.py`, `model_v221_streams.py` | admitting any `p` on a new stream (`new_any_p`, law A); an admit that commits and then answers `Full` (`full_after_commit`, law E); a reply committed in a different frame from the effect; an admit whose frame can become durable after a later frame of the same stream (a journal that is not prefix-ordered); admitting latest+1 over an unreplied latest (`no_s7`); an aborted body's data writes kept (`abort_keeps_data`) or its reply dropped (`abort_drops_reply`) or kept without a ticket (`abort_no_ticket`) |
| **L30 adopt** | (a) AA1's trace: main at v1, fork b, main commits A at v2, b commits B at its v2, fork c from `snapshot_of(b)`, then `adopt(c)` is `Diverged(c)`, and main still reads A at v2, before and after reopen; (b) a fork of main with no further main commit adopts, and main then equals the fork's M at the fork's version; (c) `adopt(main)` is `Diverged(0)`; (d) a fresh fork of main, then one more main commit, then its adopt is `Diverged` | M, W2 | an adopt that compares versions only (`adopt_version_only`, caught by (a) as main reading B); an adopt that resolves `parent` and refuses a deleted one as `Dangling` |
| **L31 migrate indexes** | (a) AB1's trace: after `migrate`, the catalog lists `t` with exactly the index `by_b` and identity Y; `lookup` by `by_b` gives k1 and k2 equal to M; the old Table and the `by_a` Index are `Stale`, and they read M from a Snapshot taken before the migrate; later `put`s of `New` rows keep `by_b` equal to M, and reopen gives the same; (b) with an extractor in `to` that returns `Capacity` on the second row, and again with an index key over the key limit, `migrate` returns `Full` and `Limit(Key)` respectively, the version is unchanged, and the catalog, rows and `by_a` equal M from before; (c) `create_table` with a spec of two indexes is one schema commit (version + 1) and both indexes equal M after the first `put` | M, W2 | a migrate that keeps `from`'s indexes or calls their extractors (`migrate_keeps_old_index`, caught by (a): `by_a` is still listed); a migrate that installs rows after an index build fails (`migrate_partial`, caught by (b)) |
| **L32 step stop** | with `steps = Some(n)` and a body that does a `put` of a new row and then loops forever: (a) `run` returns `Stopped(OutOfTime { steps: n })`; the version, the account and the journal's frame count are unchanged; the row is absent in `snapshot_of(main)`, after `durable` of the last earlier ticket, and after reopen; and the next `run` commits at version + 1; (b) `look` with the same `n` and a looping query returns `Stopped(OutOfTime { steps: n })`; (c) with `steps = None` and a terminating body, the outcome, version and frame bytes equal those of the same body run with `Some(n)` large enough that it does not stop | M, W2 | a scope that closes after the publish or the handoff (`stop_after_publish`, caught by (a): the row is visible and its frame is on disk); a stop reported as `Failed` (`stop_as_failed`, caught by (a) and (b)) |
| **L33 client positions** | (AG1, AH1, AH2) over §1.8's v2.24 admit table: no client, correct or buggy, takes another request's reply as its own; the store binds one digest per latest position; an unchanged retry never mismatches. One stream; requests with distinct bytes; every send carries a new slot and deadline; each send refused before admission (a choice per delivery) or admitted; admitted attempts that reply, fault, or reply after the stream moved (`Missing("reply")`); lost responses; one client crash; one server reopen. Clients: the §1.8 client, and v2.23's three client mutants run as buggy clients. **Cases:** changed args or Key at latest (`RequestMismatch`, no commit, no run); reset to `p` = 0 after latest advances (`HistoryUnavailable`, terminal); reopen after admit (`Abandoned`, digest kept) and after reply (`Duplicate` for the same bytes, `RequestMismatch` for others); unchanged retries across slot and deadline changes (match). Laws: H, a client never sends `p` > latest + 1; I, every accepted reply is the run of the request's own bytes; B, while `p` is latest its digest never changes, and only `Admitted` binds one; M, the request bound at `p` never gets `RequestMismatch`; Q, the §1.8 client never sees `RequestMismatch` or `HistoryUnavailable`. The join halves are RUNNER-SPEC v2.11.3 L21 (`join_digest`); the built client half is WIRE W4c | M; `model_v224_client.py` (supersedes `model_v223_client.py`) | Store mutants, each CAUGHT: answering `Duplicate` before comparing (`compare_after_duplicate`, I); answering `Abandoned` before comparing (`compare_after_abandoned`, I); a digest that covers the deadline (`digest_includes_deadline`, M); a reply that drops the digest (`digest_dropped_on_reply`, B); a mismatch that re-binds the stream (`mismatch_mutates`, B); an earlier open's re-commit that binds the sender's digest without comparing (`readmit_rebinds`, B); `Rejected(latest)` below latest, read as `Superseded` (`superseded_below_latest`, I). Client mutant: advancing `p` after a pre-admission refusal (`advance_on_refusal`, H; WIRE W4c's `refused_p_reused`) |
| **L35 request digest** | (AH1) `request_digest(key, args)` with `key` = the 32 bytes `00 01 … 1f`: for `args` = `01 02 03` it is `172630b31e7cd5050e8c06447738d3002e79578c1bb18836380fa604eb1c2135`; for empty `args` it is `2b8d69847b046143da891fdb319ab0fb26033ef1b8116ff0e61e544adceb307b`. The hashed bytes are `11` ‖ `hirsel.request.v1` ‖ `20 00 00 00` ‖ key ‖ `03 00 00 00` ‖ args. Changing one byte of `key`, or moving a byte from the end of `key` to the start of `args`, changes the digest | M; RUN by Python `hashlib.sha256`, a separate implementation (`l35-digest-vectors.py`, receipt `l35-digest-vectors-run.txt`) | hashing key ‖ args with no lengths or tag (`no_framing`, caught: both values differ, and the moved byte gives the same digest); xxh3 used as the identity (`xxh3_identity`, caught by the values) |
| **L34 published value** | (AG3) a table `t` with key u64 and value `V0 { a: u32 }`, schema number 0, holds `put(1, {a: 7})`; its stored value is `00 00 07 00 00 00`. `t` has one index on `a`. Then `t` is opened with `V1 { a: u32, b: u32 }`, with `b` defaulted to 9, which appends number 1, and `put(2, {a: 5, b: 6})` is made. For `changes(before0, now)`, where `before0` is the Snapshot taken before key 1 was put: key 1's `published_value` is `07 00 00 00 09 00 00 00` (8 bytes; codec.md: a record is its fields in order, u32 little-endian) and key 2's is `05 00 00 00 06 00 00 00`; a delete of key 1 gives None; the Change for an index row of `t` is `Missing("table")`, and so is the Change for that index row's deletion; a value whose number is 7 is `Stale(t's id)`; `span_changes(now, prefix(t), prefix(t + 1))` gives keys 1 and 2 with the same published values as the Changes do. The bytes are derived by hand from codec.md, not taken from a run | M; hand-derived | removing the 2-byte prefix from every row (`strip_two_bytes`, caught: key 1 gives the 4 bytes `07 00 00 00`); keeping the prefix (`raw_value`, caught: key 2 gives `01 00 05 00 00 00 06 00 00 00`) |
| **L36 keyset pages** | (a) over a table and over an index with many equal J, every limit pair in {1, 2, 3} × {one row's bytes − 1, one row, 3 rows} and every resume, the concatenated pages equal `scan`/`find` on the same Snapshot, each row once and in order; (b) each page has ≤ `rows` rows, and its bytes are ≤ `bytes` unless it is one row; (c) the last page has `next` = None, and no page but a page over empty bounds is empty; (d) `next` with a newer Snapshot is `Moved`, and with a recreated table's Shape is `Stale(old id)`; (e) zero limits are `Missing` | M (its sorted list, sliced by hand at the limits); **PLANNED** (service-ops-1127 slice O7) | mutants `resume_inclusive` (opens at Included(the last full logical key): a row repeats), `resume_by_j` (resumes after J only: equal-J entries are skipped), `byte_cap_refuses_first` (an oversized first row yields an empty page, so the caller loops), `no_peek` (the last page ends with a `next` that reads nothing), `stale_unchecked` (a recreated table's Shape is accepted). |
| **L22 look** | for any body over Shapes and Indexes, `look(s, body)`'s value and reads equal those of `run` with the same body on a branch whose head is `s`, and the store is unchanged (version, journal, account) | M | a look that commits or bumps the version; reads recorded differently from `run`; a fault that escapes `isolate` |
| **L23 changes** | `changes(a, b)` equals the key-by-key comparison of full scans of `a` and `b`'s data trees, for pairs on one branch, across a fork, and across a checkpoint (nodes loaded from packs); `Foreign` for Snapshots of two opens | M | a subtree skipped whose node id matches but whose content differs (an id reused within an open); a deletion reported without None; streams-only commits reported |
| **L24 call record** | a committed `record` reads back from `recorded(db, b, v)` equal, with `p` and CallInfo from the same frame, and replay with and without the section gives the same state; a body that records, replies and returns `Error` leaves its record in the reply's frame, and `recorded` at `u.ticket`'s version returns it; aborted bodies with no reply, failed and refused transactions record nothing; a second `record` is `Exists`; once its segment is compacted, `recorded` is None; in a `memory` store it is always None. W2 adds: (i) right after `run` returns `Committed` with a CALL section, with no `durable` call, `recorded` is Some under every W4 schedule of the journal task, including one that has not yet taken the frame; (v) Z1, S4: after 100 main commits made durable, `fork` at version 1 and `run_on` that fork's version 2 with a CALL section, `recorded(db, fork, 2)` is Some under every W4 schedule, including one where the journal task has not taken frame 101; (ii) after a cut covering the version is taken, it is None both before and after that cut's ack, and if the cut is refused it is Some again once the refusal is learned; (iii) after an injected write failure, two `recorded` calls in a row both return `Poisoned(e)` with the injected `e`, and the next `durable` returns the same `e`; (vi) AD1: a record with `revision = Some(r)`, where r is the hand-written bytes 0x00, 0x01, …, 0x1f, reads back as `Some(r)`, the same 32 bytes, from `recorded` before and after reopen and after every W2 crash cut that keeps its frame; a record with `None` reads back as `None`; a `Some` of 31 or 33 bytes is `Missing("revision")` and commits nothing; a call section whose revision length is 1 is `Corrupt(Malformed)` at open; (iv) Y1: with `group_commits` = 1 and `group_bytes` = 1, (a) under a W4 schedule that holds the journal task's syncs until the 4th commit's `send` waits, then releases them, and (b) after 8 commits, each synced and acked before the next, with no `durable` call, a following `recorded` call returns. The W4 scheduler never finds every task blocked | M, W2; `model_v24_streams.py` law G | a call record dropped on `Aborted` with a reply (`abort_drops_call`); a record committed without its reply (`call_without_reply`); a section that changes replayed state; a section torn from its commit; an ack channel of capacity 2 × `group_commits` (`ack_cap_2g`, caught by (iv)(a): every task blocked, as in Y1's counterexample); a frame `send` without the drain before it (`send_without_drain`, caught by (iv)(b): the 7th ack finds the channel's 6 places full); a None revision written as 32 zero bytes (`revision_none_as_zeros`, caught by (vi): None reads back as `Some`); a revision dropped from the frame (`revision_dropped`, caught by (vi): `Some(r)` reads back as None); a `recorded` that reads segments without the barrier (`recorded_no_barrier`, caught by (i)); a fast path that compares the requested version with `journaled` (`recorded_version_fastpath`, caught by (v) as None); one that decides compaction from the files present (caught by (ii)); one that consumes the `Failed` without putting it back (caught by (iii)) |
| **L25 packs** | (a) with `pack_bytes` = 1, and with `pack_bytes` below one NODE at the value limit, a checkpoint writes one NODE per pack, no NODE is split, every pack is at most max(`pack_bytes`, 72 + its NODE frame), and reopen gives M. (b) Cut power before the MANIFEST of a cut that writes several packs, then during open's cleanup after every removal. Two more checkpoints and reopen give M, and no listed pack holds a leftover's bytes. Also crash the process after MANIFEST rename and before its directory sync, then cut power during the reopen's cleanup, keeping any subset of removals. The surviving MANIFEST names only present packs, and reopen gives M. If O1 fails, open returns `Io(e)` before any removal. | W2, M. `model_v25_packs.py` and the checkpoint family of `model_v2.py`. | a cut that lists a leftover without rewriting it (`skip_existing`), an open that loads packs it finds rather than the listed ones (`adopt_by_probe`), a cut starting below `next_pack` (`start_below`), a pack closed after the NODE that passes `pack_bytes` (`close_after`), or recovery that removes files before the O1 sync (`no_open_sync`) |
| **L26 read-set cover** | for random bodies over two tables with indexes (`fetch`, `range` with every bound kind, `lookup` iterated fully and partly), `look(a, body)` gives value x and reads R; after random commits to b, including puts that change V but not J, a delete and re-put of a row at the same J in one commit, and two rows whose extractor gives the same J, if no `tree_key` of `changes(a, b)` hits R (equal to a Point, or inside a Span), `look(b, body)` gives x again | M; the oracle is re-execution | a Point without its table prefix; a lookup row returned without its Point (J1); a span bound off by the 0x00 successor |
| **L27 account** | (a) a `memory` store on main with `resident_limit` one row above the catalog plus one row: 100,000 overwrites of one key are never `Refused`, and after each commit the account equals the hand sum of §1.11's `entry` over the live rows; (b, S4) with a pin held, overwrites whose conservative charges pass the limit while the exact size stays under it are not `Refused`, and after the settle the account equals the exact count; (c, AF1, S4 and S5) in the state of (b), with the conservative account past the limit and the exact one under it by more than each commit adds, `admit(s, 0, d)` on a new stream is `Admitted` and `create_table` of a table with no rows succeeds. Each leaves the hand-derived exact count plus its own charge. The S1 fork cases check that (i) a settled store with 228 of 300 bytes accounted can fork its live head and still account for 228, (ii) forking a reader-held snapshot that would bring the account to 322 against a limit of 321 returns `Full` with those exact values and leaves the store unchanged, and (iii) a fork of a live head under a pin admits 228 exact bytes when conservative charges would make the old additive check request 738 against a limit of 510. | M; hand-derived | an overwrite that adds without subtracting (the S1 source's `put_bytes`); a refusal made on unsettled charges; an `admit` that checks the limit without settling (`admit_skips_settle`, caught by (c): `Full` on a new stream); a schema commit that does the same (`schema_skips_settle`, caught by (c): `create_table` is `Full`) |
| **L28 refused cut** | with `checkpoint_task_bytes` swept from the least `open` admits upward until a cut completes: every `checkpoint` either returns `Checkpointed` or `Full(c)`; after `Full(c)`, `version` is unchanged and a `pin` made by that cut does not exist; then a put of a row with a 1 MiB value, a `checkpoint` refused because its NODE does not fit, a delete of that row and a `checkpoint` that completes; after each, reopen (and a W2 crash at every cut of the retried checkpoint) gives M, and `close` returns `Ok(())` | M, W2; `model_v29_refusal.py` | a table updated in place and left half-counted (`inplace_counts`); a pack counter advanced past a refused cut (`advance_next_pack`); an allocation after the rename (`alloc_after_publish`); a task that ends on `Capacity` (`refusal_ends_task`); a refused schema commit or pin published in memory |
| **L29 frame size** | (a) for random transactions (puts, deletes, index entries, a streams section whose values are 41 bytes admitted and 41 + R replied (AH3, v2.30 AN2), a CALL section with its code field (v2.30 AN1)): the payload size the body's arithmetic computes equals the `len` of the COMMIT the encoder produces for the same op list, and that equals the sum of §2.4's field widths computed by the test. (b) The boundary, worked by hand from §2.4. The payload is 21 (branch 4, version 8, at 8, section count 1) + 5 (data tag 1, ops count 4) + Σ(9 + k + v) per put (op tag 1, key length 4, key k, value length 4, value v, where v is the stored bytes, schema number included). Sixteen puts with distinct ascending 12-byte tree keys (4-byte table prefix ‖ 8-byte key) and fifteen values of 1,048,576 stored bytes give 26 + 16 × 21 + 15,728,640 = 15,729,002 + the last value. **cap − 1:** last value 1,048,277, payload 16,777,279, frame 16,777,295: `Committed`. **cap:** last value 1,048,278, payload 16,777,280, frame 16,777,296: `Committed`; on disk the reader replays it. **cap + 1:** last value 1,048,279, payload 16,777,281: `Limit(Transaction)`, no effect. A `memory` store and a disk store give the same three outcomes. (c) The reader, given a hand-built COMMIT with `len` 16,777,281 and a valid check, treats it as a bad frame (§2.8) | §2.4 by hand; the encoder called directly | size arithmetic that drifts from the encoder (a memory store then accepts what a disk store cannot write, or a disk store writes a frame its reader rejects); a cap in frame bytes against a reader bound in payload bytes (the 80-byte gap of U1); a missing section's head in the arithmetic; a streams value counted without its 32 request-digest bytes (`digest_not_counted`) |
| L15 read set | `Done.reads` equals the hand-written points and spans of the body, in order, as data-tree keys (I1): a `fetch`, a `range` with each pair of bound kinds, and a `lookup`, each written out byte by byte from §1.3's rule | hand-derived | a range that records no span; reads of the transaction's own writes left out; a Point without its table prefix (the S1 source's `fetch_bytes`) |
| L16 stale table | a Table value opened before `create_index` returns `Stale` on `put`; after `drop_table` and `create_table` of the same name with an index, the old Table, Shape and Index return `Stale(old id)` from `fetch`, `range` and `lookup` in `run`, from `look` on a current Snapshot, and from `get`, `scan` and `find`, never None or the new table's rows; on a Snapshot taken before the drop they return M at that version, and the new handles return `Stale(new id)`; in S4 the same with `migrate` in place of drop and create (I3; its index rules are L31) | M | an index silently skipped; a read handle checked by store only (the S1 source's `fetch`, `get`, `find`). **Red-on-S1 case (K2):** runner-spec-1020's p17 step 4a at 62f15988, an old handle read after `drop_table` + `create_table` of the same name gives `Ok(None)` (`runner-spec-1020/programs/p17_h_store/probe.hirsel`) |
| L17 schema | a stored schema without field f, read by a codec whose f has a default: rows decode with the default; then `put` under the new codec and reopen under it and under the old one: every row, old and new, equals M (the old codec reads new rows only if SEM-10 resolves them, else `table` is `Schema`); a pin taken before the change reads equal to M through today's Shape; removing a field without a tombstone gives `Schema` until `migrate`; after `migrate`, a pin older than it is `Stale`; `migrate` at every W2 cut is all-or-nothing; `table` appending a schema number moves `version(snapshot(db))` by exactly one, and a second `table` call with the same types leaves it equal (K1) | W2, M | a schema matched by position; rows written under the new schema decoded with the old reader (no tag); a schema number reused after `migrate`; a non-atomic migrate |
| **L18 CallInfo** | (a) `seed` equals the §1.7 formula computed by the test's own splitmix64, which must give `sm(0) = 0xE220A8397B1DCDAF`, and equals `0xC817A9BF84F9A191` for store {1, 2}, branch 0, version 1; (b) under a test clock that steps backwards, `at` never decreases on a branch; (c) after a crash and reopen, the CallInfo the next commit gets continues the branch's versions, and a replayed commit's `at` equals the one `run` returned; (d) `id` differs between two branches at the same version | hand-derived vector, W2, a test Clock | `seed = 0` (the review's mutant); a seed from a counter that restarts at open; `at` from the raw clock; `at` not stored in COMMIT; a CallId without the branch |
| **L19 identity** | (a) two stores created in one run have different `identity`; (b) `identity` is the same after reopen; (c) a pack or segment copied from store B over a file store A's manifest names makes A's open `Corrupt(Header)`; (d) one frame of B spliced into one of A's files at a frame boundary makes the open `Corrupt(Checksum)` (E3) | W2, F | an identity drawn at each open instead of at genesis; a header identity that is not checked; a constant identity |
| **L20 foreign handles** | every handle of store B used with a Db of store A (Table, Index, Shape, Branch, Snapshot, Ticket), and a Snapshot or Ticket from an earlier open of A used in `fork`, `pin` or `durable`, returns `Foreign`, and A's state still equals M | M | a missing store check; `durable` of an old ticket returning `Ok`; a fork from a stale Snapshot whose node ids collide with this open's |
| **L21 close, drop and poison** | (a) after `close` returns, an `open` of the same directory in the same run succeeds (LOCK released) and the state equals M at the last commit, because every commit's frame was sent at `run` and `close` waited for the last one's ack; `close` makes no cut (T1); (b) after an injected sync failure, `close` returns `Poisoned(e)` with the injected `e`, and then (a)'s `open` succeeds and gives a prefix; (c) dropping a Db without `close` releases LOCK, and reopen gives a crash prefix (L3); (d) a second `open` while a Db is live is `Held`; (e) `open` with `group_commits` = 0 or `group_bytes` = 0 returns `Missing` with that field's name, takes no LOCK and creates no file, and with both set to 1 every write holds one frame, and reopen gives M (W1) | W1 injection, W2, M | a close that refuses when poisoned; a close that skips the join or the LOCK release on an error path; a close that reports a later error instead of the first; a drop that leaks LOCK; an open that spins on, or is stuck at, a zero group bound |

L5 counts frames because incrementality is itself a count. The bound is derived from the tree, not taken from a run, so it is not a pinned number.

---

## 6. Measurements

**Method.**
- Protocol: core/MEASURE.md.
- Host: turbo2, x86-64, Linux 6.8.0-137, the same machine and disk for every row and its baseline.
- Builds: LLVM release and Cranelift speed, from `core/tools/measurement_artifacts.py`.
- Unit: instructions (callgrind) unless the row says otherwise.
- **Baselines are never the store itself.** Each is either a W6 Rust twin (889) or the old store's recorded figure (STORE-RECORD.md, historical cost targets).
  - W6 twins live in `core/twins/m4/` and do the same work with the same inputs and outputs: same rows, same file bytes for the durable rows, same sync count.
  - **A row's baseline does the work the spec requires of the store on that row (Q3).** That means its checks (I3's catalog check, frame checks on read), its encoding, its clock read, its isolation and its persistence: where the store must keep an old version, the twin keeps it too. A twin that skips required work is published beside the fair twin as a **bound**, never as the target.
- **GUESS** targets are mine. The first measurement replaces each, and after that the row holds against regression.
- Rows marked **PLAN** take their targets from M4-PLAN §4.

| # | Row | Workload | Baseline (not the store) | Target | After |
|---|---|---|---|---|---|
| R1 | in-memory transaction | `memory` store, one table, 100,000 transactions each putting one row (8-byte key, Account value as in core/programs/m2b/table), then reading it back | **target:** W6 twin `store_pcopy.rs`, a persistent path-copying B+ tree with the spec's fan-out of 16–32 entries per node, `Arc` nodes and shared entry bytes, keeping the base version across each put and dropping it at publish, plus §3.1's per-commit clock read and `catch_unwind`, the I3 check, op-list append and I4 account update per put, and the I3 check, `Point` record and decode per fetch; no COMMIT encode (Q2). **Reference:** `store_im.rs` (`im::OrdMap`, base kept, 64-wide nodes), 109,213 per commit (RUN, store-r1-cost-1030). **Bound, not a target:** the in-place twin `store_mem.rs` (`BTreeMap`, no old version), 4,992; and the m2b table twin, 0.990 / 2.31 (M4-PLAN §4, PLAN) | ≤ 3.0× `store_pcopy.rs` LLVM, ≤ 6× Cranelift (**GUESS** until that twin is measured; 1030's ESTIMATE for a fan-out-matched twin with copied entries was ~25k, which puts today's store at ~4.0× and X6 at ~2.7×). Today: 0.91× `store_im.rs`, 19.8× `store_mem.rs` | S1 |
| R2 | range scan per row | 100,000 rows as R1, one `range` over all of them, per row | W6 twin: `BTreeMap::range` over the same keys, decoding each value | ≤ 2.0× LLVM (GUESS) | S1 |
| R3 | `get` on a reader task | Snapshot of R1's 100,000 rows sent to a spawned task; 100,000 `get`s in a fixed pseudo-random key order (splitmix64 from 1) | W6 twin: `Arc<BTreeMap>` read on a spawned thread, same key order | ≤ 3.0× LLVM (GUESS) | S1 |
| R4 | durable one-key commit | one table, K = 16 keys (u64 key `i mod 16`, 8 codec bytes), 32-byte values (word j = (i × 0x9E3779B97F4A7C15) rotl 16j, LE), history 64 (commits 0–63 durable in the journal before timing, with no pin, fork or checkpoint, Z2); measured commits 64 … 63 + N, each `run` then `durable`, one at a time | old store: 75.5k instructions, 2,528 log bytes, 1 pwrite, 1 fdatasync; and W6 twin `durable_commit.rs`: encode the 91-byte frame, `write`, `fdatasync` | ≤ 75.5k instructions (PLAN); 97 log bytes (§2.4, arithmetic); 1 write and 1 sync per commit; ≤ 3× the twin's user instructions (GUESS) | S3 |
| R5 | group throughput with a real sync | one writer, R4's commit sequence and history (Z2), 10 s of back-to-back one-key commits, a `durable` after every 64th; turbo2's local disk | W6 twin `group_commit.rs`: the same frames, a writer thread and a journal thread draining up to 64 frames per `fdatasync`; history only: old log host 4.5–5.7k transfers/s at one sync per group | ≥ 0.8× the twin's commits/s and ≤ the twin's syncs per commit (GUESS) | S3 |
| R6 | checkpoint after 64 commits at 100k rows | 100,000 rows checkpointed, then 64 one-key commits on random keys, then `checkpoint` | old store full compaction: 372.1M instructions, 13.2 MB written | ≤ 3.7M instructions and ≤ 0.2 MB written (GUESS); NODE count within L5's bound (law) | S2 |
| R7 | open at 100k rows | open of R6's store after one checkpoint and an empty journal: pack read, decode, build | old store load at 100k: 1,955.3M instructions; W6 twin `open.rs`: read the same pack file whole and build `BTreeMap` from its leaf entries | ≤ 200M instructions (GUESS, 0.1× the old store); ≤ 4× the twin (GUESS) | S2 |
| R8 | resident memory | 1 M rows (12-byte keys, 32-byte values), no index and with one index; peak RSS after load and after open; a pin held across 10,000 commits | W6 twin: RSS of `BTreeMap<Vec<u8>, Vec<u8>>` with the same rows; old store: 343 B/row at 100k (34.3 MB) | measured RSS per row within ±25% of the §1.11 account (118 / 204 B/row), which calibrates its constants; ≤ 2× the twin's RSS (GUESS); the pin's extra ≤ 10,000 × (2h+1) × node size (law) | S2 (pin: S4) |
| R9 | **MEASURED** (store-build-1010, main, 2026-11-16): checksum per 4 KiB | 10,000 checks of a fixed 4 KiB node image, LLVM release; each side's output equals its Rust reference on development and release builds | W6 twin: table-driven CRC-32C and scalar xxh3-64 in Rust on the same bytes; the Rust ratios follow from 889's parity check | **xxh3-64: 92,247,236 instructions, 5.6 ms (2.25 instr/byte). CRC-32C: 266,801,271 instructions, 74.3 ms (6.51 instr/byte).** Picked: xxh3-64, and the format fixes it (E1). From here the row guards against regression: xxh3-64 ≤ 92.3M instructions. | done |

**Twins that skip required work (Q3, BY READING `core/twins/m4/` at origin/main 93f4029b and the rows above).** Each must gain the listed work before its row's ratio is a target. Until then it is published as a bound.
- **R2: fair.** A read needs no old version. Per row both sides step the tree and decode; the store's one Span and one I3 check per range are not per row.
- **R3:** the twin lacks I3's per-call check. `get` on a Snapshot looks up the Shape's catalog key in the Snapshot's catalog on every call (§1.5). The twin should do one lookup in a catalog map per `get`.
- **R4:** `durable_commit.rs` writes a 56-byte frame with CRC-32 (its MEASURE.md), not the store's 97-byte COMMIT with its xxh3-64 check (§2.4, E1). The method requires the same file bytes for durable rows, and the row's "91-byte frame" matches neither. It also updates a `BTreeMap` in place (no old version), and it reads no clock and has no isolation. It should write the store's exact frame bytes and do R1's per-commit work.
- **R5:** `group_commit.rs` is not landed. When written, it does R4's per-commit work and §3.1's END policy (an END write and sync when the queue is idle or every 64th group), so syncs per commit compare like with like.
- **R6:** no twin. The old store's full compaction is history, not a baseline. A fair twin does the cut's required work: walk the new nodes, encode each NODE frame with its xxh3-64 check, update a persistent node table's counts, write and sync a pack, and make the manifest with §3.2's 7 syncs. Until it exists, R6's targets stay absolute GUESSes.
- **R7:** `open.rs` reads the pack and builds a `BTreeMap` from leaf entries. The store must check every frame's xxh3-64 (§2.8), rebuild the persistent nodes, build the checkpoint task's per-node table, and `sync` each live segment (§3.2). The twin should do all four.
- **R8:** the twin holds the rows in a `BTreeMap`. The store must also hold the checkpoint task's per-node records (96 B per node, §1.11) and, for the pin part, the pinned old version. The twin should hold a per-node record table and, for the pin, a persistent map with the old version kept.
- **R9: fair.** The same bytes and the same check on both sides.

---

## 7. Build order: six slices

Each slice lands in final form: no stepping stones, no adapters, and later slices only add. Every slice runs §5's laws for what it has, and §6's rows after it.

**What each slice needs.** "—" means nothing.

| Slice | From 998 (ordered map) | From 907 (tasks and providers) | From 880 (spec) | From the codec derive | From earlier slices |
|---|---|---|---|---|---|
| S0 codec (std-codec-1008, not a store slice; D4) | — | — | SEM-10 as landed | — | — |
| S1 memory | **landed, std.omap 6b87c486:** the map at `K = V = Bytes` (memcmp order); `insert(mut ids, m, k, v)` and `remove(mut ids, m, k)` with one shared id counter; node views `root`, `node_id/level/count/len/key/value/child`; a range cursor of Arc shares (`send`); descent by recursion | — | — | — | std.codec as at 227d3382 plus D4's two additions (S1 may start before 1008 lands, and lands after it) |
| S2 disk | the decoded constructors `leaf(id, keys, values)`, `branch(id, low_keys, children)` and `from_root`, without revalidation | W1 `host.sync`, `host.sync_dir` (**landed**, 596b3309); W2 with rename over an existing name, remove and append (**landed**, 1070c6a3); `host.random` (**landed**, 1951a976) | — | — | S1 |
| S3 journal | — | `channel.try_receive` with §3.3's `Empty`/`Closed` rule (**landed**, 1070c6a3; tasks.md CONC-5); W1 failure injection (L8); W2 as for S2; W4 (L3 schedules, L9) | — | — | S2 |
| S4 pins, forks, migrate | — | W2 | S5a field defaults (**landed**, beff7ef2) for the defaults half of L17 | — | S2, S3 |
| S5 streams, scopes | — | W2 (L13) | — | — | S3 |

**The codec derive is no slice's dependency.** P1's switch from hand-written codecs to `codec.of()` changes no store code. It belongs to the derive lane (core-derive-codec-v1), after 913 lands the bound. v1's S6 is therefore not a store slice.

**S0, codec dictionaries and schema** is std-codec-1008's `core/std/codec.hirsel` (at 227d3382, not yet on main). The store depends on what it actually provides (D4):
- `Codec[T]`, which is plain (computed);
- `encode`;
- `decode`, and `take_with` for decoding at an offset;
- `schema` and `fingerprint`;
- `resolve -> Reader[T]` and `read`;
- `Key[T]` with `encode_key`, `decode_key` and `key_schema`, in the order-preserving, prefix-free key format §2.4 relies on.

The store needs two more things, filed to 1008:
- `Reader[T]: shareable + send`;
- a `read` from an offset.

v2's sketch (`codec_from` with `StaticFn`, decode over `View[Bytes]` with the writer schema as an argument, `resolve -> Codec[T]`) is withdrawn.

**S1, the in-memory store.** It starts the day 998's map lands.
- Delivers:
  - `memory`, `Options`, `Error`, `StoreId`, `identity`;
  - tables, indexes and schema epochs, with catalog entries made in memory;
  - `run`, `run_on(main)`, `fetch`, `put`, `delete`, `range`, `lookup`, `next`, with `fetch` and `range` over a `Shape` (F5) and `Aborted(Undone[A])` (F2: the streams ops of an aborted Tx are committed alone; S1 has no `reply` yet, so its `ticket` is None);
  - `look` and `changes` (F5, F6);
  - read sets and CallInfo;
  - `Snapshot`, `get`, `scan`, `find`;
  - the resident account and `Refused` at the limit (§1.11);
  - foreign-handle checks;
  - uses `std.check`'s xxh3-64 (E7); a store lane writes `core/std/check.hirsel` if no other lane has.
- `run`'s handoff is a no-op in a `memory` store, which has no journal.
- Also: schema numbers and Readers (D1), and `follow` (D5).
- Laws: L1, L2, L15, L16, L17 (schema numbers, in memory), L18 (a, b, d), L20 (in-memory handles), L22, L23 (in memory), L26, L27 (a), L29 (memory, and the encoder directly). Rows: R1–R3 (R9 is measured).
- v2.6 follow-up to the landed S1 (I1–I4): `Read` as tree keys and `fetch`'s Point prefixed; `lookup` returning the scoped `IndexRows` that records a Point per row, and no index write when J is unchanged (J1); the liveness check on every read; exact overwrite accounting on main.

**S2, on disk.**
- Delivers:
  - HEADER, NODE, MANIFEST, END creation, LOCK;
  - `open` and `close` in final form (§1.10 steps), with no journal task yet. Disk `open` is not `pub` until S3 (T1): S2's laws reach it from inside the package;
  - the checkpoint task, with packs capped at `pack_bytes`;
  - GC and copy-forward;
  - schema commits (`create_table`, `create_index`, `drop_table`);
  - `checkpoint`, `durable(Cut)`;
  - identity at genesis;
  - the exact account at each cut.
- There is no journal yet and no public disk store, so nothing promises a `run` ticket's durability. The laws claim durability only at a `Checkpointed` ack. `durable(Journal)` and the `close` wait for frames are the final code paths, reached once S3 sends frames. No S2-only behaviour is built (T1).
- Laws: L4, L5, L6, L11 and L12 (packs and manifest), L19, L21 (d), L25, L28. Rows: R6, R7, R8.

**S3, the journal.**
- Delivers:
  - segments, COMMIT and END frames, with the call section (F7) and `recorded`;
  - the journal task and §3.3's drain and close rule;
  - tickets and `durable(Journal)`;
  - `open` made `pub` (T1);
  - rotation and recovery (§3.4), with the `d` rule;
  - poison, poisoned `close`, and cancellation.
- Laws: L3, L8, L9, L10, L11 and L12 (segments and END files), L18 (c), L21 (a, b, c), L23 (across a checkpoint), L24, L29 (disk). Rows: R4, R5.
- Final as specified. Q1 is ruled, and C1 fixes the last-segment rule (the review's two reasons for not calling S3 final).

**S4, pins, forks and migrate.**
- Delivers `fork`, `adopt`, `delete_branch`, `run_on` for any branch, `pin`, `unpin`, `pinned`, `snapshot_of` and `migrate`.
- Laws: L7, L16 (`migrate`), L17, L20 (stale Snapshots), L27 (b). Row: R8 (the pin). Also: `migrate` gives new ids (I3); memory cuts and settle before refusing (I4).

**S5, streams and scopes.**
- Delivers `admit` with rule S7 and the request digest (AH1, AH2; `request_digest` needs std's SHA-256 from std-sha256-1071), `reply` (kept on `Aborted`, F2, with its digest), `record`, Abandoned detection, and `KeyScope`, `scope`, `scope_keys`, `union` and `narrow`. It also delivers `published_value` and `span_changes` (AG3), once the R2-contract (WIRE-SPEC §6, RUNNER-SPEC R2-b) has agreed its signature. Until then, nothing may publish `Change.value`.
- Laws: L13, L33 (a model; the client half is built by WIRE W4c, the join halves by RUNNER L21), L34, L35.

**Lanes per the delegation table:**
- S0–S5 have this written specification, so they go to Sol, even on the critical path.
- S2 and S3 carry durability, so a defect there is costly. Review them with an attack (astra), not by re-implementation.
- Per the orchestrator, the format sections (§2, §3.2–3.6) get one more short review before S2 lands. S0 and S1 can start from this version at once.

---

## 8. Language findings

Outcomes re-run on 78e07cb1 (`probes-v2.txt`). All are the same as on 072c82b7.

| # | Program (`programs/`) | Outcome | Refusing rule | Resolved by |
|---|---|---|---|---|
| 0 | the whole design | — | — | **No store-specific core operation is needed.** The wanted core operations are all general and are now ruled: `try_receive` (finding 9) and Random (finding 8), both 907. A positioned read (finding 10) is not M4 scope. |
| 1 | `tx_body`: a transaction body as a `scoped[view]` closure over `mut Tx` (a Snapshot view, a writes map, `View[Log[Read]]`), run under `isolate` | ACCEPTED (RUN) | — | nothing needed. It took two annotations, findings 7 and 11. |
| 2 | `snapshot_task`: a Snapshot of `Arc[Node]` roots held by a second task | ACCEPTED (RUN) | — | nothing needed |
| 3 | `index_call`: an index keyed by `StaticFn[fn(V) -> Result[J, Capacity]]`, called through `code.callable` in a generic body; `code.identity` | ACCEPTED (RUN) | — | nothing needed. Body revisions wait for `code.revision` (M4-PLAN §2.5). |
| 3b | `index_erase`: a `List` of `own[send] fn(V) -> Result[Bytes, Capacity]` that captures a StaticFn and a key codec, so J is erased | ACCEPTED (RUN) | — | library form. The literal's type `Result[own fn, (Capacity, F)]` has to be written in full: the error side is the capture tuple (LANG-14). |
| 4 | `cursor_for`: `for k in rows` over a user cursor | REFUSED (RUN) | LANG-16 "this source is not iterable" | **library form:** `while let` over `next`, ACCEPTED with owned results (`cursor_owned`) and with `-> Option[View[Bytes]] in r` (`cursor_view`). No ruling asked: P1 already says "no `for` change". |
| 5 | `codec_bound`: generic `Table[K, V: codec]` and `put[K: codec, V: codec]` | REFUSED (RUN, on 78e07cb1 too) | LANG-5 "unknown bound" | specified by beff7ef2 (OWN-4, SEM-10); 913 implements it. The store does not depend on it: library form, dictionary values (`tx_body`, ACCEPTED). |
| 6 | `descend_loop`: tree descent that reassigns a view, `n = view b.kids[i]`, in a loop | REFUSED (RUN) | OWN-10 "a place of `n` is viewed" | **library form:** recursion (`snapshot_task`, ACCEPTED), at most about 6 deep. For 880: a spec clarification, unless OWN-10 is meant to forbid re-pointing a view local. I think it is, and recursion is fine. 998 adopted recursion. |
| 7 | `tx_body`: `clone(key)` with `key: Bytes` | REFUSED until annotated (RUN) | LANG-12 "a type parameter of the callee is still open", because `clone[T, U](x: T) -> Result[U, Capacity]` (core.hirsel:142) leaves U open | **spec clarification** for 880: the result has to be annotated. A `clone` whose result type is its argument's would remove the annotation. That is a core API question, not the store's. |
| 8 | identity at creation | `host.random(r, mut b)` **landed** (1951a976) | — | done: `open` draws 16 bytes once (§2.3). |
| 9 | journal-side group drain | `channel.try_receive` **landed** (1070c6a3) with §3.3's rule | — | done; §3.3 states the `Empty`/`Closed` rule it must have. |
| 10 | paging nodes from disk; a file's size; a directory listing | no operations (BY READING host.hirsel: sequential `read` only, no seek, stat or readdir) | — | **library form now:** whole-file reads, state in memory under `resident_limit`, a manifest naming every file. Paging waits for a general `host.read_at`, which is not M4 scope, and needs no change to node bytes (§1.11). |
| 11 | `tx_body`: a local named `bytes` | REFUSED (RUN) | LANG-2/5 "expected a non-keyword name" (`bytes` is the spawn label keyword) | spec clarification: list `bytes` among the reserved words |
| 12 | `narrow` (first version): `pub type OutOfScope = OutOfScope` | REFUSED (RUN) | LANG-9 "a chain of aliases reaches itself" | spec clarification: a one-variant sum named like its type is read as an alias. Library form: a two-variant `Error`. |
| 13 | `db_task`: `Db` holding a `Task[u64]`, `Sender` and `Receiver`; a `Journal` record spawned with `take` | ACCEPTED (RUN) after `Journal: send` was declared (CONC-2) | — | nothing needed. It makes the writer non-send, so single-writer is a type fact. |
| 14 | node identity for incremental checkpoints | no `Arc` pointer identity (BY READING core/core/arc.hirsel) | — | library form: writer-assigned node ids, which 998 adopted. Content hashing, as Dolt and Noms do, would cost a hash per node (§10). |
| 15 | cancellation as a value | `isolate` does not catch `Cancelled` (FAIL-16, failure.md:260-265) | — | nothing needed: no `Error.Cancelled`. Each wait's effect is the crash cut it stands at (§3.7). |

---

## 9. Decisions this spec makes, with their reasons and reversibility

| Decision | Reason | Reversible? |
|---|---|---|
| state in memory; files read whole; `resident_limit` | no seek, size or listing in the host (ruling 2) | yes, when `read_at` exists; the format holds |
| node ids from the writer, not content hashes | no Arc identity; hashing costs O(new bytes); ids are exact and free | no, it is format. Hashes could be added later as a non-MUST kind. |
| schema, pin and fork changes only at a checkpoint | one catalog per journal suffix; pinned old versions need their nodes written | the API is unchanged either way |
| a new segment at every open | there is no truncate; salts become unnecessary | no, it is format |
| an END file per segment for the synced end | a flip in a synced last frame must be `Corrupt`, while a power cut's zeroed full-length frame must still open (D1, D3) | no, it is format (kind 0x0012, role 4) |
| `UNKNOWN_LIMIT` = 64 KiB | an old reader must bound what it allocates for a kind it cannot know | no, it is format; a larger new kind is MUST=1 |
| one FIFO frame queue with backpressure; bounded drain | classic group commit with no stranded tail; grouping touches only I/O | the queue capacity is a constant |
| no `Error.Cancelled` | a cancellation is an uncatchable failure (CONC-7, FAIL-16) | — |
| xxh3-64, a u64 bound to identity, file and offset | R9 measured it 2.9× cheaper than CRC-32C in Hirsel (`int.mul_wide`); one checksum across the compiler store, the tape and the store; 64 bits; the binding refuses moved and spliced frames | ruled 2026-11-16, reversible until S2 lands; then it is format |
| no B-tree invariant re-check on read | host rule; Goose's "safety, not integrity" | — |
| old reader pins become Snapshot values plus named pins | one writer process; a held value is the pin | — |
| `adopt` only by fast-forward | no merge in scope (Noms FastForward) | a merge would be added |

---

## 10. Prior art

OPENED means I opened the file at that revision. KNOWN means I know it but did not open it.

| Source | Mechanism | Verdict | Weakness, and what Hirsel does instead |
|---|---|---|---|
| SpacetimeDB 0b6f11a, OPENED | commit header with a crc32c on write and read (crates/commitlog/src/commit.rs:24-32, :163-177, :307-334) | ADAPTED | a per-commit checksum, kept; the algorithm REPLACED by xxh3-64, which is cheaper in Hirsel (R9) and 64 bits, and bound to the frame's place, which SpacetimeDB's CRC is not. min_tx_offset and epoch sit in every commit header, and an all-zero header means EOF only with preallocated segments (commit.rs:43-47). Hirsel has no preallocation; the version sits in COMMIT, the epoch in the file HEADER, and the synced end in the END file. |
| SpacetimeDB | segment magic, log format version, checksum algorithm, `ensure_compatible` (segment.rs:19-42, :73-78) | ADAPTED | the checksum algorithm field is a negotiation one format does not need. Hirsel keeps magic and format and drops that field. |
| SpacetimeDB | flush then sync (commitlog.rs:148-153) | ADOPTED | — |
| SpacetimeDB | a failed fsync panics (commitlog.rs:123-140) | REPLACED | it kills the process. Hirsel poisons the Db value (§3.8), and `close` still cleans up. |
| SpacetimeDB | snapshot pages addressed by blake3, hardlinked from the previous snapshot, objects synced before the root (crates/snapshot/src/lib.rs:198-240, :286-295, :367-427) | REPLACED | every snapshot still hashes every page to find the unchanged ones, which is O(state), and it needs hardlinks the host lacks. Hirsel skips subtrees already on disk by node id. |
| SpacetimeDB | serial reducers with read sets (crates/datastore/src/locking_tx_datastore/mut_tx.rs:393, 624-682) | ADAPTED | a range read can depend on the whole table. Hirsel records one span per range. |
| LMDB ba47717, OPENED | two meta pages; pick the newest valid one (libraries/liblmdb/mdb.c:1364-1395, `mdb_env_write_meta` :4881, `mdb_env_pick_meta` :5026) | REPLACED | the in-place meta write relies on sector atomicity. Hirsel writes a temporary file, syncs it, renames it and syncs the directory. The END file's in-place rewrite relies on nothing: a torn END is only lost detection. |
| LMDB | copy-on-write page touch (`mdb_page_touch` :3028) | ADOPTED | in memory, as 998's path copy |
| LMDB | free pages reused below the oldest reader (`mdb_find_oldest` :2651, `mdb_page_alloc` :2704, :2791-2831; FREE_DBI :1357) | REPLACED | one long reader holds back every page freed after it. A Hirsel pin keeps only the nodes it does not share with the head (reference-counted packs). |
| LMDB | reader table in shared memory (mr_txnid :885, :3309-3367) | NOT TAKEN | stale slots after a crash need `mdb_reader_check`. Hirsel has one process and readers that are values. |
| LMDB | single writer, `mdb_txn_commit` (:4652), `mdb_env_sync0` (:3120) | ADOPTED | the single writer is enforced by type: `Db` is not send |
| SQLite e5cb4b0, OPENED | WAL header with salts and cumulative checksums; a commit frame ends a transaction (src/wal.c:40-100, header size 32 at :480, frame header 24 at :477, magic :491) | ADAPTED | the salts guard reuse of one WAL file (`walRestartLog` :3976). Hirsel never reuses: it starts a new segment at every open and every cut. One COMMIT frame per transaction gives atomicity by its own CRC. |
| SQLite | checkpoint backfill bounded by readers' marks (wal.c:328-398, `walCheckpoint` :2282, `sqlite3WalCheckpoint` :4402) | REPLACED | a long reader starves the checkpoint and the WAL grows. Hirsel writes new packs and never writes into a file a reader depends on. |
| SQLite | per-reader `mxFrame` snapshot (wal.c:111-157) | REPLACED | Hirsel readers hold immutable Arc roots |
| Noms e5fa29d, OPENED | manifest = version:lock:root:tables, updated under flock by temp + rename (go/nbs/file_manifest.go:24-31, :100-147) | ADAPTED | the optimistic lock string serves multiple writers. NOT TAKEN: Hirsel has one writer. |
| Noms | immutable table files, conjoined past 192 tables (go/nbs/store.go:29, conjoiner.go:33-37) | ADAPTED | conjoin is triggered by table count, not by garbage, and this path has no GC. Hirsel copies forward by live ratio and reference-counts nodes. |
| Noms | prolly chunk boundaries by rolling hash (go/types/rolling_value_hasher.go:15-44) | NOT TAKEN | it gives trees independent of history, for diff and merge across stores. Hirsel does not merge, and it would pay a hash over every byte. |
| Noms | datasets and heads, SetHead and FastForward (go/datas/database.go:18-90) | ADOPTED | as branches and `adopt` |
| Dolt 356dd13, OPENED | NewBranchAtCommit and DeleteBranch (go/libraries/doltcore/doltdb/doltdb.go:1677, :1780) | ADAPTED | as `fork` and `delete_branch` in one store directory |
| Dolt | node address = hash of its bytes; WalkAddresses (go/store/prolly/tree/node.go:175, :64-78) | REPLACED | a hash per new node. Sharing in memory already gives Hirsel the dedup across branches, so ids suffice. |
| Dolt | DifferFromRoots skips equal subtrees (go/store/prolly/tree/diff.go:64-144) | ADAPTED | the checkpoint walk skips subtrees already on disk, by id |
| Goose d5ae5bb, OPENED | a verified loader: "safety, not integrity"; a checksum shows the file is the one written (docs/design/serialization.md:26-55); container format outside the design (:245-247) | ADOPTED | there is no container or version. Hirsel adds HEADER and the kind rule (§2.7). The review checked the same distinction. |
| Goose | F_FULLFSYNC on macOS (src/runtime/runtime_os.h:88-94) | ADOPTED | as a requirement on W1's `host.sync` on macOS |
| PostgreSQL fsync failure, 2018, KNOWN | a failed fsync cannot be retried | ADOPTED | poison, no retry |
| ALICE (Pillai et al., OSDI 2014), KNOWN | a rename is durable only after a directory fsync; a file's appended bytes can be zeros after a crash | ADOPTED | the `sync_dir` barriers in §3.2; the END rule's tolerance of zeroed full-length frames (D3) |
| Neon, KNOWN | branches at an LSN over a shared WAL in a page server | NOT TAKEN | it needs a page server and object storage (row 12) |
| the old Hirsel store (STORE-RECORD.md), BY READING | 54 core operations; checkpoints rebuilt from the whole state and nodes reused by SHA-256 digest (`checkpoint_tree`) | REPLACED | O(state) per checkpoint (372.1M at 100k rows). Hirsel is library code on general core and writes only new nodes. |
| store-spec-review-1006 model.py, OPENED | an executable crash-cut model of v1 | ADAPTED | it had no oracle (every cut passed). v2's model adds one and the END rule, and its mutants show it can fail (§3.9). |
| Convex npm-packages/convex/src/server/impl/query_impl.ts:279-312, OPENED | `paginate`: an opaque `continueCursor`, `maximumRowsRead` and `maximumBytesRead` | ADAPTED (v2.25) | its cursor is a served token that names a position but not the snapshot, so a later page can read another version, and its split cursors and page statuses serve a reactive query. Here a page names its Snapshot (`Moved` otherwise), and the served token waits for caller binding |
| Goose stdlib/dictionary.goose:1-15, OPENED | the library's keyed collection is an open-addressing hash table with no order | NOT TAKEN (v2.25) | it has no ordered cursor to page, so there is nothing to adapt |

---

## 11. M1 target: packed pages, per-collection roots, BlobRef, journaled fork (v2.40, NOT YET BUILT)

> **Status: M1 target.** The page codec and sorted builder live in `std.page` (AP3, AP8, AP13, AP20). §0–§10 describe the store as built, and they stay true until the M1 cutover slice lands. That slice flips this section to built and rewrites §2, §1.1, §1.3 (the result types and W2 step 2, AP1a, AP15), §1.8, §1.9, §1.10, §1.11, §3.1 (the commit sequence), §3.3, §3.4 (open, AP21), §3.5, §3.5a (install, §11.1), §3.6, §3.7, §3.8, §4 (its ownership and order table, which still sends `Cut`) and §5 to match, from the restatements in this section. Where §0–§10 describe the checkpoint task, its channel, its allowance or its `Cut`, `Checkpointed` and `Refused` messages, and where a changelog entry of v2.36 or v2.37 is marked withdrawn, that text is history: M1 is built from §11. The slice plan, the corrections to the master plan and the bench gates are in `store-wholehog/M1-DESIGN.md`. Clause ids are AP1–AP21, and the laws are L37–L51. v2.33 answered the M1 review (`store-wholehog/m1-review-1625/REVIEW.md`, findings B1–B6, H1–H14, M1–M8) v2.34 answers its rereview (`REREVIEW.md`, the partly resolved findings and N1–N13), v2.35 answers the second rereview (`REREVIEW-2.md`, its partly resolved findings and R1–R8), v2.36 the third (`REREVIEW-3.md`, its partly resolved findings and S1–S10), v2.37 the fourth (`REREVIEW-4.md`, its partly resolved findings and T1–T12), and v2.38 rewrites the checkpoint to option C (Sam's gavel ruling 1 of 2026-11-22, `SHAPE-1644.md`), answering the fifth (`REREVIEW-5.md`, U1–U13, which are not §2's older U1); v2.39 restores AP21 and moves the CALL onto its Replied op (AP1a, schemasmash-durable-1683); v2.40 answers the option C review (`store-c-review-1692/REPORT.md`, findings 1–13, cited here as C-1 … C-13) and the orchestrator's two capacity rulings of 2026-11-22 (one typed cause, `StoreCapacity`, through each operation's own result, AP15); the finding each clause answers is named in brackets, and `m1-review-1625/RESOLUTION.md` has one row per finding.

Integers are little-endian unless marked BE, as in §2. "str" and "bytes" have §2's meanings. xxh3-64 always means seed 0 with the default secret (E1).

### 11.0 The seven choices §11 makes, and why

1. **The admission unit is the row version, not the page (B4, B5).** The logical account counts each distinct stored row once across every participating root (AP15). It depends only on the logical contents of the roots, so live publication, journal replay, a reopen with no cut and a captured export all compute the same figure for the same state, whatever page partition or copy-on-write history produced it. The alternative, publishing the result of the canonical sorted op sequence that replay uses, fixes B5 only: B4's page-union charge still exceeds a one-row conservative charge, a delete still copies a leaf and so gains charge (losing the delete-only guarantee), and the writer pays a second apply per commit on R1's path. The row-version law is simpler, exact, and keeps **a delete-only commit is never refused by admission**. The journal cap (AP15) is not admission: it refuses every frame alike until a cut publishes. Its cost is stated in AP15: resident page memory can exceed the admitted figure under divergent roots, by a bound given there, and is reported separately.
2. **A live-head fork syncs its FORK frame before it exposes the branch (B1).** `fork` waits for the frame's group sync, then installs the branch and returns. No handle, name lookup or CallId can name a branch id that a crash could reissue. "Instant" means no tree walk, no settle and no cut: O(1) work plus one group sync, independent of tree size (L44). A durable allocation reservation or a persistent incarnation field was rejected: each adds format and replay state to buy an exposure window no caller needs, because `fork` is already `waits`.
3. **Relocation finds its closure by reverse edges over physical frames (B2, B3, H7).** The writer keeps one record per located live frame (AP16), keyed by its stamp (unique among live frames), holding its current location `(pack, offset)` and its incoming list, its live parents and holders named by stamp or holder id, so moving a parent changes no child's links (R6). A compacting cut rewrites the union of all live ancestors of every moved frame, found upward from the moved frames, before it emits anything. Dead frames in listed packs may repeat a live stamp or point into retired packs; open resolves only live edges, by exact location, from the MANIFEST roots (AP21). The descent test on `low` is deleted.
4. **A collection-map leaf owns its collections' root pages through typed edges (B6).** `kids` of a map leaf holds one edge per slot, and every traversal (read, uniqueness, re-stamp, cut, relocation, reference count, account) follows it (AP2, AP7).
5. **Journal frames have their own fixed allowance and never count against `resident_limit` (N2, N3, R1; v2.34, restated v2.35).** The two promises "a delete-only commit is never refused by admission" and "every byte the writer holds, frames included, is under `resident_limit`" cannot both hold: a delete frees no row version under a pin, yet its COMMIT frame is real memory. §11 keeps the first. `resident_limit` bounds the logical account alone, on disk and in memory alike. The lengths of frames handed to the journal worker and not yet acked are bounded separately by `FRAME_LIMIT` (AP15), with backpressure, never refusal; the journal that no cut covers has its own cap, `journal_limit`, which refuses at once (AP15). **No single figure bounds the store's process memory in M1** (v2.35, R1): AP15 lists each journaling buffer with its own fixed bound (the frames handed over, the one frame being built, the worker's group buffer) beside the segment reads and a cut's memory, and page memory, with the location table that describes it (AP16), is reported, not admitted. A commit may wait for journal acks. In exchange every refusal rests on the exact logical figure, the same roots get the same answer whatever history produced them, and a live-head fork is never refused by admission (only the journal cap or an allocator refusal during its preparation, before anything is sent, can fail it).
6. **The MANIFEST's pack lists are page trees, so every covering cut can be named (N8, H14, R5; v2.35).** The `packs` and `retired` lists leave the MANIFEST payload and become two AP3 page trees keyed by pack seq. Their pages live in class-2 packs, which `packs` never lists: they are found from the MANIFEST's two list roots, and a cut that leaves one with no live list page adds it to `retired`, so the built cleanup removes it with no directory listing (AP14, AP16, AP21). The MANIFEST then holds 152 fixed bytes plus the branch and pin records, every producer of a record checks one size expression (AP4), and so the MANIFEST of any covering cut fits its limit: **no cut is ever refused for MANIFEST size**. The alternative, a fixed share of one MANIFEST for the lists, has a workload (more than ⌊64 MiB / 29⌋ live single-blob packs) under which no covering cut fits and the journal grows without bound. Cost: a cut also writes the pack-list pages it changed (O(changed rows × list height)) and the rebuilt retirement list into class-2 packs, which roll at `pack_bytes` like every class (S4), and open follows two more trees and streams their files once.

7. **The writer pauses for each cut, writes only what changed, and a refused cut loses nothing (Sam's gavel ruling 1, 2026-11-22, option C; v2.38).** v2.34–v2.37 tried to make every covering cut unrefusable by funding its planning ahead of time. They used first a fixed checkpoint-task allowance, then a checkpoint reserve with credit per pack, per unlocated frame and per root holder, a size proof, and staging beside commits. Five review rounds kept finding blockers in that funding (REREVIEW-5, U1–U13). §11 now takes bbolt's shape. The one writer runs each cut itself and serves nothing else until it ends (AP16): it rotates, plans, writes the frames no earlier cut wrote (incremental copy-on-write, within L5's bound), syncs, renames the MANIFEST and installs. Every allocation is ordinary and fallible, from the writer's own limit, and comes before the MANIFEST rename. So a cut that cannot get memory is refused and **loses nothing**: the old MANIFEST and the journal, chained across the rotation, still recover every committed write, and the journal keeps growing until a cut succeeds. To bound that growth, **a write that would take the uncovered journal past `journal_limit` is refused at once and never waits**, with the cause `Journal(Capacity{requested: U + f, available: journal_limit})`: `Refused(Journal(c))` from `run` and `run_on`, `Full(Journal(c))` from `admit` and a live-head `fork`, and `Before(Full(Journal(c)), …)` from `serve` and `conclude` (AP15, v2.40). Any cut-made operation is the way out. No commit, migrate, fork, pin, open or install pays checkpoint credit, the checkpoint task and its workspace are gone, and nothing is staged beside commits. Cost: calls wait out a cut's pause, bounded in AP16 (about 0.1–0.25 s for 10 GB with 1% changed, GUESS), and a store whose writer cannot get a cut's memory refuses writes once its journal reaches the cap.
   - **Prior art.** bbolt v1.4.0 (`tx.go` at commit 0d516858): `Commit` (line 170) writes a single writer's dirty pages. If `tx.write()` fails it rolls back before `writeMeta` (line 267), so the old meta page still names the old tree, and a failed `writeMeta` rolls back too (line 269). **Adopted:** the paused single writer, and "every failure before the root pointer flips leaves the old root". **Adapted:** bbolt reuses freed pages in place, so its rollback also restores the freelist. §11 writes only new pack files at seq ≥ `next_pack`, so a refused cut leaves files that the next cut or open replaces (H2), and an I/O failure poisons instead of rolling back (§3.8). RocksDB (`include/rocksdb/options.h:2935–2938` at ff683b11): with `WriteOptions::no_slowdown`, a write that would wait "fails immediately with Status::Incomplete()" (`db/db_impl/db_impl_write.cc:2248`, `:2943`, `:2982`). **Adopted:** the journal cap refuses at once, as `no_slowdown` does. **Not taken:** RocksDB's default write stall, which delays or blocks writers (`level0_stop_writes_trigger`, `hard_pending_compaction_bytes_limit`), because here a blocked writer would also block the cut that frees it. `max_total_wal_size` (`options.h:879`) forces flushes rather than refusing; §11 takes from it only the idea of one figure for the whole journal.

### 11.1 Format word, kinds and limits

**AP1. The format word is 3.** HEADER's `format` (§2.3) is 3 in every file. A file with any other format is `Unsupported(format)`. A format-2 store directory is refused at its MANIFEST's HEADER, before payload dispatch, O1, cleanup, rotation or worker startup, and nothing in the directory changes. There is no dual reader and no migration path: M1 is the one format cutover.

**Kinds of format 3:**

| Kind | Name | Where | MUST | Payload limit (a decode ceiling of format 3, AP4) | Status |
|---|---|---|---|---|---|
| 0x0001 | HEADER | first frame of every file | 1 | 56 | unchanged |
| 0x0011 | COMMIT | j-* | 1 | 16,777,280 (U1) | **payload changed (AP1a)**: section tag 2 retired, the CALL rides its Replied op; keys are logical keys (AP5) |
| 0x0012 | END | j-*.end | 1 | 8 | unchanged |
| 0x0013 | FORK | j-* | 1 | 4 + 4 + 8 + 2 + 255 = 273 | new (AP19) |
| 0x0020 | NODE | — | — | — | **retired; never reused** |
| 0x0021 | PAGE | p-* of class 0 (data trees) or class 2 (the two list trees, AP14) | 1 | 8,384 (AP3) | new |
| 0x0022 | BLOB | p-* of class 1 | 1 | 8 + 16,711,680 = 16,711,688 | new (AP8) |
| 0x0030 | MANIFEST (format 2) | — | — | — | **retired; never reused** |
| 0x0031 | MANIFEST | MANIFEST | 1 | `MANIFEST_LIMIT` = 16,777,368 (AP4) | new (AP14) |
| 0x0040, 0x0050, 0x0060, 0x0070 | CLAIM, AUTHORITY, PRINCIPAL, FENCE | as §2.7 | as §2.7 | as §2.7 | unchanged |
| 0x0080, 0x0082, 0x0083 | export stamp, catalog delta, inventory | EXPORT (role 5), PREPARED (role 6), §3.5a | as built | as built | unchanged payloads |
| 0x0081 | preparation metadata | PREPARED (role 6) | as built | 1,048,832 (as built) | **payload changed (AP1b, C-1)**: the target loses `checkpoint_task_bytes` and gains NEW's `pack_bytes`, `checkpoint_bytes` and `journal_limit` |
| 0x0084 | prepared rows | PREPARED (role 6) | as built | 4096 + 16,711,680 + 8 = 16,715,784 (was 4096 + 1 MiB + 8) | payload unchanged |

**Retired kinds are refused whatever their MUST bit (M3).** A format-3 file holding a frame of kind 0x0020 or 0x0030 is `Unsupported(kind)`, whether its MUST bit is 0 or 1. This is an exception to §2.7 rule 4, which skips an unknown MUST = 0 kind: a retired kind is known, not unknown. A known kind found in a file of another role, or a PAGE in a class-1 pack or a BLOB in a class-0 or class-2 pack, is `Corrupt(Kind(kind))`.

**Install a prepared release (§3.5a) under format 3 (H12).**
- Its scratch files carry format 3 in their HEADERs. The EXPORT copies format-3 MANIFEST, pack and segment bytes as it copies format-2 bytes now. No 0x0080–0x0084 payload carries a stamp, a root or a location (0x0081's format-3 layout is AP1b): the inventory names collection ids with a row count and a digest, and prepared rows are (logical key, stored value) pairs (AP5).
- **Target checks before activation (T10; v2.40 C-1).** NEW opens with its own options, so install checks them before activation, while OLD is still selected. The checked values are exactly the four that the PREPARED target carries (AP1b): NEW's `resident_limit`, `pack_bytes`, `checkpoint_bytes` and `journal_limit`. They come from the validated import and from nowhere else. The import's SHA-256 covers every PREPARED frame, the 0x0081 frame included, and activation names that digest, so the values checked are the values the activated release was prepared with. Before activation the import runs, in order:
  1. **Open's options step** (AP15) over those four values. A target below the journal floor is `FullNew(Journal(Capacity{requested: checkpoint_bytes + FRAME_LIMIT, available: journal_limit}))`, and one whose floor is out of range is `FullNew(Journal(Capacity{requested: 18,446,744,073,709,551,615, available: journal_limit}))`, exactly as NEW's open would refuse them.
  2. **AP21's steps 0–4** over the immutable final layout it will publish, with NEW's limits: its private packs as written and the prospective MANIFEST it will rename, before the rename (C-7). This validates the class-2 files that NEW's open will stream (step 0 needs only its fixed chunk buffer).
  3. **The account.** It checks the proposed exact account against NEW's `resident_limit`. Over it is `FullNew(Resident(Capacity{requested: the account, available: resident_limit}))`.

  The layout the import emits is the one it checked: it is computed once, before activation, from the validated inventory, and emission follows it. An allocator refusal in these checks, or in the install's own cut (AP16), is `FullNew(Allocation(c))`, with c the allocator's request and room. The built install's two mappings, a target `OpenLimit` and `TargetNoRoom`, both become `FullNew` with the cause named. Three outcomes stay distinct, and in each OLD stays selected and healthy:
  - a target-capacity refusal, `FullNew(cause)`;
  - a validation failure of NEW's private output, `Store(e)`, with e AP21's `Corrupt`, `Dangling` or `Unsupported` error, which is a store defect;
  - an I/O failure, `Store(Io(e))` or `Scratch(Io(e))` as built.
- **Map edits come from the validated inventory, classified by the catalog delta (N7, R3).** As built, `changed_ranges` gives every id the delta drops a range with no catalog row and every id it creates a range with one, and `range_inventory` writes one record per range, so a dropped id has its own zero-count record. For every collection id in OLD's map or created by the delta, the import takes exactly one action:
  - *remove*, for an id whose range has no catalog row (the delta drops it): its validated record must be the zero-count drop record, as `read_valid_range` accepts it (count 0, digest of no rows); its map entry is deleted, so the id keeps neither an entry nor its 126-byte descriptor;
  - *replace*, for an id whose range has a catalog row (the delta creates it): its map entry gets the bulk-built root of the validated rows, and zero rows give the empty root with a 126-byte descriptor;
  - *keep*, for an id with no range: one the delta leaves alone or edits only in the catalog (a schema number appended to a populated table, §3.5a, whose range `changed_ranges` skips because `old == new`). Its map entry, root, row count and every page stamp are OLD's, unchanged.
  The three cases partition the ids. So a valid prepared drop of a populated table and its index passes validation and removes two map entries (L51 (e)), and a schema append on a populated, indexed table keeps every row and index entry through activation and reopen (L51 (f)).
- **Stamp reservation.** After M1–M3 validate and size the rows, the import computes R = 2·n + b + 1 for each replacement collection, with n its rows and b its rows whose stored value is over the inline threshold, by checked u64 arithmetic. The bulk build of n rows makes at most n leaves (each holds at least one row) and at most n − 1 branches (each holds at least two entries, AP13), and at most b blobs, so R is a bound. The import reserves the sum of every collection's R from the shared counter in one step, before building (past 2⁶⁴ − 1 it is `Limit(Ids)`, AP4), and builds each tree with stamps from its own sub-interval. OLD keeps committing while the import builds: its stamps come from above the reserved block, so no OLD stamp can alias an imported one. The graft's map-path pages (V3–C7) take stamps from the counter at graft time, above the block.
- **Account.** Before activation the import settles over the roots it would publish (main with the map edits above and the delta's catalog rows, plus every other participating root) and admits that exact figure (AP15), which becomes A with the graft. A kept collection adds nothing, and a removed one stops counting only where no other root holds it.
- The import builds each replacement collection by AP13's bulk build, and the graft at V3–C7 applies the map edits above in one map operation on main.

**AP1b. Preparation metadata of format 3 (kind 0x0081; v2.40, C-1).** The 0x0081 payload is the codec encoding (`core/std/api/codec.md`: an integer at its width, little-endian; `Bytes` a u32 count then the bytes; a record its fields in order) of one `PreparedMeta`:

```text
PreparedMeta   { base: ExportStamp, target: PreparedTarget, next_table: u32, changes: u64, ranges: u64 }
ExportStamp    { attempt: Bytes, store_hi: u64, store_lo: u64, opened: u64, branch: u32, head: u64,
                 next_table: u32, activation: u64, terminal: u64, end: u64 }                    // unchanged
PreparedTarget { describe: Bytes, release: Bytes, digest: Bytes, resident_limit: u64,
                 pack_bytes: u64, checkpoint_bytes: u64, journal_limit: u64 }                   // format 3
```

The built `PreparedTarget` ends `resident_limit u64, checkpoint_task_bytes u64` (92 + d bytes). Format 3 deletes `checkpoint_task_bytes` with the checkpoint task and appends three u64s. With d = |describe|:

| Off | Size | Field | Rule |
|---|---|---|---|
| 0 | 4 | attempt count u32 | 16 |
| 4 | 16 | attempt | |
| 20 | 8 | store_hi u64 | as built (the PREPARED HEADER's store) |
| 28 | 8 | store_lo u64 | as built |
| 36 | 8 | opened u64 | the HEADER's seq |
| 44 | 4 | branch u32 | 0 |
| 48 | 8 | head u64 | as built |
| 56 | 4 | next_table u32 | non-zero |
| 60 | 8 | activation u64 | as built |
| 68 | 8 | terminal u64 | as built |
| 76 | 8 | end u64 | as built |
| 84 | 4 | describe count u32 | d ≥ 1 |
| 88 | d | describe | |
| 88 + d | 4 | release count u32 | 32 |
| 92 + d | 32 | release | |
| 124 + d | 4 | digest count u32 | 32 |
| 128 + d | 32 | digest | |
| 160 + d | 8 | resident_limit u64 | NEW's |
| 168 + d | 8 | pack_bytes u64 | NEW's |
| 176 + d | 8 | checkpoint_bytes u64 | NEW's |
| 184 + d | 8 | journal_limit u64 | NEW's |
| 192 + d | 4 | next_table u32 | ≥ `base.next_table` |
| 196 + d | 8 | changes u64 | the 0x0082 frame count |
| 204 + d | 8 | ranges u64 | the 0x0083 frame count |

So the payload is 212 + d bytes: 84 of `ExportStamp` (20 + 64), 108 + d of target and 20 of tail.
- **Writer.** `prepare` stores NEW's effective options, the values NEW's open will use with every default filled in, never the caller's raw `Options`. It encodes the metadata into one buffer before it opens PREPARED and compares that buffer's actual length, not a formula, with the kind limit 1,048,832. Longer is `Scratch(Limit(Value))`, the built writer's mapping, and no PREPARED is created. With the fixed 212 bytes, the longest `describe` that fits is 1,048,620 bytes.
- **Reader.** The import reads the frame head, refuses a payload length n over 1,048,832 before it reads the payload (§2.7), decodes one `PreparedMeta` that consumes exactly n bytes (bytes left over, or a field that ends early, are `ImportMalformed`), and checks the built field rules above. It does not range-check the four target values itself. They are checked by T10's step 1, exactly as NEW's open would check them.
- **Binding.** Install's T10 checks, its AP21 run, the cut that emits NEW's layout and the account check use exactly these four decoded values (§11.1 T10).

**AP1a. COMMIT payload of format 3: the CALL rides the Replied op that owns it (v2.39).** §2.4's COMMIT is kept with these changes, and the cutover slice rewrites §2.4 to match:
- **Sections.** The section count is 1–2. Tag 0 is data and tag 1 is streams, each `ops u32` then ops. The tags are strictly ascending (v2.40, C-8): a COMMIT holds at most one section of each tag, data first. **Tag 2 (`call`) is retired**: a format-3 COMMIT holding it is `Corrupt(Malformed)`.
- **Ops.** Tag 1 is a put (key, value), tag 2 a delete (key). These are the only data-section tags. In the streams section, tag 1 puts an Admitted or a Replied value. **Tag 3 is a Replied put with its call**: key (u32 length ‖ the stream name), value (u32 length ‖ the streams value, whose state byte must be 2, Replied), then the call: entry key (u32 length ‖ bytes), args (u32 length ‖ codec bytes), revision (u32 length ‖ bytes; length 0 or 32), code (u32 length ‖ 16 or 48 bytes, v2.30 AN1), and settle (u8: 0, or 1 or 2 followed by base u64-BE ‖ at u64-BE ‖ seed u64-BE, v2.29 AM6). The call has no stream field: its stream is the op's key.
- **Decoder rules**, each failure `Corrupt(Malformed)`: a tag-3 op outside the streams section; a tag-3 op whose value's state is not 2; more than one tag-3 op in a COMMIT; a revision length other than 0 or 32; a code length other than 16 or 48; a settle other than 0, 1 or 2; a call that ends early; two sections with one tag, or a streams section before a data section (C-8); any other op of the streams section (a tag-1 put or a tag-2 delete) whose key equals the tag-3 op's key (C-8). So every decoded CALL belongs to exactly one Replied op in its own frame, that op is the only op of its stream in the frame, and an Admitted put, a delete or a missing op can never carry one. The row that replay leaves for the stream is therefore the reply that `recorded` returns. Other streams keys need not be unique or ordered on read, as built; the writer emits each streams key once, in order.
- **Writer.** `serve` and `conclude` attach the record to the Replied put that consumes its `Completion` (§1.3; with no reply call the record is discarded, as built). The encoder writes that put as tag 3 and never encodes `rec.stream`, which names the same stream by construction. That construction is the built check, kept (C-8): before anything is published, `serve` and `conclude` require `rec.stream` to equal the Completion's stream (the built `check_completion_record`), for a Pending made by `untried` as for one made by `serve`, and a mismatch is `Before(Foreign, None)` (AP15) with nothing written. A record is kept and discarded exactly as §1.3 says.
- **Replay.** Replay applies a tag-3 op as the put it holds and skips its call, which changes no state. The call's CallInfo is the frame's when settle is 0, and the recorded run's otherwise, as built.
- **`recorded`.** `recorded` returns the call of the tag-3 op in the COMMIT at (b, version), or None, with `CallRecord.stream` set to that op's key (W2 step 4). Its result type `Recorded` and the one-CALL rule are unchanged. **W2 step 2 is restated (C-8):** "the latest cut this Db has taken" is the **last published generation**, the one whose MANIFEST the writer last renamed or opened. A version at or below that generation's head of b (the version AP14's branch record holds; a branch the generation does not list has none) gives None, because its CALL is in a covered segment. A refused cut never counts, before or after its rotation, however many refusals follow the last publication, so a CALL in a segment that two refused cuts rotated past is still returned. The writer keeps those heads with its holder records (AP16 step 5), so the test reads no file. Steps 1, 3 and 4 are unchanged. `journal_segment`, the parser behind recovery, `replay_export` and `recorded`, decodes the op once for all three (H9).
- **Size.** The call loses the section tag (1 byte) and the stream field (4 + |stream|). §2.4's replied call example, 294 + L bytes (326 for L = 32), becomes 281 + L (313 for L = 32) with its 8-byte stream name. A data-only COMMIT is byte-identical to §2.4's, so the 97-byte one-key put and L29's boundary are unchanged. **Worked (C-8):** the 313-byte commit is 97 bytes of data frame (head 16, fixed payload 21, section 5, the put 1 + 4 + 12 + 4 + 34 = 55), 71 bytes of streams section (tag 1, ops 4, op tag 1, key 4 + 8, value 4 + 49) and 145 bytes of call (key 4 + 32, args 4 + 16, revision 4 + 32, code 4 + 48, settle 1). A settle of 1 or 2 adds base, `at` and seed, 24 bytes, so the same commit with settle 1 or 2 is 337 bytes. The body's `Limit(Transaction)` arithmetic (Q2) counts the call this way.

**§2.7 rule 6 is restated:** the format word changes at a cutover that retires kinds or changes a kept kind's payload, as AN2 (word 2) and AP1 (word 3) did. A kind added inside a format follows rules 1–5.

**AP4. Limits: write admission and decode ceilings are separate (H8).**
- **Decode ceilings are fixed by format 3** and never follow an API constant: a BlobRef `len` and a COMMIT op's value length are at most `FORMAT_VALUE_MAX` = 16,711,680 bytes; a BLOB payload at most 16,711,688; a prepared-rows payload at most 16,715,784; a COMMIT payload at most 16,777,280. A stored value within them is readable by every format-3 reader, whatever the writing open's API limit was.
- **Write admission** is the API constant `VALUE_LIMIT`, checked by `put`, `migrate` and import as `Limit(Value)` is today. M1 sets it to 16,711,680 = `FORMAT_VALUE_MAX`. Lowering it later refuses larger new writes and leaves larger stored values readable, because readers check only the decode ceiling. Raising it past `FORMAT_VALUE_MAX` is a format change, and past the transaction limit it needs a chunked COMMIT path, which M1 does not have.
- Each tree keeps its own domain: a catalog value keeps its built cap of 1 MiB (`encode_catalog`), so kind 0x0082 keeps its limit, and 0x0081 keeps its 1,048,832-byte limit over its format-3 payload of 212 + |describe| bytes (AP1b); a streams value keeps its built bound; a collection-map value is always the 56-byte `CollectionRoot` (AP7).
- The key limit stays 4096 bytes on the logical key (AP5), which is today's check on the id-prefixed key. The page target is 4096 bytes (AP3). The inline threshold is 1024 bytes (AP8). The transaction limit stays 16,777,280 COMMIT payload bytes (U1). A put of a full key and a full value fits one COMMIT with room for its fixed fields.
- **Counters.** Branch and table ids are u32. A `fork` or `create_table`/`create_index`/`migrate` that would need an id that is not below 2³² − 1 is `Limit(Ids)`, refused before anything is allocated or published (H10). Stamps are u64; a stamp reservation that would pass 2⁶⁴ − 1 is `Limit(Ids)` too.
- **MANIFEST size: one expression (H14, N8, R5).** The MANIFEST payload (AP14) is 152 fixed bytes (72 of fields, the two list-root PagePtrs of 36 each, and the branch and pin counts of 4 each) plus its records:
  `manifest_bytes(B, P) = 152 + Σ_{b ∈ B} (143 + |name_b|) + Σ_{p ∈ P} (122 + |name_p|)`, by checked u64 arithmetic,
  for the branches B and pins P it lists. `META_LIMIT` = 16 MiB = 16,777,216 and `MANIFEST_LIMIT` = 152 + `META_LIMIT` = 16,777,368. The pack and retirement lists are not in the payload (choice 6); they have no share and no size limit beyond their u64 seqs.
  - **Every producer of a record checks it.** The only producers of a branch or pin record are `fork` (both paths) and `pin`. Commits, `create_table` and the other schema commits, `migrate`, the install graft and cuts add none, and `adopt`, `unpin` and `delete_branch` only remove one. A `fork` requires manifest_bytes(B ∪ {its record}, P) ≤ `MANIFEST_LIMIT`, and a `pin` manifest_bytes(B, P ∪ {its record}) ≤ `MANIFEST_LIMIT`, over the current live branches and pins (a journaled fork's record counts from the moment it is admitted); otherwise `Limit(Manifest)`, before any id, frame or cut.
  - **A cut checks the same expression** over the branches and pins it captures. Every captured record passed its producer's check and no record has grown, so it holds: **no cut is ever refused for MANIFEST size**. This is a guarantee about the MANIFEST payload only: a cut can still be refused for memory, which loses nothing (choice 7, AP16).
  - Open refuses a MANIFEST payload over `MANIFEST_LIMIT` (the kind's limit) before decoding it. Q1a's `checkpoint_task_bytes` and its allowance are deleted with the checkpoint task: the writer encodes the MANIFEST into the cut's pack buffer (AP16).

### 11.2 Pages

**AP2. One page, one `Arc`, one layout (B6, H3).** Every tree (each collection's tree, the collection map, `streams` and `catalog`) is a B+ tree of pages.
- In memory a page is `Arc[Page]` with `Page { image: Bytes, kids: List[Arc[Page]], blobs: List[Arc[Bytes]] }`.
- **Edges.** A page's owning edges, in slot order, are:
  - in a branch, `kids[i]` is the child of the entry in slot i;
  - in a leaf of the collection map, `kids[i]` is the root page of the collection in slot i. A collection with no rows has the store's one **empty page** `EMPTY` as its kid: a shared `Arc[Page]` with a zero-entry image and stamp 0, which is never written, never counted and never loaded. On disk its root is the all-zero PagePtr (AP20). A map leaf has no blobs: a `CollectionRoot` is always inline (AP7);
  - in any other leaf, `kids` is empty, and `blobs[j]` is the body of the BlobRef whose `body` field is j.
- Every traversal follows these edges and never a location: reads, the uniqueness walk of AP10, re-stamping, the cut, relocation, reference counts and the account. So a map entry written before any cut, whose PagePtr holds no location, is reached by its kid.
- **Canonical form is a disk invariant, not a memory one (H3).** The cut writes every page in canonical form (AP3) into a fresh buffer. A published in-memory image may be unsealed: it may hold free space, dead entries, a shorter common prefix and any `body` numbering. Every reader of a published image (a Snapshot, a reader task, `changes`, the cut) finds entries through the slot array and finds a body through its `body` field, never by position. A published image is never changed: sealing copies.
- The child and blob locations inside an image (`pack`, `offset`, `len`, `checksum` of each PagePtr and BlobRef) are meaningful only on disk. In memory they hold whatever was loaded, or zero for a page no cut has written. The `stamp` fields are always meaningful.
- The writer keeps the location table (AP16). The cut fills the location fields from its layout as it encodes each image into its frame.

**AP3. PAGE payload (kind 0x0021): one page.**

| Off | Size | Field | Rule |
|---|---|---|---|
| 0 | 1 | level u8 | 0 = leaf; a branch's level is its children's + 1 |
| 1 | 1 | zero u8 | |
| 2 | 2 | count u16 | entries, ≥ 1 |
| 4 | 2 | prefix u16 | P, the length of the page prefix |
| 6 | 2 | dead u16 | bytes of dead entries in the heap; 0 on disk |
| 8 | 8 | stamp u64 | non-zero (AP6) |
| 16 | 4 | heap u32 | the offset of the first byte after the last heap entry |
| 20 | 4 | zero u32 | |
| 24 | P | prefix | the page prefix bytes |
| 24 + P | heap − 24 − P | heap | the entries |
| heap | len − heap − 2·count | free | 0 bytes on disk |
| len − 2·count | 2·count | slots | count u16 offsets into the page, one per entry, in ascending key order |

`len` is the payload length.

**Leaf entry**, at its slot's offset:
- `klen u16`, then `klen` bytes of stored suffix (AP5);
- `vtag u8`: 0 = inline, 1 = BlobRef, anything else `Corrupt(Malformed)`;
- for vtag 0: `vlen u16` (at most 1024), then `vlen` value bytes;
- for vtag 1: a 42-byte BlobRef (AP8).

**Branch entry:**
- `slen u16`, then `slen` bytes of stored separator suffix;
- a 36-byte `PagePtr` (AP20) to the child;
- `rows u64`, the child subtree's exact row count;
- `bytes u64`, the child subtree's row charge: the sum of `entry(k, v)` over its rows (AP15).

Entry 0's separator is always empty (slen 0): it stands for "below every key" and takes no prefix. This holds on **every** branch, whether made by a split, a merge or a bulk build. For entry i ≥ 1, the logical separator is the page's prefix ‖ its suffix. Every key in child i − 1 is below it, and every key in child i is at or above it.

**The page prefix.**
- In a leaf, the prefix is a common prefix of every entry's key after the collection id is removed (AP5).
- In a branch, it is a common prefix of the separators of entries 1 … count − 1. A branch with one entry has P = 0.
- On disk it is the *longest* such common prefix. A leaf with one entry therefore has that entry's whole key as its prefix and an empty suffix.

**The canonical (disk) form.**
- `dead` = 0 and no free space (`heap` = len − 2·count).
- The heap holds the entries in slot order, packed from 24 + P with no gaps.
- The prefix is the longest common prefix.
- BlobRef `body` fields number 0, 1, … in slot order over the leaf's BlobRef entries, and the page has exactly that many blobs.
- So the disk bytes of a page are a function of its logical content, its stamp and the locations of its children and blobs.

**A private page** (AP9) may hold free space, dead entries, a shorter common prefix and any `body` numbering, with these bounds (H3, N11): its image is at most 16,384 bytes, so every slot offset and `dead` fits u16; `blobs` holds exactly one body per live BlobRef entry, so `blobs.len` ≤ count and every `body` field is below it. **An edit that removes or overwrites a BlobRef entry drops its body from `blobs`:** the last body moves into the freed index, the one live entry that named the last index is rewritten to name the freed one, and the list shrinks by one. So superseded bodies are never kept alive by a private page (a pinned Snapshot keeps its own page and its own body), and `body` never approaches u16 exhaustion. Sealing renumbers in slot order. An edit whose result would not fit 16,384 bytes repacks the page first: it rebuilds the canonical form into a new buffer, keeping the stamp (the page is private), then applies the edit. Repeated delete and insert churn therefore repacks before it can overflow a field, however small the canonical size stays.

**Size bound.** The page target is T = 4096 bytes.
- A leaf exceeds T only when it holds one entry. The largest leaf is 24 + 2 + 4096 + 1 + 2 + 1024 + 2 = **5,151** bytes: a stored key of up to 4096 bytes (the streams and catalog trees have no id to strip), with an inline value at the threshold.
- A branch exceeds T only when it holds at most three entries. The largest branch is 24 + 3 × 54 + 2 × 4096 + 3 × 2 = **8,384** bytes, and that is the PAGE payload limit.
- **A page is legal** when its canonical size is at most T, or it is a leaf with one entry, or it is a branch with at most three entries. Any edit that leaves a page illegal (an insert, an overwrite that grows a value, a separator pushed up from a child) splits it at once, in the same write.
- **The split rule (N6).** Cut the page's entries, in slot order, into the fewest consecutive pieces that are each legal **and, for a branch, each hold at least two entries**. Among the cuts with that fewest number, take the one whose largest piece (by canonical size) is smallest, and among those the leftmost. Such a cut exists, because a branch piece of two or three entries is always legal. At most three pieces are needed after one edit of a legal page: the edit inserts at most two adjacent entries, and the entries before and after them form legal runs. Example: children A0, A1, B, C with a 4,001-byte separator before A1 and one-byte separators before B and C. The 1 + 3 cut would leave a unary branch and is not a candidate; the split is 2 + 2, [A0, A1] keeping the long separator and [B, C] with B's separator moved up. The parent gains one entry per extra piece, and may split in turn. A root that splits gains a new root above it.
- **The merge rule (H6).**
  - A delete or a shrinking overwrite that leaves a non-root page's canonical size below T/4 merges it with its right sibling under the same parent, or its left sibling when it is the last child, if the merged page is legal. Otherwise nothing is done, except for a unary branch below.
  - **No non-root branch is ever left with one entry.** A non-root branch left with one entry merges with its sibling (chosen as above) when the sibling has at most two entries, which is always legal (at most three entries). Otherwise it borrows the sibling's nearest entry, which leaves it two entries and the sibling at least two, both legal. **Borrowing rotates through the parent separator s between the pair** (the logical separator of the right page in the parent):
    - from the left sibling L into the unary right page R: L's last entry e leaves L; e's child becomes R's entry 0 (empty separator), R's old entry 0 becomes R's entry 1 with logical separator s, and e's logical separator becomes the parent's new separator for R;
    - from the right sibling R into the unary left page L: R's entry 0 leaves R; its child is appended to L as entry 1 with logical separator s; R's entry 1 becomes its entry 0 (its separator emptied), and that entry's old logical separator becomes the parent's new separator for R.
    Every page then keeps entry 0's separator empty, and each prefix is recomputed.
  - **A merge restores the bridge separator.** When branches L and R merge, R's entry 0 (empty separator) becomes an entry of the merged page whose logical separator is R's separator in the parent, and R's separator is removed from the parent.
  - A page left with no entries is removed from its parent. A root branch left with one entry is replaced by its child. A root leaf left with no entries makes the tree empty: its root becomes the empty page (AP2).
- In a branch split, the right piece's first entry becomes its entry 0 with an empty separator, and its separator moves up to the parent.
- The separator pushed up by a split, a borrow and a bulk build is the **shortest separator**: the first (lcp(a, b) + 1) bytes of b, where a is the left side's last key and b the right side's first.
- **Occupancy.** Every non-root branch holds at least two entries and every non-root leaf at least one. So a tree of n rows has height h ≤ 1 + ⌈log₂ n⌉ (a single leaf has h = 1). This is the bound L5 uses (§11.15).
- The rules are deterministic over a given page and edit. M1 does not rely on two different edit sequences giving the same pages: the account does not depend on page partition (AP15), so live and replayed trees may differ in partition and agree in every answer (B5).

**Decode rules (each failure is `Corrupt(Malformed)` unless named; M3).** Every computation below is in u64 with no wrap; an overflow is `Malformed`.
- The header's zero fields are zero; count ≥ 1; stamp ≠ 0.
- 24 + P ≤ heap ≤ len − 2·count.
- Each slot points into [24 + P, heap). Its entry, with every length field it holds, ends at or before heap. **On disk the entries tile the heap in slot order:** slot 0 is 24 + P, each later slot equals the end of the previous slot's entry, and the last entry ends at heap. So no slot can alias another entry's bytes (M3).
- **Key widths.** In a collection's leaf, 4 + P + klen ≤ 4096 (the logical key limit, AP4); in a streams or catalog leaf, P + klen ≤ 4096; a branch separator obeys the same bound for its tree. In a collection-map leaf, P + klen = 4: the key is exactly one u32 id.
- vtag is 0 or 1; vlen ≤ 1024; a BlobRef's `body` is below the page's BlobRef count, and the `body` fields are 0, 1, … in slot order.
- A branch stores an empty suffix in slot 0, and every other slot has a non-empty separator after restoring its prefix. A branch's level is ≥ 1, and every child's level is the branch's level − 1. A collection map leaf's entries all have vtag 0 and vlen 56, and each value is a valid `CollectionRoot` (AP7).
- dead = 0 and heap = len − 2·count on disk.
- **Not checked** (§2.8's line): key order across slots, separator bounds and prefix maximality. They are our writer's output under a 64-bit check bound to its place. A wrong order gives a wrong but well-formed tree, and decoding stays total.
- **Precedence.** For each edge open resolves, the checks run in this order and the first failure is the answer: the location order rule (AP20, using only the parent's location and the PagePtr's `pack` and `offset`, so a self-pointer is `Corrupt(Order)` before any checksum that depends on itself is tried); listing and class (`Dangling`, `Malformed`); the five load checks (`Corrupt(Checksum)`); then this decode (`Corrupt(Malformed)`) and the child-level rule.

**AP20. `PagePtr`, 36 bytes, in branch entries, in `CollectionRoot` and in MANIFEST roots:**

| Off | Size | Field | Rule |
|---|---|---|---|
| 0 | 8 | pack u64 | the seq of the pack that holds the child's PAGE frame: class 0 in a data tree, class 2 in a list tree (AP14) |
| 8 | 8 | offset u64 | the byte offset of that frame's head in the pack |
| 16 | 4 | len u32 | the frame's payload length, which is the page's length |
| 20 | 8 | checksum u64 | xxh3-64 of the page's payload bytes (not the frame head) |
| 28 | 8 | stamp u64 | the child's stamp; equal to the stamp in the child's header |

**The empty tree** is the all-zero PagePtr: stamp 0 is never issued. It is allowed only as a `CollectionRoot` root and as a MANIFEST root. In a branch entry, or with stamp 0 and any other field non-zero, it is `Corrupt(Malformed)` (M3).

**A load through a PagePtr checks five things:**
- the frame at `offset` in `p-<pack>` passes its frame check (§2.1), which binds store, file and offset;
- its kind is PAGE;
- its payload length is `len`;
- xxh3-64 of its payload is `checksum`;
- its header stamp is `stamp`.

A failure is `Corrupt(Checksum)`. A pack not listed, or an offset at which no frame of the pack starts, is `Dangling(stamp)`. A pack listed with class 1 is `Corrupt(Malformed)`. In a list tree (the MANIFEST's `packs` and `retired` roots and every edge below them) the rule is reversed: the PagePtr must name a class-2 pack, which is a `p-*` file that `packs` does not list, so a list PagePtr into a listed pack is `Corrupt(Malformed)` and a data PagePtr into a class-2 pack is `Dangling(stamp)`. The frame check alone cannot catch a pointer that names another valid page of the same store; the checksum and stamp checks catch it (L47).

**Acyclic by location, on every owning page edge (M3).** For a branch entry's child, and for a map leaf's non-empty collection root, the child's (pack, offset) is lexicographically below its parent's, else `Corrupt(Order)`. A map descriptor that names its own leaf therefore fails. A BlobRef has no order rule: a blob has no edges. The rule applies to the edges open resolves (AP21), never to dead frames. With in-place edits (AP9), a parent's stamp may be lower than a child's, so stamps cannot order the tree.

### 11.3 Keys

**AP5. Logical key and stored suffix.**
- The **logical key** of a collection row is `collection id u32 BE ‖ encoded key`:
  - for a table row, `Key[K]`;
  - for an index entry, `Key[J] ‖ Key[K]`.
- These are exactly the bytes §2.4 calls the data-tree key, and the code's `logical_key` builds them.
- The logical key is the only key form that leaves the store, or that a COMMIT carries:
  - `Read`'s `Point` and `Span`;
  - `Change.key`;
  - a runner `Cover`;
  - `Resume`'s last key (L36);
  - wire `Row.key`;
  - COMMIT ops (§2.4);
  - the limit checks.
- So keys of different collections never collide in a read set, a subscription or a frame.
- The **stored suffix** is what a leaf holds: the logical key without its 4-byte collection id and without its page's prefix. A collection's tree holds rows of one collection only, so the id is implied by the tree.
- The collection map, `streams` and `catalog` trees strip nothing but the page prefix. Their logical keys are as in §2.4, and the collection map's are `collection id u32 BE`.
- A reader rebuilds a logical key as id ‖ page prefix ‖ suffix, in one allocation, only where a logical key leaves the store.

### 11.4 Stamps

**AP6. A stamp names a subtree's logical content.**
- Stamps come from the one writer counter that node ids use today (§2.5): shared by every branch, never rolled back, and reserved before a map operation.
- **A published page never changes its logical content under its stamp.** Logical content means its entries, its values and its edges' stamps (children, collection roots and blobs).
- A page takes a new stamp when a Tx first copies it (AP9). While it is private to that Tx, its in-place edits keep that stamp: no one else can see it, and no cut has written it.
- **An in-place edit of a published page across commits (AP10) draws a new stamp for the page and for every ancestor on its path, up to the root, including the collection-map leaf and map path above a collection's root.** Otherwise the cut, which writes only stamps that have no record (AP16), would skip the edited page, and the edit would be lost once its journal segment retires.
- Relocation, sealing to canonical form, a location refresh in a branch entry or descriptor, and a prefix recomputation keep the stamp.
- **Stamp equality is meaningful only within one open of one store (M6).** Two pages with one stamp in one open have equal logical content. `changes(a, b)` first checks that a and b are Snapshots of the same store and open (`Foreign` otherwise, as built), and only then skips a child whose stamp is equal on both sides without loading it. No cache, table or fast path compares stamps across opens.
- `changes(a, b)` walks the two collection maps in id order. An entry present on both sides with equal root stamps gives nothing. A collection present only in a gives one deletion per row of its tree, and one present only in b gives one insertion per row, each with its logical key. So a `drop_table` costs O(1 + indexes) to commit, and `changes` across it costs the dropped rows, as L23 requires.
- A BLOB's stamp is drawn from the same counter when its value is first written, and is kept across relocation.
- **List pages are stamped on the writer, before the watermark is captured (S1).** A cut (AP16) runs on the writer, which owns the counter. It draws, by checked u64 arithmetic, a fresh stamp for every list page the cut builds or edits, then captures `next_stamp` as the counter after those draws. A list page that class-2 relocation moves unchanged keeps its stamp, as every relocated frame does (T3). So every list page, like every data page the cut writes, has a stamp below the captured `next_stamp`, and every commit after the capture draws a stamp at or above it. A draw past 2⁶⁴ − 1 refuses the cut with `Limit(Ids)`, losing nothing (U4). A cut that is refused or whose I/O fails leaves its drawn stamps consumed: the counter never rolls back. After a crash, open resumes the counter at the selected MANIFEST's `next_stamp`, which is above every list page that MANIFEST reaches.
- **The watermark covers listed packs only (M6).** Every frame in a pack that the selected MANIFEST lists, live or dead, and every list page reached from its two list roots, has a stamp below that MANIFEST's `next_stamp`. A pack the MANIFEST neither lists nor reaches from its list roots is a leftover of an unpublished cut (H2): it may hold higher stamps, open never reads it, and the next cut that reaches its seq replaces it before any MANIFEST lists it. So stamps issued after `next_stamp` by an open that made no cut, and reissued by a later open, never meet a listed frame with the same stamp. Open checks the watermark on every frame of every listed pack (AP21).
- **One location per live stamp (B3).** In the graph that a generation's roots reach, each stamp has exactly one location. Listed packs may also hold dead frames: an old copy of a relocated page or blob repeats its live stamp, and an old copy of a rewritten ancestor may point into a retired pack. Neither is reached, so neither is decoded beyond its frame check and its stamp (AP21).

### 11.5 Per-collection roots

**AP7. `Roots` becomes three page trees: `{collections, streams, catalog}`.**
- `collections` is the collection map: a page tree keyed by `collection id u32 BE`. Its value is a `CollectionRoot`, always inline (vtag 0, vlen 56) whatever the store's inline threshold (H8), and its root page is the leaf's kid for that slot (AP2).
- Every table and every index is one collection, so `share_roots` stays three Arc shares. Dropping or swapping a collection changes one map entry.

**`CollectionRoot`, 56 bytes:**

| Off | Size | Field | Rule |
|---|---|---|---|
| 0 | 36 | root PagePtr | the collection's tree; all zero when it has no rows (and then `rows` and `bytes` are 0, else `Corrupt(Malformed)`) |
| 36 | 8 | rows u64 | exact row count |
| 44 | 8 | bytes u64 | row charge of the collection's rows: the sum of `entry(k, v)` (AP15) |
| 52 | 1 | kind u8 | 1 table, 2 index (as the catalog's kind); other values `Corrupt(Malformed)` |
| 53 | 1 | residency u8 | 0 = Memory. 1 = Tiered is reserved for M3 and is `Unsupported(0x0021)` in M1 (the kind of the page holding it). Other values are `Corrupt(Malformed)`. |
| 54 | 1 | tree u8 | 0 = B+ pages. 1–255 are reserved (a future per-collection Bε-tree) and are `Unsupported(0x0021)`. |
| 55 | 1 | zero u8 | non-zero is `Corrupt(Malformed)` |

**Counts.**
- `count(snapshot, shape, lower, upper)` over a whole table returns `rows` after one map lookup.
- Over a sub-range it descends with the branch entries' `rows`, in O(log N).
- `put` over an existing key leaves `rows` unchanged; a new key adds 1, and a delete subtracts 1.

**`drop_table` (H11).** A table owns one collection for its rows and one per index. `drop_table(mut db, t)` is one schema commit on main, made by one cut (§1.2): it deletes the table's map entry and every one of its indexes' map entries, and deletes the table's and indexes' catalog entries, in one map operation and one catalog operation. Its cost is O((1 + i) × the map's height) page edits for i indexes, independent of the table's row count. No other collection's root stamp changes. Branches and pins keep their own maps, so a branch or pin that still holds the table keeps every row and index range.

### 11.6 Values

**AP8. Inline values and BlobRef.** A stored value (schema number u16 ‖ codec bytes, D1) of v bytes in a table or index row, or in a streams or catalog row, is inline when v ≤ the store's inline threshold, and otherwise a BlobRef. A `CollectionRoot` is always inline (AP7).

**BlobRef, 42 bytes, a leaf entry's value when vtag = 1:**

| Off | Size | Field | Rule |
|---|---|---|---|
| 0 | 8 | stamp u64 | the blob's stamp (AP6), non-zero |
| 8 | 8 | len u64 | v, the value's length: at least 1 and at most `FORMAT_VALUE_MAX` (AP4), else `Corrupt(Malformed)`. A reader never compares it with the threshold or with `VALUE_LIMIT`. |
| 16 | 8 | checksum u64 | xxh3-64 of the v value bytes |
| 24 | 8 | pack u64 | the seq of the class-1 pack that holds the BLOB frame |
| 32 | 8 | offset u64 | the byte offset of that frame's head |
| 40 | 2 | body u16 | the index of this value's body in the page's `blobs` (AP2); 0, 1, … in slot order on disk |

**BLOB payload (kind 0x0022):** `stamp u64 ‖ the value bytes`. A load through a BlobRef checks:
- the frame check;
- the kind;
- payload length = 8 + len;
- the payload's stamp = the BlobRef's stamp;
- xxh3-64 of the value bytes = checksum.

Each failure is `Corrupt(Checksum)`. A pack not listed, or an offset at which no frame starts, is `Dangling(stamp)`; a pack listed with class 0 is `Corrupt(Malformed)`.

- **Blobs are immutable and live in class-1 packs only.** Compaction of class-0 packs never copies a blob.
- A class-1 pack is copied forward only when its own live ratio (AP14 `live` over `bytes`) falls below the same threshold as §3.5's. Then each moved blob's leaf, and that leaf's live ancestors, are rewritten, keeping their stamps (AP16).
- A put of a BlobRef value costs the same COMMIT bytes as an inline one: the COMMIT carries the value (§2.4).
- **The threshold is a store-wide field (H8).** It is the MANIFEST `limits` field `inline`, 0–1024. Genesis chooses 1024. Every later MANIFEST writes the value its open loaded, so a store whose field is t keeps t across cuts and reopens; M1 offers no call that changes it. A reader accepts 0–1024 and never needs the field to read: the vtag says which form an entry has. A writer uses the store's field for every table, index, streams or catalog value it writes into a page, whether new or rewritten. List-tree rows (AP14) and collection descriptors have fixed inline layouts and never consult it, so a store whose field is 0 still writes each 21-byte pack-list value inline (S9). A page copied by a Tx (AP9) keeps each entry's existing form until that entry's value is rewritten. So a store whose field is t < 1024 holds inline values of at most t bytes written under it, and every entry obeys vlen ≤ 1024.

### 11.7 Writes

**AP9. Copy-on-first-write per transaction.**
- A Tx starts from shares of its head's roots (`share_roots`, as today).
- The first write that reaches a page in the Tx copies it: a new `Arc[Page]` with the same image, kids and blobs and a new stamp, together with each ancestor up to the collection root, the collection-map leaf that holds the collection's edge, the map's path, and the map's root.
- A page whose stamp is at or above the Tx's first reserved stamp is **private** to the Tx.
- A later write to a private page edits its `image` in place, through `arc.with_unique` (MEM-15) on each Arc of the path:
  - insert into free space, else repack (AP3) or split;
  - delete by marking the entry dead (`dead` += its size) and removing its slot;
  - update an inline value of the same length in place, else as a delete and an insert.
- If `with_unique` returns `None` on a private page (a body shared it, for example by taking a view that is still alive), the write copies again with a new stamp. The stamp test only selects candidates for an in-place edit; `with_unique` decides soundness.
- **A failed, aborted, stopped or refused body leaves the head's pages untouched.** The head never shared a private page, and FAIL-16's `isolate` undoes nothing, so nothing needs undoing (L39).
- At commit the Tx's roots become the head's. Nothing is copied or sealed at commit: the private pages simply stop being private, because the next Tx's first reserved stamp is above them. They are published unsealed (AP2).
- **F2's streams-only publication (H13).** A body that returns a typed `Error` after a kept reply commits its streams writes and CALL on the head's data and catalog roots (§1.3): the published `collections` and `catalog` roots are the head's, unchanged, and only the `streams` root is the Tx's.

**AP10. In-place edits across commits (H4).**
- **A `run` body never edits a page it did not copy.** The head holds a share of every page the body sees, so `with_unique` would return `None`. Even if it did not, an in-place edit of a head page followed by a failure would change the head with nothing to restore it (FAIL-16).
- An edit across commits is allowed only for a writer-side apply that runs outside `isolate`. Its condition is **path uniqueness held continuously**: it takes `with_unique` on the root holder's Arc, and inside that on each Arc of the path down to the page, and holds every level until the edit and the re-stamping of the path are done. A failed check at any level copies that page and everything below it on the path, with new stamps, and the edit continues on the copies. No live Snapshot, pin, branch, follower root or other Tx then reaches an edited page, because each of those would hold a share on the path.
- The edit re-stamps the page and its path, including the map leaf and map path above a collection root (AP6).
- **A live apply on a published head runs only once nothing can fail.** Every fallible step comes first: admission (AP15), publication checks, the stamp reservation, and every allocation the apply can make (new page buffers for splits and repacks, list growth of `kids` and `blobs`, new blob Arcs). Only when all have succeeded does the apply edit the head's pages, and that phase allocates nothing and cannot fail. A writer-side apply that cannot do this edits private copies and publishes them by one root swap.
- The writer-side applies are:
  - journal replay in `open`, before `open` returns a Db (a failure fails the open and discards its state, so it may edit in place directly);
  - bulk build (AP13), into pages it made and has not published;
  - commit-time apply of a Tx's op list on the writer, which is the conditional fix inside the cutover slice (M1-DESIGN §6), under the plan-then-apply rule above.

**AP11. Index writes** keep J1's rule: an unchanged encoded index key writes nothing, so that index collection's pages and root stamp are unchanged by the commit. An index entry's value is empty in M1; the page format already carries a value field for M2's covering indexes.

### 11.8 Reads

**AP12. `View[Bytes]` reads under a page guard (M4).**
- `fetch_bytes(tx, t, k)` returns `Option[View[Bytes]] in tx`, and `get_bytes(s, shape, k)` returns the same `in s`. The view is the row's stored value (schema number ‖ codec bytes), raw: an old-schema row's view holds its old bytes. For an inline value it is a `list.slice` of the page's `image`; for a BlobRef it is the body in `blobs`.
- The anchor is the Tx or Snapshot, which holds the page's Arc. By OWN-8 the view freezes its anchor for its extent: the checker refuses a `put` through that Tx while the view lives, and the view cannot escape its extent.
- **The owned `fetch` decodes from that view.** `std.codec` takes `Bytes` parameters, which accept a `View[Bytes]` (OWN-7). So an owned read copies nothing before decoding, and decode materializes the owned `V`.
- A row under an older schema number is transcoded by that number's resolved plan at each owned read (`row_value`, as built) and by `published_value`. The raw paths (`fetch_bytes`, `get_bytes`, `scan_bytes`, `Change.value`) return the stored bytes, as `Change.value` does today (H13). A read never writes a transcoded row back.
- A read records its `Point` or `Span` on logical keys (AP5), exactly as §1.3 says.
- Typed zero-copy views of a user `V` (copying scalars, borrowing byte fields) need view types from `std.codec`. They are M2 codec work, not M1.
- **`scan_bytes`** returns a new type, `ViewRows`, declared `pub opaque scoped type` as the built `RangeRows` is (OWN-22), distinct from the built owning cursor `ByteRows` (which stays `send` and keeps its Arc-backed rows). A `ViewRows` is scoped to its Snapshot and is not `send`. `next_view(mut c)` advances it and returns `Some(key)`, the next row's logical key as a fresh owned `Bytes`, or `None` at exhaustion; `value_view(c)` returns the current row's value as `View[Bytes] in c`. Calling it before the first `next_view`, or after `next_view` returned `None`, is a program error that traps, as an out-of-range `list.get` does: the caller holds the cursor's state and the API gives no current row there. Because `next_view` takes `mut c`, the checker refuses an advance while a value view of the previous row lives (OWN-8). The handle check (`Stale`, `Foreign`) happens when `scan_bytes` builds the cursor; `next_view` returns `Error` only for `Poisoned`, and then the cursor does not advance: its current row is the one before the call. The fresh key's allocation is the runtime's, as the built `next`'s owned key is (`copied`): it is not an `Error` case.

### 11.9 Bulk build

**AP13. Bottom-up from sorted entries (H5, H6).**
- `migrate_build`, index backfill and import build each new collection's tree from its entries in ascending logical-key order, with no duplicates.
- **`migrate` sorts its output.** It evaluates the callback exactly once per source row, in source order, and keeps each returned (destination key, value) as an encoded record: the destination logical key and the stored value. A callback may reverse, drop or collapse keys, so the records are then sorted by logical key. Two records with one key make the migrate `Exists(the target name)`, as built, and nothing is published. Each index of the target is built from its own (J ‖ K) entries, collected from the sorted records by its extractor and sorted independently.
- **Workspace, charged while it is collected (H5, R2).** After the settle, the migrate keeps a running figure W, starting at A + 126 × (the collections it creates) + entry(k, v) for every catalog row it writes (the target table's row and each target index's row, as the built `migrate_build` charges them). Each record it keeps adds entry(dest key, value) + 8 (its sort-index slot), and each index entry it extracts adds entry(J ‖ K, empty) + 8. If W would pass `resident_limit`, the migrate stops collecting and is `Full(Resident(Capacity{requested: W, available: resident_limit}))`, with its workspace freed and nothing published. So the whole workspace, including every index's, is admitted before it is held. W bounds the workspace only. **Publication admits the exact account of the proposed roots:** after the build the migrate settles over the roots it would publish (main with the new collections and catalog rows and without the source's, plus every other participating root, so a pin that retains the source keeps the source's versions counted) and gets E. If E passes `resident_limit` the migrate is `Full(Resident(Capacity{requested: E, available: resident_limit}))` with nothing published; otherwise the roots publish and A = E. A migration with no records still charges its descriptors and catalog rows (L49).
- **Cost.** Collecting and sorting n records is O(n log n) compares. Packing already-sorted records of bounded size into pages is O(n) work and one Arc per page.
- **Leaves.** Fill leaves greedily: append an entry while the leaf's canonical size with it stays ≤ 4096, or while the leaf is empty; otherwise close the leaf and start the next.
- **Upper levels.** One branch entry per child of the level below. The separator of child i ≥ 1 is the shortest separator between child i − 1's last key and child i's first. Fill branches greedily: append an entry while the branch's canonical size with it stays ≤ 4096, or while it holds fewer than two entries. Each group carries its first and last logical keys (a leaf's own; a branch's from its first and last child). Only after the last-group rule below has fixed the groups are the next level's separators computed: group j ≥ 1's is the shortest separator between group j − 1's last key and group j's first key. Each branch stores the empty separator only in its own slot 0, and the empty string is never passed up as a separator (R8).
- **The level's last group.** If the greedy pass leaves the level's last branch with one entry: when the previous branch holds at most two entries, the last entry joins it (at most three entries, so legal); otherwise the previous branch holds at least three entries and is at most 4096 bytes, and its last entry moves into the last branch, which then holds two entries (legal) while the previous one keeps at least two and shrinks (legal). A level of one entry has reached the root.
- Repeat until a level has one page, which is the root.
- **Stamps** are drawn in build order: the leaves left to right, then each level left to right; a blob takes its stamp just before its leaf's.
- The same input and starting counter give the same pages and stamps.
- The bulk build reserves its stamps before building (2·n + b + 1, as §11.1's install, by checked arithmetic), so a refusal cannot return them. The build is all or nothing.

### 11.10 MANIFEST and the reserved `base` root

**AP14. MANIFEST payload (kind 0x0031):**

| Field | Encoding |
|---|---|
| generation | u64 (equals the HEADER's seq) |
| limits | key u32 = 4096, page u32 = 4096, inline u32 (0–1024; genesis writes 1024, later MANIFESTs keep the loaded value, AP8), transaction u32 = 16,777,280. A key, page or transaction field of any other value, or an inline above 1024, is `Corrupt(Malformed)` (V5 restated) |
| next_stamp | u64: every frame in a listed pack, and every list page, has a lower stamp (AP6) |
| next_branch | u32 |
| next_table | u32 |
| next_pack | u64 |
| first_live | u64 |
| prev_first | u64 |
| account | u64: the exact logical account (AP15) at this cut |
| packs | PagePtr (36 bytes): the root of the **pack list**, an AP3 page tree whose pages are in class-2 packs; all zero when no pack is listed. Each row: key = seq u64 BE (8 bytes); value, inline, 21 bytes = class u8 (0 pages, 1 blobs; other values `Corrupt(Malformed)`), frames u32 (every PAGE or BLOB frame in the pack, live or dead), live u64 (payload bytes of the frames live at this cut), bytes u64 (payload bytes of all its PAGE or BLOB frames, live or dead; live ≤ bytes, else `Corrupt(Malformed)`) |
| retired | PagePtr (36 bytes): the root of the **retirement list**, an AP3 page tree in class-2 packs, all zero when empty. Each row: key = seq u64 BE, value empty |
| branches | u32 count; each: id u32, name str (0–255 bytes), parent u32, forked_at u64, version u64, at u64, collections PagePtr, streams PagePtr, catalog PagePtr, base (below). Ascending by id; main is id 0 and always present. 143 + n bytes each, with n the name's length. |
| pins | u32 count; each: name str, branch u32, version u64, collections PagePtr, streams PagePtr, catalog PagePtr. Ascending by name. 122 + n bytes each. |

- `next_branch`, `next_table`, `next_pack`, `first_live`, `prev_first`, the meaning of `retired` and the list order rules (`Order`) are as §2.6. List rows ascend by seq within a page and across pages by AP3's separators, and no seq is in both lists (`Corrupt(Malformed)`).
- `next_stamp` replaces `next_node`.
- A pack-list row gains `class`, `live` and `bytes` beside §2.6's fields. `live` and `bytes` count the same thing, PAGE or BLOB payload bytes, with frame heads, the pack's HEADER and frames of other kinds excluded from both, so a pack's live ratio is `live / bytes` in one unit (M8). The `account` field is new.
- Open checks, per listed pack, that the frame count and the payload byte total it reads equal `frames` and `bytes` (`Corrupt(Malformed)` otherwise). It does not check `live` or `account`: they are our writer's output.
- **The list trees (N8, choice 6).** `packs` and `retired` are page trees of AP3 pages with 8-byte keys, stamped on the writer when it plans the cut (AP6, S1). `packs` is edited by AP3's split, merge and borrow rules (so its occupancy and height bounds are AP3's); `retired` is rebuilt whole by the bulk build (AP13) at every cut, never edited (AP16, S2). Their pages are written only into **class-2 packs**: each cut writes its changed pack-list pages and its rebuilt retirement list, children first, into new class-2 packs that roll at `pack_bytes` like every class (H1, S4), syncs them with its other packs, and only then publishes the new roots in the MANIFEST under the existing rename and directory barrier. A class-2 pack is never a `packs` row, so a list's storage needs no entry in itself, and no MANIFEST both reaches a class-2 pack and retires it (AP21, S6). A class-2 pack is live while the selected MANIFEST, or a leased generation, reaches a list page in it, and the writer knows its live bytes from its list pages' records and its `bytes` from writing it, open from streaming it (AP16, AP21); an EXPORT copies the class-2 packs its MANIFEST reaches, and a lease retains them. A crashed or failed cut's class-2 pack is reached by no MANIFEST, and its seq is at or above the published `next_pack`, so open ignores it and removes it as §2.6 removes any leftover (M6).

**The reserved base root:**
- `base` is `tag u8`. Tag 0 means "no merge base" and contributes no edge to any traversal. Tag 1 is reserved for a later three-way merge: it would be followed by `version u64` and three PagePtrs, the fork point's roots, retained as a pin's are. In M1 tag 1 is `Unsupported(0x0031)`, and other tags are `Corrupt(Malformed)`. Open refuses tag 1 while decoding the MANIFEST, before it resolves any root, accounts anything or removes any file.
- M1 writes 0 for every branch. A later writer that sets tag 1 makes an M1 reader refuse, which is correct, because that reader would not retain the base's pages. So the base needs no format change later.

**`live`, `bytes` and `account`** are written by every cut from the writer's location table (AP16). They exist so that M3's lazy open can select compaction (`live / bytes`, with no length source beyond the MANIFEST) and start the account without walking the trees, and L42 compares `account` with a hand sum.

### 11.11 Accounting

**AP15. The logical account: distinct row versions (B4, B5).**
- **Participating roots** are every branch head's three trees, every named pin's three trees, and the roots of a fork or pin being admitted. A Snapshot that only a reader holds does not participate (§1.11), and neither does a base root (tag 0 has none).
- **A row version** is a triple (tree, logical key, stored value bytes), where tree is `streams`, `catalog` or a collection id. Two roots hold the same version exactly when they hold the same key in the same tree with byte-equal stored values. Equality is decided by stamp where both values are BlobRefs with one stamp, and otherwise by length and then bytes, never by checksum alone.
- **The logical account** is
  `Σ over the distinct row versions held by any participating root of entry(k, v) + 126 × (the number of distinct collection ids held by any participating root)`,
  where `entry(k, v) = 62 + r8(|k|) + r8(|v|)`, |k| is the logical key's length (AP5), and |v| is the stored value's full length, inline or BlobRef. The 126 is a collection descriptor's charge, `entry(4, 56)`, counted once per id however many roots hold it and whatever their `CollectionRoot`s say.
- **It depends only on the roots' logical contents.** Page partition, stamps, copy-on-write history, whether a page is private, sealed or loaded, and how many leaves hold a row do not enter it. So publication, journal replay, a reopen that replays a fork and its commits without a cut, a captured export's recovery and a later cut all give the same figure for the same set of roots (L50).
- `CollectionRoot.bytes` and the branch entries' `bytes` are per-tree row charges (the sum of `entry` over the subtree's rows), which are partition independent too.
- **The admission figure.** The writer keeps one u64, the admission figure A, with the invariant **A ≥ the logical account of the current participating roots**. A counts no frame bytes (choice 5).
  - **Exact mode:** while main is the only branch and there is no pin, **and A has been settled since that became true**, A is exact after every commit. When the last pin or extra branch goes away (`unpin`, `delete_branch`, `adopt`), the store stays in conservative mode until its next settle, which with one root is the one-root count below. A put of (k, v) adds entry(k, v) and, over an existing key, subtracts the old row's entry; a delete subtracts the old row's entry; creating a collection adds 126 and dropping one subtracts 126 and its `bytes`.
  - **Conservative mode:** otherwise each commit adds entry(k, v) for every put and 126 for every collection it creates, and subtracts nothing.
  - **Why the conservative charge is enough.** A commit changes one head. For each put (k, v), the union gains at most the version (tree, k, v), worth entry(k, v), and loses or keeps the head's old version of k. A delete gains nothing. Creating a collection gains at most one id. So the logical account after the commit is at most the account before plus the commit's charge, and the invariant holds. In particular **a delete-only commit is never refused by admission**, on disk or in memory: it adds nothing to A, and its frame is not charged against `resident_limit`.
  - The same argument covers every other path that adds rows or roots, each charged before it publishes: `admit` and `reply` (streams puts); `create_table`, `create_index` and `table`'s schema-number append (catalog puts, plus 126 per new collection); `migrate` and index backfill (their workspace and then the new collections' row charges plus 126 each, AP13); the install graft (§11.1); a live-head fork (no new version: its roots are an existing head's, so its charge is 0); `adopt` (main takes b's roots and b is deleted, so the union cannot grow: charge 0); `unpin` and `delete_branch` (charge 0). A cut-path fork and a `pin` of a reader-held or older Snapshot add a root that may hold new versions, so they settle (below) and admit the exact figure before their cut, as 1616's `fork` does with `state_account`.
- **Frames and journaling memory (N2, N3, R1).** Each journaling buffer has its own fixed bound, and none is admitted against `resident_limit`:
  - **Frames handed over.** F is the sum of the lengths of the COMMIT and FORK frames handed to the journal worker and not yet covered by an ack. A frame of f bytes is charged to F once, when the worker accepts it at handoff, not when the body encodes it: the built `commit_prepared` adds to `db.inflight` before the send, and M1 moves that addition to the handoff, so a waiting frame never counts in its own wait. An ack releases the newly covered prefix once, by cumulative acknowledged bytes, so the release is correct across a rotation, as the built `journal_ack` counts. Before handing over f bytes, if F + f would pass `FRAME_LIMIT` = 16,777,296 (the largest legal frame, 16 + the transaction limit), the writer receives acks until F + f fits. **Progress:** every legal frame has f ≤ `FRAME_LIMIT`, so F + f can fail to fit only while an earlier frame is outstanding; the worker syncs every group it takes, a lone frame included after an empty probe, and that ack (or `Failed`) needs no new frame, call or barrier. A worker failure while the writer waits poisons the Db. `FRAME_LIMIT` is backpressure only: the frames in F are never a reason to refuse anything, and the journal cap below is the one frame-related refusal.
  - **The frame being built (S3).** The writer computes a frame's exact length before encoding it, allocates one output buffer of that length, encodes the head and payload directly into it and takes the checksum over the buffer's payload slice. No separate payload buffer exists. A multi-op COMMIT still writes its ops in key order, as built, but never clones a key (the built `encode_data_ops` clones every key into a list and sorts it). **The ordering is by index (T7).** A collection of views cannot be built (OWN-19), so the encoder does not collect key views. **The M1 Tx keeps one op per distinct key (U10)** in one indexable `List[Op]`, and its key map holds each key's slot, so a second write of a key replaces its op in that slot. The key is held twice, in the op and in the map, and both are Tx memory, charged as any Tx allocation is. At seal that list is frozen (sealing moves it and copies nothing), and the encoder sorts positions into it: it asks `list.try_reserve` for n slots of a `List[u64]` before it allocates the frame buffer, fills n of them with 0 … n − 1, and sorts them in place with an unstable sort whose comparison reads the two ops' keys through views of the frozen list, borrowed for the comparison only. No `list.sort_stable` scratch is used. A refused reservation is the commit's `Refused(Allocation(c))`, before anything is sent. **Occupied and granted are separate figures (v2.40 C-11).** An op encodes to at least 9 bytes (tag, u32 length, a 4-byte id), so 9n < `FRAME_LIMIT`, and the index list's **occupied** bytes, 8n, are below 8/9 × `FRAME_LIMIT` = 14,913,152. That fixed bound is about occupied bytes only. The list's capacity is the G slots `try_reserve` grants, with G ≥ n. core.md bounds G below and not above (n = 1 can be granted 4 slots, 32 bytes), so no fixed bound is claimed for the capacity. It is charged, measured and observed as 8 × G (MEM-24). Streams ops already iterate their ordered map and need no index list. So the sole writer holds at most one unsent frame buffer, at most `FRAME_LIMIT` bytes, plus the index list (8n occupied bytes, below 14,913,152, in 8 × G bytes of capacity), plus fixed scratch. Copying a key into the list or anywhere else stays forbidden. The built `encode_frame` → `disk_seal` copy, which holds the payload and the sealed output together, is replaced by the cutover slice.
  - **The worker** holds its group buffer, max(`group_bytes`, 16,777,296) bytes, and its fixed END scratch (96 bytes for END, 33 for its checksum binding, a name of capacity 32), all allocated at open (§3.1, Q1a's worker slack) and fixed for the open.
  - **Units (T7).** F and `FRAME_LIMIT` count **wire lengths**: frame head plus payload, as written. The buffer figures above are **capacities**. Neither is the MEM-24 **charge**, which rounds each block up to 8. A frame buffer of length f charges r8(f), so the frames in F charge at most F + 7 per frame: two legal queued frames of 8,388,649 and 8,388,647 bytes fill F to exactly 16,777,296 and charge 8,388,656 + 8,388,648 = 16,777,304. The index list charges 8 × its granted capacity. The group buffer charges r8(max(`group_bytes`, 16,777,296)). The worker's scratch capacities 96 + 33 + 32 = 161 charge 96 + 40 + 32 = 168. The journal channel (capacity 2 × `group_commits`), the journal ack channel (capacity 3 × `group_commits` + 3, Y1) and the `written` channel (capacity 2) each charge capacity × item size + 64 at open (MEM-24, §3.3), from the opener's limit. The checkpoint channel is deleted with the checkpoint task (U11).
  - **Segment reads are streamed, and are not frame credit (v2.40 C-4).** Recovery (§3.4, in AP21's open order) and `recorded` (W2) read each segment through one **bounded segment reader**. It has a fixed 65,536-byte chunk buffer, carries the xxh3-64 state across chunks, and has one frame buffer of at most `FRAME_LIMIT` bytes for the COMMIT or FORK being decoded. A skippable extension frame is checked in chunks against §2.7's `UNKNOWN_LIMIT` and is never buffered whole, as AP21 step 0 streams class-2 files. The reader keeps every built rule:
    - the HEADER check;
    - each frame's checksum;
    - the END file's offset, and the successor segment's `prev_end`;
    - the last segment's torn-tail rule: a frame that fails its check at the tail ends the replayed prefix, and anywhere else it is `Corrupt`.

    Recovery holds the reader in the opener's limit, and `recorded` holds it in its caller's limit, even during a live open. So each holds at most 65,536 + `FRAME_LIMIT` = 16,842,832 bytes of read buffers, whatever the segment's length. Neither is bounded by F. **A segment's length has no fixed bound.** A cut refused before its rotation leaves the same segment current, so COMMITs keep filling it up to J, or past J after a reopen with a lower J. Extension frames and a torn tail add bytes that U does not count. Bare rotation is deleted.
  - **The journal cap (C3; v2.40 C-3, C-4, C-5, C-10).** U is the **accepted** journal: the wire lengths of the COMMIT and FORK frames that the writer has handed over (at open, replayed) in the segments at or above the selected MANIFEST's `first_live`, which no published cut covers. HEADERs, ENDs, extension frames and a torn tail are not in U. The writer counts U from the lengths it hands over: + f at each handoff, 0 when a cut publishes (AP16 step 5), and at open the sum of the replayed frames' lengths, by saturating u64 addition, so a sum past 18,446,744,073,709,551,615 stays there. It is never computed from acks or file sizes. J is `journal_limit` (Options; default 268,435,456 = 4 × the default `checkpoint_bytes`). U is the publication limit: the cap below is the only frame-related refusal.
    - **When the check runs (C-10).** Each frame producer checks once, when its frame's final length f is known. That is after the body, if any, has returned and the outcome that decides the frame (a commit, a kept reply, `conclude`'s fault) is fixed. It is before the resident settle and the `resident_limit` check, before anything is published or sent, before the wait for `FRAME_LIMIT` room, and, for a live-head fork, before the id is taken (AP17). It never waits for a cut. The body's private pages have already taken their stamps by then, and AP6 never rolls them back, so a refused COMMIT may consume private stamps. It leaves the heads, U, F, A, the event stream and the provider's frame log unchanged. The body's steps were charged while it ran, as built.
    - **The comparison, with no overflow (C-5).** If U > J, the frame is refused. Otherwise, if f > J − U, it is refused. Otherwise it is admitted, and U + f ≤ J after it: equality is admitted. Neither comparison can overflow, because J − U is computed only when U ≤ J. The refusal's cause is `Journal(Capacity{requested: r, available: J})`, where r = U + f when that sum is at most 18,446,744,073,709,551,615 (u64::MAX) and r = u64::MAX otherwise. The saturated r is a diagnostic: it says only that the frame does not fit. Worked:
      - J = 16,781,392, U = J, f = 59: U ≤ J and 59 > 0, so it is refused with r = 16,781,451.
      - J = u64::MAX, U = u64::MAX − 10, f = 59: U ≤ J and 59 > 10, so it is refused. U + f passes u64::MAX by 49, so r = 18,446,744,073,709,551,615.
      - J = 16,781,392 with `checkpoint_bytes` = 4,096, then a reopen over U = 16,781,392 with `checkpoint_bytes` = 4,095 and J = 16,781,391, exactly its floor, so the options step admits it: U > J, so every frame is refused, with r = U + f (16,781,451 for a 59-byte delete).
    - **Each producer refuses through its own result (C-3; ruling (a)).** The cause is the same typed value everywhere (ruling (b), below):

      | Producer | Its frame | Refused at the cap | What stays |
      |---|---|---|---|
      | `run`, `run_on` | the COMMIT, F2's and F4's streams-only frames included | the outcome `Refused(Journal(c))`. A resident refusal is `Refused(Resident(c))`, and an allocation refusal `Refused(Allocation(c))` | nothing is published |
      | a committing `admit` (`Admitted`, or an `Abandoned` re-commit) | its streams COMMIT | `Error(Full(Journal(c)))` | the stream as it was |
      | a live-head `fork` | its FORK | `Error(Full(Journal(c)))` (AP17) | `next_branch` unchanged |
      | `serve` | the data-and-reply COMMIT, or a kept reply's streams-only COMMIT | `Error(Before(Full(Journal(c)), None))` | Nothing is stored. The position stays Admitted, and the Completion and record are dropped, as for any `Before`. Admitting the same position again in the same open returns `Abandoned(c′)` with a fresh Completion, and the runner runs the body again, as after a crash. |
      | `conclude` | its streams-only reply COMMIT | `Error(Before(Full(Journal(c)), Some(p)))`, with the Pending returned unchanged | Nothing is stored and the position stays Admitted. A later `conclude` with p, after a cut publishes, writes p's record, CallInfo, base and settle, so the concluded record keeps the original context. A caller that drops p is in `serve`'s case. |

      `ServeError` becomes `Before(Error, Option[Pending]) | After(Error)` (§11.16). `Before` still means nothing was published and the position is as it was. Its Pending is `Some` only when `conclude` is refused before publication, because `conclude` takes the Pending and is the one producer that cannot rebuild its input. `serve` always returns `None`, because a fresh run is its retry.
      - **Resident settlement is unchanged.** `Served.NoRoom(p, cause)` keeps its built cases: cause `Resident(c)` for a resident admission refusal, and `Allocation(c)` where the built `serve` refuses an allocation there. It is never `Journal`. The runner's third-refusal rule (RUNNER-SPEC H2, http.md K-8) and settle 2 (`Unknown(AdmissionRefused)`) stay attached to `NoRoom` alone. So a journal refusal never counts toward a claim's refusals and is never written with settle 2. The cap check comes before the resident check, so a frame refused at the journal is never also a resident refusal.
      - A delete-only commit is refused at the cap like any frame. It is never refused by admission, and the cap is not admission. Cut-made operations (`checkpoint`, schema commits, `migrate`, `pin`, `unpin`, a cut-path fork, `adopt`, `delete_branch`, install) write no journal frame and are not checked. They are the way out. A `memory` store has no journal: U = 0 and nothing is checked.
    - **What the caller sees (ruling (b)).** The cause names the resource that refused, and is built at the refusal site:
      - `Journal(c)`: the requested r above, with available J;
      - `Resident(c)`: the proposed logical charge, with its ceiling `resident_limit`;
      - `Allocation(c)`: the allocator's actual request and its room.

      `accounts(db).journal` is U when it is called. A caller no longer compares it with the limits to learn which resource refused.
    - **The options step and the floor (C-5).** Open's options step (§3.4 step 0, restated by AP21's open order) runs before LOCK, and before anything is read or written. It is a pure function of `Options`, so a law can check it without creating a store or a directory:
      1. If `checkpoint_bytes` > u64::MAX − `FRAME_LIMIT` = 18,446,744,073,692,774,319, the floor cannot be represented: `Full(Journal(Capacity{requested: 18,446,744,073,709,551,615, available: J}))`.
      2. Otherwise, if J < `checkpoint_bytes` + `FRAME_LIMIT`: `Full(Journal(Capacity{requested: checkpoint_bytes + FRAME_LIMIT, available: J}))`.

      At the defaults the floor is 67,108,864 + 16,777,296 = 83,886,160 ≤ 268,435,456. At `checkpoint_bytes` = 18,446,744,073,692,774,319 the floor is exactly u64::MAX, and only J = u64::MAX opens. One more byte of `checkpoint_bytes` is case 1.
    - **No refusal while threshold cuts publish, and its precondition (C-5).** Take an uninterrupted interval of one open that starts at U = 0, right after a published cut or at a fresh store. Every threshold cut in it publishes. Before each frame, U ≤ `checkpoint_bytes`, because a crossing is strict: the previous frame either left U at or below `checkpoint_bytes`, or crossed it and its cut set U to 0. Every legal frame has f ≤ `FRAME_LIMIT`, so U + f ≤ `checkpoint_bytes` + `FRAME_LIMIT` ≤ J, and the frame is admitted. The proof covers nothing else. In particular it does not cover an interval that starts from a recovered U.
    - **The attempt counter (C-5).** The writer keeps `since`, the bytes handed over since the last cut attempt. `since` is 0 at open, counted from that open's initial rotation (§3.4 step 8). Each handoff adds f to it. Every cut attempt resets it to 0, whether the cut publishes or is refused, even when step 1 refuses before rotating. A threshold cut is attempted when `since` passes `checkpoint_bytes`. U is reset only when a cut publishes. So after an open, U includes the recovered suffix while `since` starts at 0, and a recovered U close to J can refuse the next frame before any automatic attempt. Worked, with `checkpoint_bytes` = 4,096 and J = 16,781,392:
      1. A 4,096-byte COMMIT makes U = `since` = 4,096. That does not pass 4,096, so no cut is attempted.
      2. A 16,777,296-byte COMMIT (two puts, see L50 (e)) makes U = 16,781,392 = J, admitted at equality. `since` passes 4,096, so a cut is attempted, and here it is refused.
      3. The writer crashes after that frame's sync. Reopen recovers U = J and sets `since` = 0.
      4. The next one-key delete (59 bytes) is refused with r = 16,781,451.

      The way out is `checkpoint` or any cut-made operation. Open itself needs no cut.
    - **Three attempts at the defaults.** Suppose every cut is refused, nothing makes an explicit attempt, and there is enough admissible traffic after each refusal. Each attempt fires when `since` passes 64 MiB, so it fires at most 67,108,864 + 16,777,296 = 83,886,160 bytes after the previous one. Three attempts fit, because 3 × 83,886,160 = 251,658,480 < 268,435,456 = J. A fourth would need more than J. These are attempts, not fixed offsets. Frames that reach exactly 67,108,864 bytes and then add one 16,777,296-byte frame attempt at 83,886,160, 167,772,320 and 251,658,480 bytes, just past 80, 160 and 240 MiB. A large frame refused below J does not close the writer to a smaller frame that still fits.
    - **At the cap** no frame is handed over, so no threshold cut fires. The way out is a `checkpoint` call, any cut-made operation, or a reopen with a larger `journal_limit`. Open never refuses for U: it recovers whatever U it finds, which may exceed J, and then every frame is refused until a cut publishes.
    - **Disk (C-4).** The uncovered journal holds U accepted bytes, at most J (or the U that open recovered). Beside them are extension frames and a torn tail, which U does not count, and one HEADER and one END per segment. A segment is made by each open's initial rotation (§3.4 step 8) and by each cut that passes step 1, whether it publishes or is refused. So at fixed U, repeated opens or refused cuts add HEADER/END pairs without limit. M1 states no physical-disk bound for the journal directory. Such a bound would be a separate contract with its own option and refusal, not U. An export lease keeps the segments it captured, at most the uncovered journal at its capture. Packs hold the live frames plus dead copies awaiting compaction, about 2 × live (GUESS), plus the leftovers of the largest refused cut (H2).
  - **A cut's memory** is the writer's, allocated fallibly while it is paused and freed when the cut ends (AP16).
  So, in wire lengths and capacities, journaling holds at most 2 × `FRAME_LIMIT` + the index list's granted capacity 8 × G (its occupied 8n bytes are below 8/9 × `FRAME_LIMIT`) + the group buffer + the worker's 161 bytes of scratch + fixed encoder scratch, beside the segment reader and a cut's memory. In charges the same parts are at most 2 × `FRAME_LIMIT` + 7 × (the frames in F, plus one) + 8 × G + the group buffer + 168, plus the three channels. A `memory` store has no frames.
- **Settle (AF1, I4 restated).** A settle computes the exact logical account of a set of roots and replaces A by it. Every commit path settles before it refuses for the limit, as §1.11 says: when the conservative figure would pass `resident_limit`, it computes the exact logical account of the roots the commit would publish (the walk below, over the proposed head and every other participating root) and admits or refuses on that, so a `Refused` or `Full` rests on the exact figure, and A becomes that exact figure when the commit is admitted. A `memory` store settles the same way and at the same cuts.
- **A cut's settle (N1).** The writer is paused for the whole cut (AP16), so no commit, fork, pin or deletion runs between the cut's settle and its publication. When the cut publishes, A = X, the exact account of its generation's roots, and `exact` is true. A refused cut leaves A as it was. The built `apply_cut_ack`'s rules for an ack that arrives after newer charges have no M1 case.
- **How the exact account is computed.** In exact mode it is A. **With one participating root (N10)** it is the streams and catalog trees' row charges (each the sum of its root page's branch-entry `bytes`, or of its rows when the root is a leaf), plus the sum of `CollectionRoot.bytes` over the collection map's leaves, plus 126 per collection: a walk of the map's leaves, O(collections), and of no collection's pages. The map's branch entries' `bytes` cannot replace that walk: they are the map's own row charge (126 per descriptor), not the collections' contents. Otherwise the cut walks all participating roots together, tree by tree, in key order: the n-way form of `changes`' walk. Over a key range where every root that holds the tree reaches one page by one stamp, it adds that page's row charge from its parent entry's `bytes` (or the `CollectionRoot`'s `bytes`) without loading it. Elsewhere it descends, and at the leaves it counts each key's distinct values once. Descriptors are counted by id from the collection maps. The walk visits only pages that some participating root does not share, plus one path per shared subtree, so its cost is proportional to the divergence among the roots, not to their size. Within one open stamps decide sharing (AP6).
- **Open.** `open` computes the exact logical account of the recovered roots (the MANIFEST's branches and pins, then the replayed COMMIT and FORK frames). The recovered state is the state after some admitted commit or fork c (L3). A was at most `resident_limit` after c, and A was at least the logical account of c's roots, which open computes exactly because the account depends only on the roots' contents. So **a durable history that fit at publication never reopens `Full`** by admission under the same `resident_limit`. An open whose own allocations (the recovered trees, the location table, AP16) are refused is `Full(Allocation(c))`, as any open allocation is (U11), and so is an open below the journal-cap floor.
- **What it bounds, and what it does not.** The logical account bounds distinct row versions, which is the store's data. It does not bound page memory under divergent roots: a leaf copied by one root and kept by another holds its unchanged rows twice in memory and once in the account, and two byte-equal large values stored on different branches under different blob stamps are one version in the account and two bodies in memory (R1). Let R be the number of participating roots. Each root's trees hold each version at most once, so page memory is at most R × the page memory of one tree holding every version. M1 states no numeric ratio between a tree's page memory and its row charge: one-entry leaves, under-T/4 pages a merge could not make legal, and unsealed images up to 16,384 bytes are all legal. Page memory is reported (the resident account below), never admitted. This replaces §1.11's node-union figure, which bounded node memory and depended on history. Reversible: the account is writer state and the MANIFEST `account` field's meaning is set by this clause, so a different unit is a spec change before the cutover lands, and a format change after.
- **Resident account (M2):** the sum of
  - each distinct page's image capacity, for the pages reachable from the Db's heads, its pins and the two list roots of the selected generation (at open, the selected MANIFEST's), each page once by stamp within this open (S9);
  - each distinct blob body's length, each body once by blob stamp, however many leaves name it;
  - the writer's location table (AP16), at its MEM-24 charges: the record map's and the pack inventory's OMap nodes and rows, each record's out-edge stamps (8 per edge) and incoming list (its entries' storage), and spare capacity in any of them. These are charges, not a reporting unit: the writer's own limit pays them, and they admit nothing against `resident_limit`.
  Reader-only Snapshots are excluded: the Db cannot enumerate them, and their memory is the holder's. The figure is computed by the writer when each cut publishes and at open, and `accounts(db)` returns the latest one. It admits nothing in M1. It is the figure M3's cache budget bounds, once M3 can evict.
- **`accounts(db)` reports five numbers, synchronously:** `logical`, the admission figure A; `exact`, whether A is known to equal the logical account (R7): true in exact mode, and after a settle or a published cut, which sets A = X, until a publication whose exact change is unknown clears it. A conservative commit clears it (a delete subtracts nothing though the union may shrink), as do `unpin`, `delete_branch` and `adopt`; a live-head fork keeps it, because it leaves the set of versions unchanged; `frames`, F, the bytes of frames sent and not acked; `journal`, U, the journal bytes that no published cut covers (the journal cap's figure); `resident`, the resident account as of the last published cut or open. The writer holds all five, so no call waits on a task.
- The two accounts separate cleanly because a page's row charge is in its parent's entry. Eviction (M3) changes the resident account and leaves the logical account alone.

### 11.12 Checkpoints and relocation

**AP16. The paused cut: what it writes, the location table and the relocation closure (B2, B3, H7; v2.38, option C).**

**The location table.** The writer keeps, as its own memory (not format):
- **The record map**, a persistent ordered map `std.omap.OMap[u64, Record]` keyed by stamp, as the built §3.5 node table is (live stamps are unique, AP6, AP21). It holds one record per frame that the installed generation reaches, its two list roots' pages included. A record holds:
  - the frame's location (pack u64, offset u64), its payload checksum u64 and length u32, and its kind (PAGE or BLOB);
  - its **out-edge stamps**, a `List[u64]` with one slot per owning edge of the page, in slot order (empty for a blob);
  - its **incoming list**, one entry per live edge into it (U9). Each entry is a tagged source: `Page(stamp u64)`; `Holder(kind u8, id u64, tree u8)` for one of a branch's or a pin's three roots, where id is the branch id or the pin's serial, a u64 the writer issues once per pin and never reuses within the open; or `List(which u8)` for one of the two list roots. The tag is separate from the full-width value, so no page stamp can equal a holder (U7).
- **A record holds no share of the body (T4).** A private page, and a published page on AP10's unique path, has exactly the owners it had without records, and `with_unique` decides an in-place edit as before.
- **Only a cut creates records.** A frame that no cut has written has no record. Commits, `migrate`, index backfill, the install graft, forks and pins allocate no checkpoint state. A cut creates the records of the frames it writes, and removes those of the frames its generation drops.
- **The pack inventory** is a persistent ordered map keyed by seq. It holds one entry per pack that exists (listed, reached class-2, or retired and not yet unlinked), with its class, `frames`, `live` and `bytes`, and its leased and retiring flags.
- The records' locations are the **stamp index**: a page or blob is located exactly when it has a record. Dead copies (B3) are never records. They count only in their pack's `frames` and `bytes`.
- **Relocation changes a location, not an identity (R6).** A moved frame keeps its stamp. So the incoming lists that name it, and its parents' out-edge stamps, need no update, and a later cut's upward walk from a stationary child reaches the moved parent.

**The cut.** The writer runs every cut inline, and **it is paused**: from step 0 to step 6 it serves no other call. One writer means one cut at a time, and no commit, fork or pin runs between the moment a cut reads the roots and the moment it publishes. The steps replace §3.5's writer steps 0–4 and the checkpoint task's steps 1–6 at the cutover. **The checkpoint task, its channel, `checkpoint_task_bytes` and the `Cut`, `Checkpointed` and `Refused` messages are deleted.**

0. **Drain (X1).** Receive journal acks until `journaled` reaches the last frame sent, as built. `Failed(e)` poisons the Db (§3.8), and nothing is created.
1. **Rotate.** Allocate the rotation's buffers (the segment and END names, the HEADER, and the END payload and frame) (U4). Then create segment n+1 and its END by the barrier table, and send `Rotate`, as built §3.5 steps 1–2.
   - A refusal of these buffers refuses the cut before anything is created or sent.
   - After creation, only I/O can fail, and I/O poisons.
   - Nothing has been sent since step 0, so the roots the cut reads in step 2 are exactly the roots at the rotation. This is §3.5 step 3's ordering rule.
2. **Plan.** The generation's roots are every branch head's and pin's current roots with the triggering operation's change H applied (plan item 0). The writer builds a **candidate** from its own limit, by fallible allocation, in this order (v2.40 C-6). Each item reads only what the items before it produced:
   0. **the prospective holder delta H** (C-2, below), which the live records never see before step 5;
   1. the fresh set N and the candidate record map and inventory (the reference count, below);
   2. the cut's settle X, by AP15's walk over the generation's roots;

   Items 0–2 are the **mandatory candidate**. The writer keeps it as the **retry base**: the candidate maps are persistent, so keeping that version is one share.

   3. selection and the relocation closure (below), which are optional;
   4. **the data layout:** every class-1 and class-0 frame's pack seq and offset, from the installed `next_pack` upward (pack-contiguous emission, below), and each data pack's `frames`, `live` and `bytes`. That gives the data retirements;
   5. the list edits and the retirement list (below), from that final data layout, with the candidate records and incoming entries of the new and discarded list pages and the two list roots' `List` entries (U8);
   6. the list pages' stamps, drawn from the counter by checked u64 arithmetic (AP6, S1), followed by the capture of `next_stamp`;
   7. **the class-2 layout:** every list frame's pack seq and offset, continuing the seqs after the data packs;
   8. the buffers that the remaining steps use: one pack buffer of max(`pack_bytes`, 16,777,476) bytes, one removal-name buffer, and the MANIFEST's encoded fields, from H (step 4).

   The pack buffer holds any pack with two or more frames (at most `pack_bytes`), a singleton (at most 72 + 16 + 16,711,688 = 16,711,776 bytes for a BLOB), and the whole MANIFEST file (HEADER 72, frame head 16, payload at most `MANIFEST_LIMIT`, FENCE 20: at most 16,777,476), one at a time.
3. **Write.** Encode the frames children first in the order the two layouts fix: class 1, then class 0, then class 2. Write each pack into the pack buffer as `p-<seq>.tmp` (with `Write`, which truncates a leftover), then sync it and rename it onto `p-<seq>`.
   - Emission is children first. So when a parent is encoded, each child's checksum is already known. A page's PagePtr takes its frame payload's xxh3-64. A BlobRef takes `len`, the value's length, and the xxh3-64 of the value bytes alone, never of the BLOB payload, which begins with the 8-byte stamp (AP8, U3).
   - As each frame is encoded, its candidate record takes its location, checksum and length. This is a fallible insert into the candidate map.
4. **Publish.**
   - For an install, first check NEW's bounds against the exact account and the packs actually written (§3.5a, §11.1 T10, U4), and run AP21's steps 0–4 over the written packs and the prospective MANIFEST (C-7).
   - Then `sync_dir` the pack directory. Encode the MANIFEST into the pack buffer **from the live holder records with H applied** (C-2). That means every field AP14 names: each branch's id, name, parent, `forked_at`, version, `at`, three roots and base, and each pin's name, branch, version and roots, plus the candidate's list roots. No copy of the holders is made (U3). **Every root PagePtr, changed or not, takes its stamp's candidate location.** A holder whose logical root did not change can still have a new physical location, because relocation may have moved its root page. The counters are:
     - `first_live`: the seq of the segment that step 1 created;
     - `next_pack`: one past the last pack step 3 wrote, or the installed `next_pack` when step 3 wrote none;
     - `next_stamp`: item 6's capture;
     - `next_branch`: the live counter (below);
     - `next_table`: H's value, or the installed one when H has none;
     - `prev_first`: from the removal plan;
     - `account`: X.

     Write `MANIFEST.tmp`, sync it, rename it onto `MANIFEST`, and `sync_dir`.
   - **The rename is the commit point**, and the last point at which the cut can be refused.
5. **Install.** After the MANIFEST's `sync_dir`, and with no allocation:
   - Swap the installed record map and inventory for the candidate's versions.
   - Apply H to the live holder records: insert its record into the slot item 0 reserved, remove its removed record, and swap its replaced roots. Then record this generation as the **last published** one, with each listed branch's encoded version and its `first_live`. W2 step 2 reads these (AP1a).
   - Set A = X and `exact` = true (AP15), U = 0 (AP15: the new segment is empty, and every earlier frame is covered), and d and the carried set from the removal plan.
   - A swap allocates nothing.
6. **Remove** the removal plan's names, formatting each into the one name buffer and ignoring NotFound, as built. Any other failure poisons (V6).

**The prospective holder delta H (v2.40 C-2).** The triggering operation supplies its change as data. The writer builds it at plan item 0 and does not touch its live branch and pin records until step 5:
- **Added:** a `pin`'s pin record (name, branch, version and roots), or a cut-path fork's branch record (id, name, parent, `forked_at`, version, `at`, roots and base 0).
- **Removed:** a deleted or adopted branch's id, or an unpinned pin's name.
- **Replaced roots:** a schema commit's or `migrate`'s new main roots at version + 1, an install's graft roots and version, or an `adopt`'s replacement of main.
- **Record-byte total:** the record-byte total T (S10) after the change.
- **Counter:** a schema commit's or an install's `next_table`.

H owns its added record's storage. It reserves the one slot of growth that record needs in the writer's branch or pin list and in its name index, as AP17's preparation does, and copies no other record. Item 1's walk and item 2's settle read the live records through H, so the generation they see is the prospective one. **Live and prospective counters are kept apart:**
- **Consumed live, and never rolled back:** stamps (AP6) and a cut-path fork's id, whose `next_branch` advance happens when the id is taken (AP17).
- **Prospective, applied at step 5 only:** `next_table`, the holder records, T and the last published generation.

**A refused cut drops H with the candidate.** The live records, T, `next_table` and the last published generation are then as they were. A cut with no triggering operation (`checkpoint`, a threshold cut) has an empty H.

**A refused cut loses nothing.** A refusal in steps 1–4 before the rename refuses the cut. The refusal is an `Allocation(c)` from any allocation, `Limit(Ids)` from the stamp draw, or an install's `FullNew(cause)`. The writer drops the candidate, H and the cut's buffers, and is left as follows:
- The installed record map and inventory, A, `exact`, U, the heads, the pins, T, `next_table`, the last published generation and the MANIFEST are as they were. The installed maps are other versions, which the candidate never edited, and the live holder records never saw H (C-2).
- Packs that step 3 already wrote stay as leftovers at seq ≥ `next_pack`, which the next cut replaces and open removes (H2). They are contiguous from `next_pack`, because every cut writes its packs at consecutive seqs from the installed `next_pack`, so open's leftover sweep, which stops at the first missing seq, finds them all.
- A rotated segment stays as the current segment. The old MANIFEST plus the journal, which `prev_end` chains across the rotation, still recover every committed write. This is built §3.5 Q1's argument.
- The only lasting traces are consumed stamps (the counter never rolls back) and spare capacity left in the writer's own maps by a refused growth. The resident account reports that capacity.
- A capacity refusal never poisons. The journal keeps growing until a cut succeeds, up to the cap (AP15).

**Who sees a refusal.**
- **A cut-made operation** publishes only when its cut reaches step 5. These operations are `checkpoint`, every schema commit, `migrate`, `pin`, `unpin`, a cut-path `fork`, `adopt`, `delete_branch` and the install. If the cut is refused, the operation changes nothing and returns `Error(Full(Allocation(c)))`, with c the refused allocation's `Capacity` (requested: the bytes it asked for; available: the writer's room then). An operation refused by its own admission before the cut returns `Error(Full(Resident(c)))`. A stamp refusal returns `Limit(Ids)` instead, and an install returns `FullNew(cause)` (§11.1 T10).
- **`delete_branch`** is atomic: the branch's removal and every record and pack it alone held change at one MANIFEST rename, or not at all.

**Triggers** (§3.5's, restated):
- **The journal since the last cut taken exceeds `checkpoint_bytes`.** The cut runs inside the call whose frame crossed the threshold, after that call has published and handed over its frame, and before it returns. The call returns its own outcome, and a refused cut is not the call's failure. The count runs from the last cut taken, refused or not, so a refused threshold cut is tried again after another `checkpoint_bytes`.
- **`checkpoint`.**
- **Any cut-made operation.**

A write refused at the journal cap triggers no cut (AP15).

**The pause.** A cut's pause is

  `T = t_drain + t_plan + t_write + t_sync`

- **t_drain** is the journal's queued prefix (v2.40 C-12). F ≤ `FRAME_LIMIT` bounds its bytes, not its groups: a prefix of small frames can span several groups, and step 0 waits through each group's sync. R4 and R6 count the group syncs they observe.
- **t_plan** is O(|N| + |M ∪ C| + |L| + their edges + D + K + P + V), counted in candidate-map updates and walk steps, each an O(log) OMap step:
  - N is the fresh frames;
  - M ∪ C is the moved frames and their closure;
  - L is the list pages;
  - D is the settle's divergence (AP15's n-way walk visits only pages that some root does not share);
  - K is the records the cascade drops;
  - P is the pack inventory, which selection scans;
  - V is the records scanned to find M when a data pack is selected.
- **t_write** = B / w, where B is the bytes written (the bound below) and w is the device's sequential write rate.
- **t_sync** is (p + 3 + s_r) syncs:
  - one per pack written (p);
  - the pack directory's `sync_dir`;
  - the MANIFEST's sync and its `sync_dir`;
  - the s_r syncs of the barrier table's segment creation, including the predecessor's END.
- **t_remove**, after the commit point: one unlink per name of the covered interval and of the retired packs (step 6). A long covered interval, for example after a held export lease, makes it long. R4 and R6 count the removals.

**Worked example** (GUESS, under SHAPE-1644's assumptions):
- The store is 10 GB with 1% of its pages changed since the last cut, dirty ancestors included. So B ≈ 100 MB, about 25,000 frames of 4 KiB, in p = 2 packs at the 64 MiB default.
- At w = 0.5–1 GB/s, **t_write = 0.1–0.2 s**.
- t_plan is about 25,000 record inserts and their edges.
- t_sync is about 2 + 3 + s_r flushes at 1–5 ms each on a flash device (GUESS).

So **T ≈ 0.1–0.25 s**. R4 and R6 measure it (M1-DESIGN §4.2).

**Two shapes make the pause longer than B suggests:**
- Deleting the sole holder of a large branch makes K every record that only that branch reached. That cut's pause is proportional to the branch's size even when B is small.
- An unchanged cut over many divergent retained roots has N empty and still pays D.

**The pause's upper bound** in bytes is the write bound below. From L5, k one-key commits since the last published cut write at most k × L5's page count, plus the list pages and the closure.

**The fresh set and the reference count (N13).** The candidate starts as the installed record map and inventory, shared. Each update path-copies, so the installed versions never change before step 5.
1. **Fresh set.** Walk down from the generation's roots, stopping at every page or blob that has a record. The frames reached are N. Each is reached once, however many roots share it, and each gets a candidate record whose location comes from the layout.
   - The walk keeps one stack per tree it is in, each at most 65 levels. A map leaf's collection root starts a new stack, so the map path and the collection path are counted separately (U9).
2. **Add incoming entries.** Add each edge of a frame of N to its target's candidate incoming list. Add a holder entry to the root record of each holder whose root is new or differs from its installed one.
3. **Then remove.** For each holder whose root changed, or that the generation drops, remove its entry from the old root's candidate record.
   - A located record whose incoming list becomes empty is dead. It leaves the candidate map, and the entries named by its out-edge stamps are removed in turn (the cascade of §3.5's count, by stamp).
   - Step 2 ran first, so a located child shared by a dropped parent P and a fresh parent Q keeps Q's entry and stays live.
4. Each pack's candidate `live` is its installed `live` minus the payload bytes of its dead records. Selection reads this figure.

So the closure walk below sees exactly the generation's live edges. A located child whose only live parent is fresh is reached through Q's entry, and the upward walk stops at a fresh page, which is written anyway.

**What a cut writes**, in children-before-parents order (a blob before the leaf that names it, a child before its parent, a collection's root before its map leaf):
- every page reachable from a live root that has no record;
- every blob reachable from a live root that has no record;
- the moved frames and the closure, below;
- into class-2 packs (choice 6, S2, T3):
  - the pack-list pages the cut builds or edits (a row added, removed or with new `frames`, `live` or `bytes`, and their ancestors);
  - the surviving pack-list pages that class-2 relocation moves, with their old stamps;
  - the whole rebuilt retirement list.

It fills each written page's PagePtrs and BlobRefs from the layout and from step 3's checksums, and writes canonical images (AP3). Pages go to class-0 packs, blobs to class-1 packs and list pages to class-2 packs. Pack size follows H1 for all three classes, with PAGE and BLOB in place of NODE.

**Pack-contiguous emission (T6).** Emission is in three runs, one per class:
1. every blob of the cut (class 1);
2. every data page, children first (class 0);
3. every list page, children first (class 2).

A blob has no edges, a data page's edges go only to blobs and data pages, and a list page's edges go only to list pages. So every edge's target is emitted before its source.

Within each run the frames fill packs in emission order. A frame that would take the open pack past `pack_bytes` closes it first, unless the pack holds no frame yet (H1). Seqs are taken from `next_pack` in that order. So each pack is complete before the next begins, and the one pack buffer holds the open pack.

**Relocation (copy-forward).**
- **Selection.** Packs are selected as §3.5 selects them, per class, by `live / bytes` (AP14), from the candidate inventory.
  - A class-2 pack is selected by the same rule. Its `live` comes from its records, and its `bytes` is the PAGE payload bytes of its file, which the writer knows from writing it and open from streaming the file once (AP21 step 0, S7). Class-2 selection is part of the list edits below.
  - The moved set M is every live frame in a selected data pack, found by scanning the candidate records for its seq.
- **The closure.** C is the set of live pages from which some live edge path reaches a frame in M. It is the union, over every frame in M, of its live ancestors under every live root: branch heads, pins, the two list roots, and, after a later merge, base roots. It includes a leaf whose blob moves and a map leaf whose collection root moves.
- **Finding it.** Walk upward from every frame in M through the candidate incoming lists, marking each located record reached in a worklist. Stop at records already marked, at fresh records (already in N) and at holders.
  - The walk visits exactly C ∪ M, their incoming entries and the boundary of N. No reachability scan is needed.
  - A selected pack holding a frame with no live parent is dead, not moved.
- **Relocation is optional, and never takes the mandatory part's memory (U13).** Its allocations come after the mandatory candidate (plan items 0–2) has succeeded: the worklist, M ∪ C's candidate updates and their share of the new packs.
  - If one of them is refused, the writer drops relocation's part of the candidate (found by the worklist) and continues with M = C = ∅ for data.
  - If a later plan item (4–8), or a record insert or pack write of step 3, is refused while relocation is in the candidate, the writer **restores the retry base** (v2.40 C-6). That is the mandatory candidate of items 0–2. Restoring it discards everything built on it: the dead counts, changed pointers, list edits, emitted flags, layouts and partial candidate records. The writer then redoes items 4–8 and step 3 once, with M = C = ∅ for data. The redo emits its final layout from the installed `next_pack` again. A pack the first try already renamed is either rewritten at its seq (`Write` truncates the `.tmp`, and the rename replaces `p-<seq>`) or, when the redo writes fewer packs, left as a contiguous leftover above the redo's last seq (H2). Consumed stamps stay consumed: the redo draws its list stamps above the first try's, and captures `next_stamp` after them. Only a refusal without relocation refuses the cut.
  - A cut without data relocation still covers the journal, writes every fresh frame and retires every dead pack, and the next cut selects again. This is the only way a cut does less than §3.5 selects.
- **Emitting.** The cut writes N ∪ M ∪ C ∪ L once. The emission walk goes down from the generation's roots and from the planned list roots. It enters a child only when the child is in N ∪ M ∪ C ∪ L (fresh, marked, or a planned list page) and not yet emitted, and it appends each frame after its children. The plan visits the frames run by run: blobs, then data pages, then list pages.
- Each frame in M is written into the cut's new packs, keeping its stamp. Each page in C is written once, after its children, keeping its stamp, with its edges' new locations. Every live root then reaches only new locations for M ∪ C.
- **Invariant of the new generation (S6):** every data edge reachable from its roots resolves to a frame in a pack it lists with the edge's class (0 for a page, 1 for a blob) and does not retire. Every list edge resolves to a page in a class-2 pack it reaches and does not retire. This is a property of the closure, checked by L41, not a runtime guard.
- **The old copies.** The old frames of M ∪ C become dead in their packs. The old copy of a page in C may stay listed in a pack that is not selected. It repeats a live stamp and may point into a retired pack, which is legal (AP6, AP21). These frames count toward the next cut's selection, and a cut never compacts recursively.
- Stamps are kept, so `changes` reports nothing (L41).

**List edits and retirement, in one pass over the final layout (S2, T3).** After the fresh set, selection and the closure, the plan does the following:
1. **Reads the final data layout and inventory (plan item 4, C-6).** Every frame of N ∪ M ∪ C has its new pack and offset, and every data pack its new `frames`, `live` and `bytes`. The data packs the cut stops listing (every frame dead, or selected and moved) are its data retirements.
2. **Edits the pack list** by AP3: rows added for the cut's new data packs, removed for its data retirements, rewritten for changed counts. Each edited page and its ancestors are copied with fresh stamps. Untouched pages keep their old locations and stamps.
3. **Discards the old retirement list.** None of its pages survives the cut.
4. **Relocates surviving list pages.** It computes the old class-2 packs that the edited pack list still reaches, each with its `live` from the surviving pages in it, and selects among them by the rule above. Every surviving page in a selected class-2 pack is moved, keeping its stamp. So is every surviving list ancestor of a moved page (its closure within the list tree), whether or not that ancestor was selected. Edited pages are already fresh.
5. **Classifies the old class-2 packs from the final locations.** An old class-2 pack that holds no page of the final pack list is a class-2 retirement. Every pack of the old retirement list is one, unless a page of the final pack list is in it.
6. **Builds the retirement list whole** by AP13, with fresh stamps, into the cut's new class-2 packs. Its rows are:
   - this cut's data retirements;
   - its class-2 retirements;
   - the carried set: every seq that an earlier cut retired and did not unlink because a lease held it (U6).

   A carried seq stays in the set until a removal plan unlinks it, whether or not the lease that first held it still exists. So a pack released by its lease is still a row, and the next cut unlinks it. The pages this step builds are in new packs, which no row names, so adding a row empties no further pack. Steps 2–6 reach their fixed point in one pass, and each old pack is classified once.
7. **Emits only reachable list pages:** the fresh and moved pages of the final pack list and the pages of the new retirement list. A page marked by an initial selection but discarded in step 3 is not written. So no unlisted, unretired class-2 file is created below the new `next_pack`.
8. **Eligibility is not unlinking.** A row makes its pack eligible. The removal plan unlinks it only when no lease holds a generation that lists or reaches it. An export of generation G captures the data packs G lists and the class-2 packs G reaches, and they stay until the first cut after its lease ends.

**The removal plan (T2, U6, U12).**
- **The deferred segment d.** The writer keeps d, the lowest journal segment that an earlier cut left on disk because a lease held it (none when no removal was deferred).
- **The lease.** At step 2 the writer takes the set of leased packs and, when an export lease is live (§3.5a; the built `segment_leased` tests `first ≤ seq ≤ terminal`), the lease's inclusive segment interval [a, b].
- **Segments removed.** The covered interval is [min(d, old `first_live`), new `first_live`). The removal plan takes that interval minus [a, b], as at most two ranges, each a pair of u64s, and removes both `j-<seq>` and `j-<seq>.end` for every seq in a range. A seq in [a, b] keeps both.
- **Packs removed.** The plan also removes the retired seqs that no lease holds. These are rows of the new retirement list, the carried set included.
- **`prev_first`.** The MANIFEST's `prev_first` is min(d, old `first_live`). So open, which holds no lease, removes the deferred segments after a crash.
- **After step 6:**
  - d becomes the lowest kept seq of the covered interval, or none when no seq was kept;
  - the carried set becomes the retired seqs the plan skipped.
- **Attempted is not removed.** The plan attempts every name in its ranges. A name an earlier cut already removed is NotFound, which is ignored, as built.
- **Barriers.** The pack-directory `sync_dir` before the MANIFEST is written stays, and O1 still makes the selected MANIFEST durable before open's cleanup.

**Bound on what a compacting cut writes.** The frames written are N ∪ M ∪ C ∪ L, where N is the pages and blobs that had no record and L is every list page the cut builds, edits or moves (T3). Their bytes are

  `Σ over N ∪ M ∪ C ∪ L of (16 + payload) + 72 × (the packs written)`,

where a pack written by H1's singleton rule holds one frame and still counts its 72-byte HEADER.
- |C| is at most the number of distinct live pages with a path to M, over all roots. That is not bounded by |M| × depth: one shared leaf reached through k different parents, from k divergent heads, has k distinct parents to rewrite at one depth (L41 exercises k above the tree's height).
- A cut that moves only blobs writes no selected PAGE frame and still rewrites their leaves and ancestors.
- A cut with M empty writes N and L alone.

**Reference counts and GC.**
- Incoming entries are added by the reference count's step 2 and removed by its step 3, additions first. A target with no entries left dies in turn.
- Retirement is planned as above (S2). The built cleanup lists no directory, so a class-2 pack that no generation reaches below `next_pack` is removed only because it is a `retired` row. A selected pack is retired by the cut that moves its live frames.
- Locations become installed only at step 5, after the MANIFEST's `sync_dir`. A cut whose I/O fails poisons the Db (§3.8).

**At open (AP21) the opening task, which becomes the writer, rebuilds the record map and inventory in its own limit from the resolved graph.**
- It makes one record per reached stamp, with its location, checksum, out-edge stamps and incoming list, the holders' and list roots' entries, and the pack inventory from the lists and the scans.
- Frames that replay creates get no record until the first cut.
- An open that cannot allocate this state is `Full(Allocation(c))`, as for any other open allocation. That is an allocator refusal, not an admission one (AP15).

**M3 consequence.** Incoming lists are rebuilt at open from a full decode. M3's lazy open cannot rebuild them without reading every pack, so M3 must either persist the reverse edges or find a closure by walking the roots it has loaded. M1's format forecloses neither.

**AP21. Open: validate the format-3 graph, then resolve live edges by location (B3, M3, S6, S7, T12; restored in v2.39).** A parsed MANIFEST and the pages decoded from its files are a **validated inventory and live graph** only after steps 0–4. Every consumer takes that validated value or a projection of it, never the raw MANIFEST fields:
- open, which builds the trees and the location table from it (step 5);
- cleanup's retirement, which removes a pack only if it is a seq of the validated R that no lease holds, after O1 has selected the MANIFEST;
- an install's pre-activation check (§11.1 T10), which runs steps 0–4 over the private packs it wrote and the prospective MANIFEST, before the rename and before activation, with NEW's limits;
- the prepare opener (§3.5a), which runs steps 0–4 over the EXPORT's captured MANIFEST and packs before it replays the export prefix, and removes nothing (v2.40 C-7).

`std.page.decode` checks one payload. Location, frame identity and class belong to this procedure. The opening task runs it before the writer serves any call, so the paused writer starts from the validated value and keeps it current only through its own cuts (AP16). No reserve, plan or hand-off state is built.
0. **Resolve the two lists and validate their files first (N8, S6, S7).**
   - **Bound each read before making it (v2.40 C-7).** For each PagePtr a list read follows, first require `len` ≤ 8,384, and require `offset` + 16 + `len` ≤ the file's length by checked u64 arithmetic (`Corrupt(Malformed)` otherwise). Only after the read do AP20's five checks confirm the target, and the streamed PAGE-head confirmation (T12, below) still follows.
   - Resolve the `packs` and `retired` roots by exact location with AP20's five checks, reading only that frame (16 + `len` bytes at `offset`), and require its stamp below `next_stamp`. Decode the page (AP3), apply the order rule, and follow its branch edges the same way. Each read is one PAGE frame, at most 16 + 8,384 = 8,400 bytes (8,384 is the payload limit).
   - Check every row's shape (AP14) and strict seq order within each list, across leaves as within one.
   - Let L be the listed seqs, R the retired seqs and C the distinct class-2 seqs the two lists reach. Require L, R and C pairwise disjoint, and every seq of L ∪ R ∪ C below `next_pack` (`Corrupt(Malformed)` otherwise). This happens before any removal and after O1 has selected this MANIFEST.
   - **The file then confirms the pointer reads (T12).** Each reached list page enters, by stamp, the one reached-stamp table that step 3 also uses for data pages. A stamp reached at two locations, in either graph or across them, is `Corrupt(Malformed)`, and one location reached twice is marked once (each list keeps its own strict row-order traversal). The table's entries are grouped by file and sorted by offset.
   - Then stream each pack of C once, front to back, through a fixed 65,536-byte chunk buffer, with the checksum state carried across chunks. Check its HEADER and every frame as step 2 checks a listed pack's: retired kinds, wrong-class kinds, unknown MUST, stamps below `next_stamp`, and §2.7's extension limits, `UNKNOWN_LIMIT` per frame and `UNKNOWN_ROOM` per file, so a skippable extension larger than one PAGE read is still checked in chunks. Record each PAGE head's offset, and merge those offsets with the file's reached offsets: every reached offset must be a PAGE head that the stream visited (`Corrupt(Malformed)` otherwise).
   - Sum the file's PAGE payload bytes, which is its `bytes` for selection (AP16). Only reached pages are decoded.
1. Read each listed pack whole, bounded at 72 + `frames` × (16 + the class's payload limit: 8,384 for class 0, 16,711,688 for class 1) + `UNKNOWN_ROOM`, by checked arithmetic (V2). Check its HEADER.
2. Frame-check every frame. Refuse a retired kind (`Unsupported`) and a wrong-class kind (`Corrupt(Kind)`). For every PAGE and BLOB frame, read its stamp and require it below `next_stamp` (`Corrupt(Malformed)`), whether or not the frame is live. Index the frames by offset. Check `frames` and `bytes` (AP14).
3. Resolve each MANIFEST root by its PagePtr, by exact location, with AP20's five checks. Decode that page (AP3) and resolve its owning edges the same way: branch children, map-leaf collection roots, and BlobRefs (AP8). Apply the order rule to each page edge (AP20). A page reached twice by one location is one page, shared. A stamp reached at two locations, in the data graph or across it and the list graph (step 0's table), is `Corrupt(Malformed)`.
4. Frames not reached are dead. They are never decoded beyond step 2, and their edges are never followed, so a dead frame may repeat a live stamp or name a retired pack.
5. Build the `Arc[Page]` values children first, then attach every branch and pin root. Then rebuild the location table and the pack inventory from the validated graph (AP16): one record per reached stamp, the `live` and `bytes` of each pack of L and C, and R as the retired set, less the members that open's cleanup unlinked. Then replay the journal (§3.4, in the order below).
**Open under format 3: validation before every cleanup (restates §3.4; v2.40 C-7).** The steps run in this order. A failure ends the open, and nothing that comes after the failing step runs:
0. **Options.** The built checks, then AP15's options step (the journal floor). This runs before LOCK, and nothing is opened, created or read.
1. **Lock**, as built.
2. **MANIFEST.** Genesis is as built, with AP14's fields. Otherwise read `MANIFEST`. A payload over `MANIFEST_LIMIT` is refused before it is decoded. Decode the HEADER, the MANIFEST and any FENCE. A requirement the opener does not support ends the open here, before O1 and before any removal.
3. **O1**, as built (§3.4 step 2b).
4. **Validate.** AP21 steps 0–4 over the selected MANIFEST and its files. A failure is `Corrupt`, `Dangling` or `Unsupported`, and nothing has been removed.
5. **Cleanup, from the validated result only** (best effort, H2). It removes:
   - `MANIFEST.tmp`;
   - the leftovers `p-x` and `p-x.tmp` for x from the validated `next_pack` upward, stopping at the first x where both are NotFound;
   - every seq of the validated R (a normal open holds no lease);
   - the segments [`prev_first`, `first_live`) with their `.end` files.

   NotFound is ignored, and any other failure is `Io(e)`, as built. The in-memory pack inventory leaves out the members of R this step unlinked, although the MANIFEST still lists their rows until the next cut rebuilds the retirement list. So the open starts with an empty carried set and with no deferred segment d (AP16).
6. **Rebuild.** AP21 step 5: the pages, the location table and the pack inventory, from the validated graph.
7. **Journal.** Read the segments from `first_live` with the bounded segment reader (AP15), keeping §3.4 step 5's END, `prev_end` and torn-tail rules. Apply the COMMIT and FORK events in order (AP19). U is the saturating sum of their lengths.
8. **Account.** The exact logical account of the recovered roots (AP15 **Open**). Past `resident_limit` it is `Full(Resident(c))`, and an allocation refusal anywhere in the open is `Full(Allocation(c))`.
9. **Rotate**, as built (§3.4 step 8). Set `since` = 0 (AP15).
10. **Streams**, as built.
11. **Tasks.** Spawn the journal task alone. There is no checkpoint task (AP16).

Three failures stay distinct: a validation error (`Corrupt`, `Dangling` or `Unsupported`), a capacity refusal (`Full(cause)`), and an I/O failure (`Io(e)`).
- **The prepare opener (§3.5a)** runs steps 0–4 of AP21 over the EXPORT's captured MANIFEST and packs, with the opener's own limits, before it replays the export prefix. It removes nothing from any directory. A validation failure there is the prepare's `Store(e)`, with e AP21's error.
- **NEW's private output** is validated by T10's step 2 (§11.1) and AP16 step 4: AP21 steps 0–4 run over the packs the install wrote and over the prospective MANIFEST, before the rename.

### 11.13 Instant journaled fork

**AP17. Which forks are journaled, and in what order `fork` decides (B1, H2, H10, H14).** `fork(mut db, name, from)` checks, in this order, and the first failure is its answer, with nothing allocated, sent or installed:
1. `from` is a Snapshot of another store, or of another open of this store: `Foreign`.
2. The Db is poisoned: `Poisoned(e)`.
3. `name` is over 255 UTF-8 bytes: `Limit(Name)`. A live branch already has `name`: `Exists(name)`. The empty name is allowed, on both paths, as built (`checked_name`), and the FORK and MANIFEST grammars carry 0–255 bytes.
4. `next_branch` is 2³² − 1, so `id + 1` cannot be represented: `Limit(Ids)`.
5. The MANIFEST size expression (AP4) over the live branches and pins plus this branch's record would pass `MANIFEST_LIMIT`: `Limit(Manifest)`.
6. **Live-head test.** `from`'s branch is live, `from`'s version equals that branch head's version, and `from`'s three root stamps equal the head's three root stamps at the call. Then the fork is a **live-head fork** (AP18). Otherwise, including a Snapshot of a deleted branch, an older Snapshot, or one whose roots differ, it takes §1.9's cut path, because journal replay rebuilds only head versions.

**The journal cap comes first (AP15).** On a disk store's live-head path, right after step 6: if the FORK frame's f = 16 + 18 + n bytes fails AP15's comparison against U and `journal_limit`, `fork` returns `Full(Journal(Capacity{requested: r, available: journal_limit}))` by AP15's comparison (r saturates at u64::MAX) before the preparation and before the id is taken (C-10), so no allocator refusal can mask it, and nothing is allocated, sent or installed.

**Preparation of a live-head fork, before anything is durable (N5, R4).** On the live-head path only, after step 6 and the cap, and before any frame, on both disk and memory stores, the fork prepares everything its installation needs, and any failure here (an allocation's `Capacity`) returns that error with nothing allocated, sent or installed:
- the owned copy of `name`;
- the three root shares of the parent head;
- one more slot of capacity in the writer's branch list and its name index, and the branch record;
- on disk, the encoded FORK frame.
So installation after the frame is durable allocates nothing and cannot fail.

**The live-head path on a disk store:**
7. The journal cap was checked before the preparation (above), and so before the id is taken. Drain the journal acks (Y1). **No admission check:** the fork's logical charge is 0 (AP18), and its frame waits for `FRAME_LIMIT` room and is never refused for it (AP15, choice 5). So a live-head fork is never refused by admission (`resident_limit` or frame room), whatever conservative charges A holds, and the same roots give the same answer after any history (N3).
8. Take `id = next_branch` and set `next_branch = id + 1`.
9. `send` the prepared FORK frame (AP19) on the journal channel as an ordinary frame: it waits for `FRAME_LIMIT` room like any frame, takes the next frame sequence s, goes through the one FIFO queue after every frame sent before it, adds its length to the writer's segment end (so a later rotation's `prev_end` includes it), adds its length to the journal bytes counted for the `checkpoint_bytes` trigger, and adds it to U (AP15). A failed send poisons the Db and returns `Poisoned(e)`, with no branch installed.
10. **Wait for its ack**: receive acks as `durable(Journal(s))` does, until the acked sequence reaches s. On `Failed(e)` the Db is poisoned and `fork` returns `Poisoned(e)`, with no branch installed. A cancellation here is a crash cut at whatever point the worker reached (§3.7): the Db dies with the task and no handle was returned. If the segment sync had already succeeded and only the ack's delivery was cut, `beta` survives recovery with its id; that is legal, because no caller holds an id that recovery gives to another branch.
11. Install the branch from the prepared storage, with no allocation: its three roots are the prepared shares, its version and `forked_at` are `from`'s version, its `at` is the parent head's `at`, its parent is `from`'s branch, and its base tag is 0. A and its exact flag are unchanged except that the store is now in conservative mode (AP15).
12. If the FORK frame took the journal past `checkpoint_bytes`, run the threshold cut (AP16, below); a refused cut is not the fork's failure. Return `(Branch{store, id}, Ticket{store, open, Journal(s)})`. `durable` of that ticket returns at once.

So no `Branch` value, `branch(db, name)` answer or CallId ever names an id whose FORK is not durable, and a crash can reissue only ids that no caller saw.

**The live-head path on a `memory` store:** steps 1–6, the preparation, steps 8, 11 and 12, with no frame and no I/O. The ticket is `Journal(c)`, where c is the memory store's commit counter after advancing it by one, as a commit advances it, and `durable` is `Ok` at once. The returned handle names the installed branch, and `branch(db, name)` finds it.

**A cut path fork** is §1.9's and uses none of the live-head preparation (R4): its roots are the supplied Snapshot's own three roots, never the parent head's (which may have advanced past the Snapshot, or been deleted); its parent and `forked_at` are the Snapshot's branch and version as §1.9 records them; and it allocates its id, settles and admits the exact figure (AP15), and makes a cut. Its branch is installed and returned only when its cut publishes, which makes the MANIFEST holding the id durable. A cut-path fork refused by admission (`Full(Resident(c))`), or whose cut is refused (`Full(Allocation(c))`, AP16), installs nothing; its id is consumed and never issued again in this open. It writes no journal frame, so the journal cap does not apply to it.

**Threshold cuts.** A live-head fork never requires a cut. If its frame's bytes take the journal past `checkpoint_bytes`, the ordinary threshold cut runs after step 11, exactly as after a commit (AP16), and the fork's answer does not depend on it (v2.40 C-12). The fork returns only after that cut ends. A refused cut (`Allocation`, `Limit(Ids)`) is not the fork's failure: the fork returns its branch, the FORK is durable and the branch is installed. An I/O failure in that cut poisons the Db after the FORK is durable, so the call returns `Poisoned(e)` and the branch is recovered at the next open from its FORK frame. A cut reads its roots on the paused writer after the installation of every fork whose frame it covers, so a cut covering s lists the branch.

**AP18. Cost of a live-head fork.**
- The new branch's three roots are shares of an existing head's, so the set of row versions held by the participating roots is unchanged, and its account charge is 0 (AP15). It does no walk, no settle and no cut.
- **Direct cost (v2.40 C-12).** A live-head fork costs one FORK frame (16 + 18 + n bytes), the drain of the queued prefix (step 7: every group already queued, each with its sync), one group sync for its own frame, one branch record and three Arc shares, independent of the tree's size. With the journal drained and below `checkpoint_bytes`, which is L44's fixture, that is **one group sync**, independent of the tree.
- **End-to-end cost when it crosses the threshold.** The call also includes the threshold cut's whole pause T (AP16, with t_remove), and the cut's outcome is as **Threshold cuts** above says. The two costs are stated separately, and L44 measures only the direct one.

**AP19. FORK payload (kind 0x0013, MUST), in a journal segment:**

| Field | Encoding | Replay rule (each failure is `Corrupt(Malformed)`) |
|---|---|---|
| id | u32 | `next_branch` ≤ id < 2³² − 1, checked before any addition (a cut-path fork refused before its cut may have consumed ids below it; an exhausted watermark of 2³² − 1 accepts no FORK); then `next_branch` = id + 1 |
| parent | u32 | names a live branch |
| forked_at | u64 | equals the parent's head version at this point of replay |
| name | str, 0–255 bytes | no live branch has this name |

**Replay keeps the record-byte total (S10).** Recovery and `replay_export` start a running total T = Σ (143 + |name|) over the validated MANIFEST's branches + Σ (122 + |name|) over its pins, and at each FORK add 143 + |name| by checked arithmetic before installing the branch. T past `META_LIMIT` is `Corrupt(Malformed)`, so a recovered branch set always satisfies AP4. The live writer keeps the same T: `fork` and `pin` check T + their record ≤ `META_LIMIT` (AP4's expression, without summing the branches), and `adopt`, `unpin` and `delete_branch` subtract the record they remove. **T changes where its records do (S10):** a record counts from its admission (AP4), a live-head fork's from step 8, and leaves T when the cut that removes it publishes. An operation refused before it publishes, a fork or a pin refused at admission, at the journal cap or by a refused cut included, leaves T exactly as it was.

**Every journal reader parses one ordered event list (H9).**
- `journal_segment` parses a segment into one ordered list of events, `Commit(frame)` or `Fork(frame)`, in file order. Recovery, `journal_apply`, `journal_account`, `recorded` and `replay_export` all iterate that one list. No reader keeps a separate list of commits.
- **`journal_recover` / `journal_apply`** apply events in order. A `Fork` makes branch `id` with the parent's head roots at that point, version = `forked_at`, `at` = the parent head's `at`, parent and `forked_at` as given, and base tag 0.
- **`journal_decode`** decodes the four fields exactly; bytes left over are `Malformed`.
- **`journal_account`** adds nothing for a `Fork` (AP18); the open account is computed from the recovered roots (AP15).
- **`recorded`** skips a `Fork` event, advancing by its full frame length: it holds no CALL section.
- **`replay_export`** (§3.5a) replays the captured prefix's event list through `journal_apply` as recovery does, so it applies FORK by the same rule. An export that ends exactly at a fork, with no later frame on the new branch, keeps the branch.
- **The next cut** writes the branch into the MANIFEST, and from then on its frames are covered by the cut like any commit's.

### 11.14 Vectors (L37)

These were derived from this section's text by independent scripts, not by the store. Checksums are xxh3-64: the 59-, 144- and 22-byte cases from a Python implementation of the 17–128-byte path that reproduces the nine published twox-hash 2.1.5 vectors for 17, 18, 19, 31, 32, 33, 126, 127 and 128 bytes, and the BLOB case from libxxhash 0.8.2 through ctypes (`m1b2/store-m1-spec-fix-1629/blob_vector.py`, which first reproduces Leaf A's checksum). The v2.35 MANIFEST and pack-list leaf are from `list_vector.py` in the same directory, which rebuilds the v2.34 MANIFEST field by field before changing it.
- **Leaf A**, stamp 5, collection 1. Two rows, keys u64 1 and 2 (`Key[u64]` is 8 bytes BE), values schema 0 ‖ u32 7 and schema 0 ‖ u32 9. Prefix 7 zero bytes; suffixes `01` and `02`. 59 bytes, xxh3-64 `0x7ea83212bab36260`: `000002000700000005000000000000003700000000000000000000000000000100010006000000070000000100020006000000090000001f002b00`.
- **Outer PAGE frame**, Leaf A at offset 72 of pack 3 (role 3), with StoreId hi 1 and lo 2 as in §2.6. The §2.1 binding gives frame check `0x221544aaf991f841` (libxxhash 0.8.2). The 75-byte frame is `210001003b00000041f891f9aa441522000002000700000005000000000000003700000000000000000000000000000100010006000000070000000100020006000000090000001f002b00`.
- **Leaf B**, stamp 7, keys 3 and 4, values 11 and 13. 59 bytes, xxh3-64 `0x5b7778306daed154`: `0000020007000000070000000000000037000000000000000000000000000001000300060000000b00000001000400060000000d0000001f002b00`.
- **Branch**, level 1, stamp 9, on disk. Child 0 is leaf A, in pack 3 at offset 72 (right after the HEADER). Child 1 is leaf B, in pack 3 at offset 147 (72 + 16 + 59), with separator u64 3, the shortest separator between keys 2 and 3. Each child has rows 2 and bytes 2 × entry(12, 6) = 2 × 86 = 172. The branch prefix is the one separator (8 bytes), so its suffix is empty. 144 bytes: `010002000800000009000000000000008c0000000000000000000000000000030000030000000000000048000000000000003b0000006062b3ba1232a87e05000000000000000200000000000000ac000000000000000000030000000000000093000000000000003b00000054d1ae6d3078775b07000000000000000200000000000000ac0000000000000020005600`.
- **FORK** of branch 1 "beta" from main at version 64. 22 bytes: `01000000000000004000000000000000040062657461`.
- **MANIFEST** of an empty store: generation 1; limits 4096, 4096, 1024 and 16,777,280; next_stamp, next_branch, next_table, next_pack, first_live and prev_first all 1; account 0; empty `packs` and `retired` roots (all zero); one branch, main (id 0, parent 0, forked_at, version and at 0, three empty roots, base tag 0); no pins. 299 bytes (v2.35: the two lists became two 36-byte roots in place of two u32 counts): `010000000000000000100000001000000004000040000001010000000000000001000000010000000100000000000000010000000000000001000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000010000000000000004006d61696e000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000`.
- **Pack-list leaf** (v2.35, choice 6), stamp 12, in a class-2 pack. Two rows: pack 3 (class 0, 3 frames, live 262, bytes 262) and pack 4 (class 1, 1 frame, live 1,033, bytes 1,033). Keys u64 BE 3 and 4, prefix 7 zero bytes, suffixes `03` and `04`, values inline, 21 bytes each. 89 bytes, xxh3-64 `0x94793e19991ff346`: `00000200070000000c000000000000005500000000000000000000000000000100030015000003000000060100000000000006010000000000000100040015000101000000090400000000000009040000000000001f003a00`.
- **BLOB** (M5), stamp 11, value u16 0 ‖ bytes (i mod 251) for i in 0 … 1022, so v = 1025 and the payload is 8 + 1025 = 1,033 bytes, beginning `0b00000000000000` `0000` `000102…`. xxh3-64 of the 1,025 value bytes is **`0x15f1400e215dacb1`**. Its BlobRef, for the frame at offset 72 of class-1 pack 4 and body 0, is 42 bytes: `0b000000000000000104000000000000b1ac5d210e40f115040000000000000048000000000000000000`.

### 11.15 Laws M1 adds and restates

The harnesses are §5's. **S1** is the store's law functions (`law_*`, `disk_law_*`). **S2** is `disk_crash_main` (`core/tools/store_crash_tests.py` SCENARIOS). **S3** is `journal_crash_main` (`core/tools/store_journal_tests.py`). **C** is the checker-refusal programs beside `look_put_refused.hirsel`. Each case goes into the named existing law or program set, not into a new suite. Every expected value is hand-derived here, from M, or from a published vector, never from the store under test.

**Hand-derived constants used below.** A u64 row of collection 1 has a 12-byte logical key, and a value schema 0 ‖ u32 is 6 bytes, so entry(12, 6) = 62 + 16 + 8 = **86**. A 1,000-byte stored value gives entry(12, 1000) = 62 + 16 + 1000 = **1,078**. C0 is the hand sum of a case's catalog rows' entries plus 126 per collection.

| Law | Statement | Harness | The wrong implementation it catches |
|---|---|---|---|
| **L37 page vectors** | the §11.14 vectors decode to their stated fields, and encoding those fields gives those bytes: PAGE leaf A, leaf B, branch, pack-list leaf, BLOB payload and BlobRef bytes, with value checksum `0x15f1400e215dacb1`, in the page-codec slice; MANIFEST 0x0031 and the complete outer PAGE frame (Leaf A at offset 72 of pack 3, frame head included) in the cutover slice; FORK in the fork slice | F (std page payload cases, then `disk_law_frames`) | a slot array in key order reversed; a prefix that counts the collection id; a checksum over the frame head; a BlobRef with its fields reordered or the checksum over the BLOB payload's stamp |
| **L38 owned and borrowed reads** | for random tables (inline values, values on both sides of the threshold, old-schema rows, one-entry oversize leaves), `fetch` equals decoding `fetch_bytes` on a Tx, `get` equals decoding `get_bytes` on a Snapshot and on a reader task, and `scan` equals decoding `scan_bytes`/`next_view`/`value_view`, and all equal M. **C** cases: a `put` through a Tx while a `fetch_bytes` view lives; `next_view` while a `value_view` lives; a view returned out of its anchor's extent; a `ViewRows` sent to another task; each is refused by the checker | S1 `law_typed_model`; C | a view sliced from the wrong offset once a page has a prefix; a BlobRef view of the descriptor instead of the body; a scoped cursor that is `send` |
| **L39 refusal leaves the head** | (a) a body that writes k rows into one leaf (so later writes edit its private copy in place), then fails, aborts, runs out of steps or is refused: every root's stamp, and a by-value copy of every reachable page image taken before the call, equal the state after, and every read equals M before. (b) F2: a body that replies and then returns `Error`: the `collections` and `catalog` root stamps and images equal those before, and the `streams` tree equals M with the reply. (c) the conditional commit-time apply: with a two-op commit whose second op's split allocation is made to fail, the commit is `Refused`, and (a)'s comparison holds; with a reader Snapshot and a follower holding the head, a successful apply leaves both reading M at the old version. (d) AP10's re-stamp: after a replay that edits a page whose path was located by an earlier cut, a cut, removal of the covered segments and a second reopen give M (the re-stamped path was written) (e) **records keep privacy (T4):** a body that writes k rows into one fresh leaf of a Tx edits it in place (by a counted allocator, one leaf allocation for the k writes, as without records); a replay that edits a page on AP10's unique path edits it in place; no commit, successful or failed, adds a record: the record map's and the inventory's sizes after the body equal their values before it, and only a cut adds records | S1 `law_abort_budget`, `law_model`; S3 | an in-place edit of a page the head shares (AP10); a privacy test by stamp without `with_unique`; an F2 path that publishes the Tx's data root; an apply that edits the head before its last fallible step; a replay edit that keeps the old stamp and is lost after the segment retires; a record holding a body share (fails (e)'s allocation count); a record created by a commit |
| **L40 old-schema reads** | rows written under schema 0 are hand-encoded (`00 00` ‖ the old codec bytes, the **raw bytes**) and read after `table` appends schema 1 with a defaulted field. **Raw, once for every handle:** `fetch_bytes`, `get_bytes`, `scan_bytes` and `Change.value`, on the head and on a Snapshot pinned before the upgrade, return exactly the raw bytes. **Typed, per Shape:** through the schema-1 Shape, `fetch`, `get`, `range` and decode by schema dispatch return M's value with the default filled, and `published_value` returns N, the new codec's bytes of that value with no schema prefix (AG3); through a schema-0 Shape held from before the upgrade, on the pin, `get` returns the old value and `published_value` returns O, the old codec bytes without the `00 00` prefix. Only the raw paths return `00 00` ‖ O (H13). No read changes any page stamp. The owned and `Change.value` assertions are in the cutover slice; the borrowed ones (`fetch_bytes`, `get_bytes`, `scan_bytes`) come with M1-6 | S1 `law_schema_numbers` | a view path that drops the schema number; a raw path that transcodes; a transcode written back; `published_value` encoding for the stored schema instead of the Shape's; a `published_value` that keeps the schema prefix |
| **L41 stamps survive relocation** | after compacting cuts that move pages and blobs: every moved frame's (pack, offset) is observed changed in the MANIFEST and frame stream while its stamp is unchanged, including (i) a cut that moves only a blob, whose leaf and ancestors are rewritten with zero selected PAGE frames; (ii) a shared leaf reached through k = depth + 2 distinct parents from k divergent heads, all k parents rewritten once; (iii) B2's case: a subtree with live children in packs 3 and 9, pack 3 dense and pack 9 sparse and selected alone, with the only surviving path to the moved child under a pin; (iv) B3's case: two successive relocations with both old parent copies still listed in dense packs; (v) N13's case: at one cut parent P points to child c in pack 9; before the next cut a commit replaces P by a fresh parent Q that still shares c, and that cut selects pack 9: c is written once at a new location with its stamp, Q is written once pointing there, P is dead, and every pack's `live` equals the hand sum of its live frames' payloads; (vi) R6's case: P owns c and d; cut 1 moves c and rewrites P (same stamp, new location); cut 2, with no reopen between, selects d's pack: d moves once and P is rewritten once more, naming both new locations; then `changes(a, b)` for a before and b after is empty, every data edge reachable from the new MANIFEST resolves into a listed, unretired pack of its class and every list edge into a reached, unretired class-2 pack (by an independent parse of the files), and reopen gives every root's M by an independent row scan; (vii) **optional relocation (U13):** with the writer's limit set so relocation's allocation is refused, two cuts each publish, covering their journal and writing their fresh frames with no data relocation (by an independent parse, every selected pack still listed); with the limit set so the list edits are refused only while relocation is in the candidate, the cut drops relocation, redoes the list edits once and publishes; a cut with room then relocates the selected packs, and a reopen gives every root's M and every pack's `live` equal to an independent scan of the files; **after an earlier rename (C-6):** with the limit set so that a record insert of step 3 is refused after the cut has renamed at least one pack of its relocation layout, the cut restores its retry base, re-emits once from the installed `next_pack` without relocation and publishes. An independent parse of the files then gives a reachable graph, pack inventory and list rows equal to the hand-derived no-relocation layout, `first_live` equal to step 1's successor, list stamps above the first try's, and leftovers only at contiguous seqs above the published `next_pack`, which reopen removes. It catches a retry that keeps the discarded layout's dead counts, pointers or list edits | S1 `law_changes_rewritten_equal`; S2 `checkpoint` | a moved page or a rewritten ancestor given a new stamp; a closure found by `low` descent (misses (iii)); a closure bound of moved × depth (misses (ii)); an eager loader that rejects a repeated stamp in a listed dead frame or picks the first frame by stamp (fails (iv)); incoming entries removed before the fresh ones are added, or a closure walked over the installed generation's incoming lists (fails (v)); a parent link kept by the parent's old location (fails (vi)); a relocation refusal that refuses the cut, or a redo that keeps relocation's marks (fails (vii)) |
| **L42 pinned and divergent roots** | a hand-specified sharing graph: pin at v, fork b at v (by the cut path in the cutover slice, and by the journaled path once it lands), divergent commits on main and b, `drop_table` of an indexed table on main while b and the pin still hold it, compacting cuts, reopen: each root, and each of the table's and its indexes' ranges on b and the pin, equals its M; an independent frame log shows each page and blob stamp written at most once per cut; MANIFEST `account` equals the hand sum of AP15 over the distinct row versions plus 126 per distinct collection id (B4's case below gives C0 + 258) | S1 `disk_law_branches`, L7 | a closure walked from main only; a version charged once per leaf; a collection map copied instead of shared; a drop that leaves index entries |
| **L43 journaled fork recovery** | at every S3 cut around a FORK frame and the commits after it, with the provider's write, sync and ack events observed independently. **Oracle (H1, N4):** the reopened state is a complete prefix of the ordered COMMIT and FORK events that holds the **observed durable prefix** (every fork that returned a Branch on disk, and every event at or below a `durable` barrier that returned `Ok`) and may hold later events whose complete frames survived under §3.4's segment rules; an ordinary `run` that returned without a durable barrier may be lost; a frame torn below a known `d` or `prev_end` is `Corrupt`; no commit on a branch survives without its FORK; the branch's head equals M at `forked_at` plus its own surviving commits; `recorded` on either branch equals its CALL model. Images: FORK complete and unsynced; absent; torn; stale and bad END; a successor segment; **a synced fork followed by a child COMMIT from `run_on` that returned and was never written** (reopen keeps the fork and lacks the commit); **a FORK whose segment sync succeeded and whose ack delivery was cut** (`fork` never returned, `beta` survives as 1, the next fork gets 2). **B1's case** (and store-write-path-1609's open gap, exact reissue above the watermark): on a store whose MANIFEST has `next_branch` = 1, the crash comes after FORK `beta` is written and before its sync, with the frame absent or torn: `fork` never returns, reopen gives `next_branch` = 1 with no `beta`, and a new `fork("gamma")` gets id 1, the exact reissue of an id above the MANIFEST watermark, legal because no handle escaped. With the FORK synced and the handle returned, reopen keeps `beta` as 1, a new fork gets id 2, and the old handle reads, writes and deletes `beta` only. The same with a cut-path fork refused `Full` before a journaled fork: replay accepts the skipped id. **Export (H9), each image with its own expectations:** (1) a captured prefix ending exactly at FORK `beta`: `replay_export`'s recovery (the preparation's memory Db) has `beta` with id 1, main's roots at `forked_at`, version `forked_at`, main's `at`, and `next_branch` = 2; (2) a prefix holding, after the FORK, one CALL commit on main and one on `beta`: each branch's head is its own CALL commit's roots and version (`forked_at` + 1 on `beta`), `beta`'s `at` is its CALL's, `next_branch` = 2, and the CALL contents are checked by parsing the saved prefix's frames directly and by `recorded` on the live disk Db before any cut; (3) after a covering cut, a new export recovers the same heads from the MANIFEST, and `recorded` on the live Db for the covered CALLs is `None` (§1.3); (4) the export leased before that cut still recovers image (2)'s heads | S3, scenario `journal-fork`; S1 export laws | replay applying a FORK before the parent's commit at `forked_at`; a fork returned before its sync (the reissued id 1 then aliases `gamma`); a fork ticket returned durable early; `recorded` or `replay_export` decoding a FORK as a COMMIT; replay requiring id = `next_branch` exactly; an oracle that demands every returned `run` |
| **L44 fork cost** | with the journal drained and the journal bytes well below `checkpoint_bytes` (so no idle END sync or threshold cut lands inside the observation), a live-head fork of a 100,000-row head under a pin with conservative charges: the provider observes exactly one frame of 16 + 18 + n bytes with kind 0x0013, one sync, no pack write; the walk, settle and cut entry points are each entered zero times and no page is visited (by the law's page-visit counter); `accounts(db)` is unchanged except `exact` stays false, `frames` returns to 0 and `journal` grows by exactly 16 + 18 + n; a fork from an older Snapshot still makes a cut | S1 `law_branches` with the S3 provider | a fork that still walks (and discards the answer) or cuts; a fork that copies the collection map |
| **L45 BlobRef** | stored values of 1024 and 1025 bytes, 1 MiB, and `VALUE_LIMIT`: the independently parsed frame stream shows vtag 0 and no BLOB frame at 1024, vtag 1, a 42-byte descriptor and one BLOB frame in a class-1 pack at 1025; all round-trip through commit, cut, compaction of class-0 packs (no BLOB frame written) and of a class-1 pack (each blob written once, its leaf rewritten), and reopen; a value one byte over `VALUE_LIMIT` is `Limit(Value)`; a stored 12 MiB value reads back under an open whose `VALUE_LIMIT` is 8 MiB (a test build of the constant); a flipped blob byte is `Corrupt(Checksum)` at open. **Threshold (H8):** a hand-built store whose MANIFEST `inline` is 512 writes 512-byte values inline and 513-byte values as BlobRefs, and after a cut and a second reopen its MANIFEST still says 512 and both forms hold. **Sidecar (N11):** one Tx replaces one row's 2,000-byte value 70,000 times: the leaf's `blobs` length stays 1 throughout (by the law's page inspection), and a Snapshot pinned before the Tx still reads the first value's bytes | S1 `law_put_validation`; S2 `oversize` | an off-by-one at the threshold; a writer that uses BlobRef at both sizes; a blob copied with its leaf; a blob read without its checksum; a decode that checks the write limit; a cut that rewrites the threshold to 1024; a private page that keeps superseded bodies (its `body` passes u16) |
| **L46 per-collection count and drop** | after random puts, deletes, overwrites, `create_table` of an empty table, `drop_table` of a table with two indexes, and recreating its name: each `CollectionRoot.rows` equals M's count and `count` over every sub-range equals M's; the drop removes exactly three map entries and changes no other collection's root stamp; an index whose encoded key is unchanged by a value-only put keeps its root stamp and emits no page (AP11); equal encoded keys in two collections, across a page split, are kept apart (AP5) | S1 `law_typed_model`, `law_drop_recreate` | `rows` not decremented by a delete; a drop that touches other collections or leaves an index; an unchanged index rewritten; a stripped prefix that crosses the id |
| **L47 page corruption and misdirection** | flipping any byte of a PAGE or BLOB payload is `Corrupt(Checksum)` at open; with the outer frame check recomputed, a flipped inner page byte is still `Corrupt(Checksum)` by the PagePtr checksum; a MANIFEST root PagePtr, a branch entry's PagePtr and a map descriptor's PagePtr each rewritten (with every enclosing check recomputed) to name another valid page of the store, with that page's checksum and the **original** expected stamp, is `Corrupt(Checksum)` by the stamp check; a BlobRef misdirected to another blob likewise. **Negative fixtures**, each independently resealed and each changing one thing: base tag 1 (`Unsupported(0x0031)`) and tag 2 (`Malformed`); residency 1 and tree 1 (`Unsupported(0x0021)`); a slot past heap; a slot that aliases the previous entry's bytes (`Malformed`, M3); a collection-leaf key with 4 + P + klen = 4097 (`Malformed`); a collection-map key of 5 bytes (`Malformed`); a `body` past the BlobRef count; a branch entry with the empty PagePtr; a map descriptor that names its own leaf (`Order`, decided before any checksum); a PagePtr into a class-1 pack; a frame stamp at `next_stamp`; a retired kind with MUST = 0 (`Unsupported`); a list-tree PagePtr into a listed pack (`Malformed`); a seq in both `packs` and `retired` (`Malformed`); a reached class-2 seq that is also a `retired` row (`Malformed`); a reached class-2 seq at `next_pack` (`Malformed`); pack-list seqs that ascend within each leaf but not across two leaves (`Order`); a reached class-2 file with a bad HEADER, or with an unreached frame of a retired kind (refused as a listed pack's); a list PagePtr whose offset falls inside another frame's payload, where a valid PAGE envelope with the right stamp and checksum has been planted (`Malformed`: the stream never visits it as a head); one stamp reached as a list page and as a data page at two locations (`Malformed`); a reached class-2 file holding a skippable extension frame larger than 8,400 bytes and within `UNKNOWN_LIMIT` (accepted, checked in chunks) and one over it (`Corrupt`); a pack-list row with class 2 (`Malformed`); a MANIFEST payload of `MANIFEST_LIMIT` + 1 bytes | S2 (L12's flip sweep); F | a load checked by the frame check alone; a stamp not compared with the header; a missing order check on map edges; a retired kind skipped as unknown; span checks that let slots alias |
| **L48 cutover refusal** | a frozen valid format-2 directory (committed fixture, not written by the current writer) opens as `Unsupported(2)`, and every file in it is byte-identical after the refusal; hand-built format-3 files holding a 0x0020 frame or a 0x0030 frame open as `Unsupported(0x0020)` and `Unsupported(0x0030)`; neither case writes genesis | S2 `genesis` | a dual reader; a retired kind decoded; cleanup or genesis before the refusal |
| **L49 bulk build and splits** | `migrate` with a callback that reverses keys (1→20, 2→10), one that drops every other row, and one that maps 1 and 2 to 10: the first two equal M and every leaf's keys ascend (independent parse); the third is `Exists` with nothing published and the account unchanged. **Workspace (H5, R2):** a migrate of 10 rows into a table with one index, each record charging entry(12, 6) + 8 = 94 and each index entry entry(20, 0) + 8 = 94, with A the settled account before the migrate, K the hand-computed entry(k, v) of the target's two catalog rows (from their encoded bytes, by the law), and the limit A + 2 × 126 + K + 13 × 94 = A + K + 1,474: the 14th charge makes W = A + K + 1,568 and it is `Full(Resident(Capacity{requested: A + K + 1,568, available: A + K + 1,474}))`, with nothing published and A unchanged. **Empty pinned migration (R2):** a store with one empty table T under a pin; migrate T to a fresh name with no records and no indexes, with K₁ the new catalog row's entry by hand: under the limit A + 126 it is `Full(Resident(Capacity{requested: A + 126 + K₁, available: A + 126}))`; under A + 126 + K₁ it publishes with A = A + 126 + K₁ (the pin keeps T's row and descriptor), and a reopen gives the same exact figure and is not `Full`; without the pin the same migrate publishes A − entry(T's row) − 126 + 126 + K₁. **Bulk separators (R8):** a bulk build with enough rows for two non-root branches L and R: the root's separator for R, decoded independently, is non-empty and lies after L's last key and at or before R's first key, and lookups of L's last key and R's first key each find their row. Three leaves with keys sharing about 4,000 prefix bytes, so that a two-child branch exceeds T: the build gives the single legal branch of three (AP13's last-group rule) and every page is legal. A level whose greedy pass ends with one entry after a group of three gives groups (2, 2). **Incremental split (N6):** a branch with children A0, A1, B, C, the A0/A1 separator 4,001 bytes and the others one byte (made by splitting A in a legal parent A, B, C) splits 2 + 2, and no non-root branch is unary. **Borrow:** a unary branch borrowing from either side gives the separators AP3's rotation states. Each branch's entry 0 has slen 0. **L5 with indexes:** a put that changes both index keys of a table with two indexes writes at most (1 + 2 × 2)(3 h_c + 1) + 3 (3 h_m + 1) pages at the next cut | S1 `law_migrate` (L31's) | an unsorted build after a reversing callback; a duplicate silently dropped; workspace held before it is charged; an illegal last branch; a 1 + 3 split leaving a unary branch; a borrow that loses the bridge separator; a non-empty entry-0 separator; a workspace or publication that leaves out the catalog rows; an empty separator passed up a level |
| **L50 account history independence** | **B5's case:** in one body insert u64 keys 6, 5, …, 1 with 1,000-byte stored values (the live leaves are [1,2,3] [4,5,6], and replay's are [1,2] [3,4,5,6]); fork the live head; overwrite key 6 on the branch. The account is C0 + 7 × 1,078 = C0 + 7,546 live, after a reopen with no cut, after a cut, and in a captured export's recovery; with `resident_limit` = C0 + 7,546 every step is admitted, **on a memory store and on a disk store alike** (frames are not charged, AP15), and the reopen is not `Full`. **B4's case:** pin a leaf holding keys 1 and 2 (values 6 bytes; exact C0 + 172), overwrite key 2 on main with another 6-byte value: the admission figure is C0 + 3 × 86 = C0 + 258 before and after the settle; with the limit C0 + 258 the put is admitted, with C0 + 257 it is `Refused(Resident(Capacity{requested: C0 + 258, available: C0 + 257}))`. **Delete-only (N2):** at the limit C0 + 258 on disk, with the journal's acks held back by the provider, with F drained to 0 and then the acks held back, deleting key 1 on main is admitted although its COMMIT frame is 59 bytes. **Frames (R1), at production sizes:** with F drained to 0 and the provider holding acks, hand over a 9 MiB COMMIT, then attempt a second 9 MiB COMMIT: it waits at handoff (observed) with F equal to the first's length, and the owned buffer capacities measured during the second frame's sealing, with the first still queued, are the first frame in the worker, the second's one output buffer (no separate payload buffer), the group buffer and the fixed scratch, and for a 10,000-key commit also an ordering list with 10,000 occupied slots (80,000 bytes) whose measured capacity is 8 × G, with G ≥ 10,000 the slots `try_reserve` granted, read from the list (C-11), and no copy of any key; releasing the held ack lets the second complete; in another run the worker fails while the writer waits, and the commit is `Poisoned`. Both again across a rotation, and with `group_bytes` = 64 MiB (a 64 MiB group buffer, not counted in F). **Exact flag (R7):** pin, put a key only main holds, settle (`exact` true), delete the key on main: A unchanged and `exact` false; a settle then gives `exact` true with A lower by that row's entry; a live-head fork keeps `exact` true. **(c) Paused cut (N1):** under B4's pin overwrite key 2 twice (A = C0 + 344, exact C0 + 258) and checkpoint: after it publishes, A = C0 + 258 and `exact` is true; a put of new key 3 then makes A = C0 + 344, the exact account of keys 1, pinned 2, newest 2 and 3. **(d) One-root settle (N10):** two stores with 300 empty tables, so the collection map has a branch page, and one nonempty table whose map entry sits in a leaf below that branch, its rows 1 and 5 rows of 6-byte values respectively: the map branch entries report equal `bytes` in both stores (every descriptor has one length), and the settle gives 126 × 301 plus the catalog and streams bytes plus 86 or 5 × 86, matching the hand sum. **(e) Journal cap (C3):** on disk with `checkpoint_bytes` = 4,096, `journal_limit` J = 4,096 + `FRAME_LIMIT` = 16,781,392 (the floor) and every cut refused by a test allocator: two COMMITs bring U to exactly J, each admitted (C-5): a 4,096-byte one (one put, 12-byte key, 4,033-byte value: 63 + 4,033), which does not pass `checkpoint_bytes`, and a 16,777,296-byte one (two puts with 12-byte keys and values of 16,711,680 and 65,532 bytes: 16 + 21 + 5 + 2 × 21 + 16,777,212), which reaches J at equality and whose crossing cut is refused. `accounts(db).journal` is then 16,781,392. A delete-only commit of one key (a 59-byte frame) is then `Refused(Journal(Capacity{requested: 16,781,451, available: 16,781,392}))`, with A, F, U, the heads and the provider's frame log unchanged; so is a body that first overwrites key 1 and then deletes it (C-10). Its frame is still 59 bytes, because the Tx keeps one op per key (U10), and its first write copied pages and took private stamps. The heads and U are unchanged. An `admit` of an 8-byte stream is `Full(Journal(Capacity{requested: 16,781,492, available: 16,781,392}))` (f = 100). A live-head fork with an n-byte name is `Full(Journal(Capacity{requested: 16,781,426 + n, available: 16,781,392}))` with `next_branch` unchanged. **`serve` and `conclude` at the cap (C-3), through the public entry points:** admit a stream while there is room, then let an intervening commit fill U so that `serve`'s data-and-reply COMMIT does not fit. `serve` returns `Before(Full(Journal(c)), None)` with c hand-computed, the stream reads Admitted, a second `admit` returns `Abandoned`, and the provider's frame log is unchanged. A `serve` that returns `Failed(p, stop)`, followed by a commit that fills the journal, makes `conclude(p)` return `Before(Full(Journal(c)), Some(p′))`. After `checkpoint`, `conclude(p′)` commits, and `recorded` gives settle 1 with p's original base, `at` and seed. A keyed claim refused at the journal three times never takes the runner's third-refusal conclude, and no settle-2 record appears. With J raised and `resident_limit` lowered to the same state, `serve` returns `NoRoom(p, Resident(c))`; a reopen of a copy of that directory opens with `journal` = J and refuses the delete the same way; with the test allocator removed, `checkpoint` publishes, `journal` is 0 and the same delete is admitted | S1 `law_branches`, `disk_law_branches`; S3 | a page-union account (B5 gives C0 + 10,780 after replay against C0 + 9,702 live; B4 gives C0 + 344); a version charged per leaf; a delete charged for its copied leaf or its frame; a frame counted in its own wait; an `exact` flag left true after a conservative delete; a delete refused by admission at the limit; a cap that waits for a cut instead of refusing; a cap checked after the fork's id is taken (fails `next_branch`); a cap computed from acked bytes; a delete exempted from the cap; a journal refusal mapped to `NoRoom` or counted as a resident refusal; a `conclude` refusal that loses its Pending or rewrites its context; an index list observed by its occupied length |
| **L51 fork admission and limits** | (a) a fork whose record brings manifest_bytes to `MANIFEST_LIMIT` exactly succeeds and one byte more is `Limit(Manifest)` with nothing sent, on an empty tree; the same at equality and one byte over for `pin`; and the cut after each success is written, never refused for size; (b) with `next_branch` = 2³² − 1 (a test MANIFEST), `fork` is `Limit(Ids)`, and a journal holding a FORK with id 2³² − 1 is `Corrupt(Malformed)` at open; (c) `fork("", s)` succeeds on the live-head path and on the cut path, and both reopen with the empty name; (d) **same roots, same answer (N3):** under B4's pin, two overwrites of key 2 with acks drained at the limit C0 + 450 (A = C0 + 344, exact C0 + 258); save the directory before any fork; continuation 1 forks a 100-byte name on the live Db (A conservative), continuation 2 reopens a copy of the saved directory (A exact, C0 + 258) and forks the same name, with the same MANIFEST headroom: both are admitted and both return id 1; (e) install (§11.1): a prepared range of 16 single-row oversize leaves with 16 blobs, built while OLD commits, reserves 2 × 16 + 16 + 1 = 49 stamps and no OLD stamp falls inside them; a prepared release that drops a populated table and its index passes validation with its two zero-count drop records, and removes both map entries and both 126 charges; a replacement with zero rows leaves one empty entry; (e′) **target options (C-1):** take two preparations of one release from one OLD, identical but for NEW's `journal_limit`, with NEW's `checkpoint_bytes` = 4,096. At 16,781,392 the install activates. At 16,781,391 it is `FullNew(Journal(Capacity{requested: 16,781,392, available: 16,781,391}))`, and OLD stays selected with its options unchanged. An independent parse of the two PREPARED files shows 0x0081 payloads of 212 + d bytes that differ only in the eight bytes at 184 + d. A `describe` of 1,048,620 bytes prepares, and one of 1,048,621 bytes is `Scratch(Limit(Value))` with no PREPARED file; (f) **keep (N7):** a release that appends schema 1 to a populated table with two indexes, with no inventory range for it, keeps every row, both index ranges and all three root stamps through activation and reopen; (g) **paged lists (N8), scaled geometry:** a test build with a `pack_bytes` that puts one blob in each class-1 pack and a page target that holds four list rows per list page; write more live blob packs than one list page and one list branch hold (pack-list height ≥ 3): every cut succeeds and its MANIFEST is 152 bytes plus its records; a cut crashed after its class-2 pack is written and before its MANIFEST rename leaves a class-2 pack that reopen ignores and cleanup removes; the reopened lists equal the hand list of packs; and a fork is admitted however long the lists are; (h) **preparation (N5):** a fork whose branch-list growth is refused (a test allocator) returns that error with nothing sent and `next_branch` unchanged; a failed send poisons the Db with no branch installed; on a memory store the returned handle and `branch(db, name)` both find the branch; (i) **cut path (R4):** advance main from v1 to v2 after taking Snapshot s at v1 and fork s: the branch's rows are v1's, before and after reopen; take Snapshot s of branch b, delete b and fork s: the branch exists with s's rows and §1.9's parent and `forked_at`, before and after reopen; (j) **list stamps and retirement (S1, S2):** in (g)'s geometry, a cut that adds a pack-list leaf, followed by a commit: every list page's stamp is below the cut's `next_stamp`, the commit's new stamps are at or above it, and live stamps stay unique after the cut publishes, after a crash before the rename and replay, and at the next cut; (j′) **moved list pages and discarded retirement (T3):** a cut that selects a class-2 pack holding an untouched pack-list leaf with stamp 40 writes that leaf into a new pack with stamp 40 (by an independent parse), and no page of the old retirement list appears in any file the cut writes; a cut that replaces the last reached page of class-2 pack A while the old retirement list's only page is in class-2 pack B makes both A and B rows of the new `retired`, and a class-2 pack holding an untouched pack-list sibling is not a row; with an export lease on that generation held through one more cut, A and B stay on disk and the first cut after release removes them; (k) **replayed record bytes (S10):** a resealed journal suffix whose FORK names take T one byte past `META_LIMIT` is `Corrupt(Malformed)` at open and in export recovery, and at equality it opens; (l) **the location table (U8):** for every cut in (g)'s geometry and in a random sequence of commits, forks, pins, deletes and cuts, after each published cut the writer's record map and inventory equal a table built by an independent parse of the files, as L41's and L42's are: every record's location, checksum and out-edge stamps, and every pack's `live` and `bytes`. Incoming entries are compared as multisets (C-9). A holder entry is named by stable identity, a branch by id and a pin by name, with its tree. The writer's per-open pin serials are mapped to names through a test projection of its serial-to-name association, and the law checks that the association is injective. For a file a lease retained, the parse also takes the lease history (which packs and segments each held lease kept) from the law's own log; then `delete_branch` of every branch but main publishes in one cut, and the reopened lists equal an independent parse | S1 `law_branches`; S1 import laws (`import_read_laws`) | a fork that cannot be encoded at the next cut; an unrepresentable id; an unreplayable empty name; a conservative frame charge that makes the same roots answer differently; an import stamp aliasing OLD's; a dropped range kept as empty; a catalog-only edit that clears a populated range; a fork made durable into a full MANIFEST; an installation that can fail after the sync; a cut refused for list size; a cut-path fork built from the parent head; a list page stamped below a watermark the writer reuses; a one-pass retirement that leaks a class-2 pack; a replayed FORK past the record ceiling; a moved list page given a fresh stamp; an emitted discarded retirement page; a location table that drifts from the files, by a missed incoming entry or a stale location after relocation (fails (l)); a PREPARED target that loses NEW's journal limit, or an install that checks OLD's options (fails (e′)); a `delete_branch` that publishes in two steps |

**Laws restated for M1** (each statement kept, wording moved to pages):
- **L5 (H6, N6).** Per one-key commit the pages a cut writes are at most (1 + 2i)(3 h_c + 1) + (1 + i)(3 h_m + 1), plus the closure of any relocation, where i is the number of the table's indexes whose encoded key the put changes (each such index edits two keys of its collection, one delete and one insert, and at most two paths), h_c ≤ 1 + ⌈log₂ n⌉ is the tallest edited collection tree's height for n rows, and h_m ≤ 1 + ⌈log₂ m⌉ is the collection map's for m collections (AP3's occupancy, which holds because no split or borrow leaves a non-root branch unary). Derivation: each edited path's pages split into at most three, each tree may gain one root, and each of the 1 + i edited collections changes one map entry and so copies one map path. With i = 0 this is 3(h_c + h_m) + 2. k commits write at most k times this. **List pages are counted beside it (S8):** at most r_p (3 h_p + 1) pack-list pages, where r_p is the number of pack-list rows the cut adds, removes or changes and h_p ≤ 1 + ⌈log₂ n_p⌉ the pack list's height for n_p rows, plus at most 2 r_r + 1 pages of the rebuilt retirement list for r_r rows. The relocation closure is counted once. The independent frame counter counts data, map and list pages separately. S8's case (a hot leaf and a map leaf, each of height 1, and one added pack-list row whose insertion splits a full three-level right spine through the root) writes 2 data pages ≤ 8 and 2 leaves + 4 internal pages + 1 root = 7 list pages ≤ 1 × (3 × 3 + 1) = 10. The streams and catalog trees are untouched by a data commit. Hand-derived; a run never sets it.
- **L28 (restated: a refused checkpoint loses nothing, its removals take constant space, and the journal cap has a floor; N8, T2, T11, U12, C3; v2.40 C-2, C-5, C-6, C-9).** (a) **Refusal at each allocation point:** with a test allocator that refuses, in turn, the rotation buffers, the first candidate record, a list page, the pack buffer and the MANIFEST's encoded fields, `checkpoint` returns `Full(Allocation(c))` with c the refused request, and nothing changes. **The copy is taken after a drain (C-9):** the law first calls `durable` on the newest ticket, so F = 0 and no ack is outstanding, then copies the heads' versions, A, `exact`, `logical`, `journal` (U), `frames` (0), the record map's and inventory's sizes, and the bytes of the old MANIFEST and of every listed pack. After the refusal each equals its copy, because step 0 has nothing to drain. The refusal points also include a record insert of step 3 after the first pack rename (C-6). A crash then and a reopen give M. A crash at each S3 point across step 4 (after `MANIFEST.tmp`'s sync, after the rename, after its `sync_dir`) reopens to M from the old image and to the published generation's M from the new one, both by an independent parse. A second `checkpoint` with the allocator removed publishes. The same holds for `pin`, `delete_branch` and an install, whose refusal is `FullNew(Allocation(c))` with OLD selected. (b) **Constant space, by a counted allocator:** under an export lease held through k cuts, so that the covered interval grows to k segments, the removal step's allocation count after its name buffer is allocated is zero for k = 1 and for k = 1,000 (a removal list of owned names grows with k). (c) **A lease cuts a hole (U12):** with `first_live` 7, a `checkpoint` refused by a test allocator after AP16 step 1 rotates to segment 8 and leaves `first_live` 7 (step 1's rotation stays); an export then leases segments 7 and 8 (§3.5a: the journal prefix from `first_live` to the terminal segment) and their END files and is held, before its copy reads 7, through two cuts that make 9 and then 10 current. The cut to 9 unlinks nothing, because its covered interval [7, 9) is all leased. The cut to 10 unlinks exactly `j-9` and `j-9.end`, keeps both names of 7 and 8, and writes `prev_first` 7. The export then recovers its captured prefix and is released. Its acknowledged CALLs are checked by an independent parse of the saved segment prefix through the capture's terminal offset, and its recovered heads through the prepare opener. `recorded` on the copy cannot be used, because an ordinary open refuses an EXPORT directory and the preparation Db is memory-only, where `recorded` is None (C-9). and the cut to 11 attempts [7, 11): it removes `j-7`, `j-7.end`, `j-8`, `j-8.end`, `j-10` and `j-10.end`, and `j-9` is NotFound and ignored. (d) **The floor, without a store (C-5):** open's options step is checked as a function of `Options`, with no directory and no LOCK. With `journal_limit` = `checkpoint_bytes` + `FRAME_LIMIT` − 1 it is `Full(Journal(Capacity{requested: checkpoint_bytes + FRAME_LIMIT, available: checkpoint_bytes + FRAME_LIMIT − 1}))`, and at equality it passes (83,886,160 at the defaults). `checkpoint_bytes` = 18,446,744,073,692,774,319 with J = 18,446,744,073,709,551,615 passes, and one more byte of `checkpoint_bytes` is `Full(Journal(Capacity{requested: 18,446,744,073,709,551,615, available: 18,446,744,073,709,551,615}))`. **The restart case:** L50 (e)'s floor store, crashed after its 16,777,296-byte frame's sync and before any cut publishes, reopens with `journal` = 16,781,392 and refuses the next one-key delete with `Refused(Journal(Capacity{requested: 16,781,451, available: 16,781,392}))`. After a `checkpoint` it admits the delete. (e) **Prospective holders (C-2):** take four cuts: (1) `delete_branch` of a branch that alone holds a committed row in its own pack; (2) a `pin`; (3) an install's graft; (4) a `checkpoint` that relocates main's root page and changes no holder. Crash each at the S3 points before and after its MANIFEST rename. An independent parse of each image finds one of two states. Either it is the old MANIFEST, with the old holders, versions, roots and packs. Or it is the new one, with exactly the prospective holders: no record of the deleted branch, the new pin, the grafted roots. In the new one every root PagePtr resolves into a listed pack and none into a retired one, (4)'s root is at its new location, `first_live` is the segment step 1 made, and `next_pack` is one past the last pack written (unchanged for a cut that wrote none). The same four, with the cut refused at its MANIFEST fields, leave the live records, `branch(db, name)`, the pins and `next_table` as they were. It catches a refused cut that changes the installed table or leaves `first_live` behind, a removal list of owned names per segment, a removal interval that unlinks a leased segment or its END, a `prev_first` that forgets a deferred segment, a carried retirement dropped on release, a cap with no floor, a floor check that needs a store or overflows, a snapshot of F taken before the drain, a MANIFEST encoded from the live holders (fails (e)(1)), a holder root encoded at its old location after relocation (fails (e)(4)), and a live record edited before the commit point (fails (e)'s refusal).
- **L29 (restated for AP1a; V2).** (a) and (b) are kept: a data-only COMMIT is byte-identical, so the boundary at cap − 1, cap and cap + 1 is unchanged. (c) is kept. (d) **The call rides its reply**, checked on hand-built COMMITs, each resealed, in L29's harness and the journal segment law:
  - A valid call commit (a one-key data put, and on stream "s0000001" a tag-3 op whose value is state 2 with an 8-byte reply, followed by its call with L = 32) is 313 bytes by §2.4's widths. Replay applies the put, and `recorded` returns its `CallRecord` with stream "s0000001".
  - Each of these is `Corrupt(Malformed)` at open and in `recorded`: a tag-3 op whose value's state is 1; a tag-3 op in the data section; two tag-3 ops; a COMMIT with a section of tag 2.
  - An Admitted put and a delete of the stream each decode with no call.
  - **Duplicates (C-8):** each of these is also `Corrupt(Malformed)` at open and in `recorded`: two streams sections; a streams section before the data section; and a tag-1 put or a tag-2 delete of "s0000001" beside its tag-3 op, in either order.
  - **Settled calls (C-8):** take the same commit with settle 1, and again with settle 2, each with base 3, `at` 1,000 and seed 0x1122334455667788. All three differ from the enclosing frame's version, `at` and seed. Each is 337 bytes (313 + 24). `recorded` returns every `Recorded` field equal to its hand value: the record's key, args, revision, code (with its replay identity) and stream "s0000001", the settle, the branch, base, `at` and seed. Replaying each frame leaves the same heads, rows and streams as replaying the hand-built frame whose tag-3 op is a tag-1 put of the same key and value. The runner's recorded-code identity check is unchanged.
  - **W2 after two refused cuts (C-8):** commit the 313-byte call frame, then make two cuts that a test allocator refuses after their rotations, so the frame's segment is two rotations behind the current one. `recorded` at its version returns the call. After a published cut it is None.

  It catches a CALL that names an Admitted put, a delete or a missing op; a stream name encoded beside the op that can disagree with it; a size computation that still counts the stream field; a second op of the call's stream that replay would apply over its reply; a settle context taken from the enclosing frame; and a W2 fallback that counts a refused cut.
- **L27 (restated with AP15; N9).** The cases use the built memory fixture's constants: its table's catalog row is 134 bytes, a collection descriptor 126, and one row version 94, so one row costs 134 + 126 + 94 = 354 and two cost 448. Each case starts from an admitted history under its own limit. (a) as built, with the row-version account. (b) **the two figures differ:** keys 1 and 2 (448 exact), pin, limit 600. Overwrite key 2: 542 conservative and exact, admitted. Overwrite key 2 again: conservative 636 passes 600, so it settles; the proposed roots hold key 1, the pinned 2 and the newest 2, exact 542, admitted with A = 542. Then put new key 3: conservative 636, settled exact 636, `Refused(Resident(Capacity{requested: 636, available: 600}))`. (c) **AF1:** the same pinned start under limit 660, two overwrites admitted conservatively (A = 636, exact 542); `admit` on a new stream with an 8-byte name (118) would make 754, so it settles to 542 + 118 = 660 and is `Admitted`. Under limit 802, three overwrites (A = 730, exact 542); `create_table` of an empty table whose catalog row is 134 bytes would make 990, so it settles to 542 + 134 + 126 = 802 and succeeds. The S1 fork cases: (i) one row (354) under limit 500: a live-head fork by the journaled path leaves A at 354 with zero page visits (L44; in M1-5); (ii) write the row, hold a Snapshot s, overwrite the row with a different same-size value (354 exact): `fork(s)` under limit 447 is `Full(Resident(Capacity{requested: 448, available: 447}))`, and under 448 it succeeds; (iii) a live-head fork under a pin with A at the limit is admitted with A unchanged (it adds 0 and settles nothing).
- L6, L7, L12, L23 (stamps for node ids; `changes` checks the open before any stamp skip), L25 (PAGE for NODE; H1 unchanged; class-specific pack bounds). L29 is restated below.
- **M6's watermark cases:** `disk_watermark_cut` from f75e9b14 and both its absent and torn variants are ported to reissued table-page, map-page and blob stamps: an interrupted cut writes and syncs a higher-stamped page into an unlisted pack, reopen reissues those stamps, and the reopened store reads M, an old Snapshot of the previous open still reads its M, `changes` between Snapshots of the two opens is `Foreign` in both directions, `pin` and `fork` of the old Snapshot are `Foreign`, and a fresh fork succeeds (S2).

### 11.16 API surface M1 adds

```text
pub fn fetch_bytes[K, V](tx: Tx, t: Shape[K, V], k: K) -> Result[Option[View[Bytes]], Error] in tx
pub fn get_bytes[K, V](s: Snapshot, t: Shape[K, V], k: K) -> Result[Option[View[Bytes]], Error] in s
pub opaque scoped type ViewRows                           // scoped to its Snapshot; not send (AP12, OWN-22)
pub fn scan_bytes[K, V](s: Snapshot, t: Shape[K, V], lo: Bound[K], hi: Bound[K]) -> Result[ViewRows, Error] in s
pub fn next_view(mut c: ViewRows) -> Result[Option[Bytes], Error]     // the next row's logical key, or None at the end
pub fn value_view(c: ViewRows) -> View[Bytes] in c                    // the current row's stored value
pub type Accounts { logical: u64, exact: bool, frames: u64, journal: u64, resident: u64 }
pub fn accounts(db: Db) -> Accounts
pub const VALUE_LIMIT: u64 = 16711680
pub const FRAME_LIMIT: u64 = 16777296                   // bytes of journal frames in flight (AP15)
pub type Limit = Key | Value | Transaction | Name | Ids | Manifest
pub type StoreCapacity = Journal(Capacity) | Resident(Capacity) | Allocation(Capacity)   // v2.40 ruling (b): the resource that refused
// changed result types (v2.40, rulings (a) and (b)); core Capacity { requested, available } is unchanged
pub type Error = … | Full(StoreCapacity) | …                     // every other case as §1.1 and the Limit gains above
pub type Outcome[R, A] = Committed(Done[R]) | Aborted(Undone[A]) | Failed(Causes) | Refused(StoreCapacity) | Stopped(core.OutOfTime)
pub type Served = Committed(Receipt) | Kept(Receipt) | Failed(Pending, Stop) | NoRoom(Pending, StoreCapacity)   // NoRoom: Resident or Allocation, never Journal
pub type ServeError = Before(Error, Option[Pending]) | After(Error)   // Some(p) only from conclude refused before publication
pub type Looked[R] = Seen(Seen[R]) | Failed(Causes) | Refused(StoreCapacity) | Stopped(core.OutOfTime)
pub type DeployError = … | FullNew(StoreCapacity)                  // §3.5a; its other cases unchanged
```

- `fork`'s signature is unchanged. A live-head fork returns after its FORK frame is synced, with a journal-mark ticket that is already durable, and is never refused by admission; the journal cap returns `Full(Journal(c))` and an allocator refusal during its preparation `Full(Allocation(c))`, before anything is sent (AP17).
- `resident_limit` bounds the logical account alone; frames handed to the journal worker are bounded by `FRAME_LIMIT` with backpressure, and the other journaling buffers by their own fixed bounds (AP15). `accounts(db).frames` reports F. The journal that no published cut covers is bounded by `journal_limit`: a frame that would pass it is refused at once with the cause `Journal(c)`, c.requested the attempted U + f (saturated at u64::MAX) and c.available `journal_limit`, through each producer's own result: `Refused(Journal(c))` from `run` and `run_on`, `Full(Journal(c))` from `admit` and a live-head `fork`, `Before(Full(Journal(c)), None)` from `serve` and `Before(Full(Journal(c)), Some(p))` from `conclude`. `accounts(db).journal` reports U (AP15).
- `Options` gains `journal_limit: u64`, default 268,435,456 and at least `checkpoint_bytes` + `FRAME_LIMIT` (AP15), and loses `checkpoint_task_bytes` with the checkpoint task (AP16). `checkpoint` and every cut-made operation return `Full(Allocation(c))` when the writer cannot allocate the cut, which loses nothing (AP16). Open's options step refuses an out-of-range or below-floor `journal_limit` with `Full(Journal(c))` before LOCK (AP15).
- `Limit` gains `Ids` (an id counter would pass its width, AP4) and `Manifest` (the MANIFEST size expression over branch and pin records would pass `MANIFEST_LIMIT`, AP4; the pack lists never cause it). `Full` carries `StoreCapacity` (below). Every other `Error` case is unchanged. `Unsupported(u64)` carries the format number for AP1, the retired kind for a retired frame, and the kind of the holding frame for a reserved field: 0x0021 for `residency` and `tree`, and 0x0031 for `base`.
- `ByteRows` keeps its built meaning: an owning, `send` cursor of Arc-backed rows.
- **`StoreCapacity` (v2.40, ruling (b)).** Every store refusal for capacity carries one typed cause, built where the comparison or allocation failed, never reconstructed later from `accounts`:
  - `Journal(c)`: the journal cap (AP15) and open's options step. c.requested is the attempted uncovered wire bytes, U + f, saturated at u64::MAX (or the floor), and c.available is J.
  - `Resident(c)`: an admission against `resident_limit` (AP15, §1.11), or an install's NEW `resident_limit` (T10). c.requested is the proposed logical charge, and c.available the ceiling.
  - `Allocation(c)`: a fallible allocation in the store: a Tx's index list, a cut's candidate, open's recovered state, a fork's preparation. c is the allocator's own report, its actual request and the room it had.

  The type is used in `Error.Full`, `Outcome.Refused`, `Looked.Refused`, `Served.NoRoom` and `DeployError.FullNew`. Core `Capacity { requested, available }` keeps its fields, and the core allocator keeps reporting it. `Stop.OutOfBytes(Capacity)` stays core: it is the body's own byte bound, not a store resource, and `conclude`'s `Failed(OutOfBytes(Room{…}))` keeps its fields. Neither the COMMIT format nor CALL replay identity changes: a cause is never written to a frame, and settle 2 means a resident admission refusal, as before.
- **Each operation keeps its own result shape (v2.40, ruling (a)).** `run` and `run_on` return `Refused(cause)`. A committing `admit` and a live-head `fork` return `Error(Full(cause))`. `serve` and `conclude` return `Error(Before(Full(cause), …))` before publication and `After(e)` after it (AP15's producer table). A `look` returns `Refused(cause)`. Cut-made operations return `Error(Full(cause))`, and install returns `FullNew(cause)`.
