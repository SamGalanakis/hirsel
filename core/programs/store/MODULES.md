# Hirsel Store Module Architecture (`store@1`, v3.2)

**Status:** Design only. No `.hirsel` source has moved. This is the normative cutover brief for `core/programs/store/`. v3.1 amends v3 (`2343476b`, `436a21b5`, with 3103's edits in `0585666d`) against astra's 3115 review; the disposition of its 11 findings, the placement scripts and the live-bug triage are in `hirsel-briefs/store-modules-amend-v31-3121/`. v3 revised v2 (`7d0cb44f`) against the 3107 review (REJECT). The store sources are unchanged since `ebcc4887`, and v3.1's facts are rechecked at `origin/main` `1e926f57` (line citations are at that commit); the §3.5 placement was recomputed at `3f6679cc`, which adds 11 private helpers. The `ebcc4887` tip includes the landed 3079 (`2d9e5e50`), 3082 (`f601b90a`) and 3080 (`979875cf`) lanes, the slot frame-limit change `1b1fc30f`, and the stream-quota change `10d42c7e`.
**Supersedes:** v3 (`store-modules-design-v3-3108`), v2 (`7d0cb44f`, `store-modules-design-v2-3097`) and v1 (`1f71060f`).
**Wholehog principle:** Each cutover lane lands its final module seam and owned state in one pass. The module graph stays acyclic at every landing. Every public declaration keeps its definition identity for the whole cutover. There are no compatibility aliases and no dual APIs.

The disposition table for every 3107 finding and lane row is in `hirsel-briefs/store-modules-design-v3-3108/DESIGN-v3.md`. That directory also holds the inventory scripts behind §3.4 and §7:
- `inventory.py <repo>` lists every declaration;
- `inventory_table.py` assigns every file and fails on an unowned file;
- `store_split.py` assigns every private `store.hirsel` declaration and fails on a gap or a duplicate.

---

## 1. Summary

`core/programs/store/hirsel.toml` lists **127** production sources:
- 125 are in module `"store"`;
- `store-health-types.hirsel` is in `"status"`;
- `main.hirsel` is in `"main"`.

Together they hold 3,020 top-level declarations. 573 of these are public: 570 in `"store"` (two of them the aliases `Health` and `StreamLatch`), 2 in `"status"` and 1 in `"main"`. Any subsystem can reach any other subsystem's private fields. `Db` (39 fields) and `DiskState` (34 fields) interleave Directory and Object state. `store.hirsel` (4,806 lines) mixes leaf types, codecs, holders, catalog, Directory I/O, transactions and transfer.

The v3 end state:

- **Public identity is untouched (§2).** Every public declaration in `"store"` and `"status"` stays declared in its module with its current header for the whole cutover. This covers types, functions and constants. Lower modules hold internal representations and never name a root type. Root public functions become adapters over those representations. No aliases are added. v2's Tier C aliases are deleted from the plan.
- **19 modules, 14 layers, acyclic at declaration level at every landing (§3).** v3's per-file owners were acyclic only by layer name: a body-level scan finds 441 upward or same-layer references, 110 references to root-held privates and 47 to root public functions in the code v3 moved (L1 alone: 100 upward references from 51 declarations). v3.1 replaces them with a computed placement (§3.5). In it, every moved declaration references only its own module, lower layers, `status`, public types (R2) and A-split bodies, and lands no earlier than everything it references. 3103's write-path owners are fixed inputs to that placement. The rules are:
  - public compound results stay at the root;
  - functions that need a live `Db` stay at the root (rule R3);
  - lower modules receive neutral inputs.
- **`Db`** drops to 13 fields and **`DiskState`** to 8 (§4). `ObjectWriterState` keeps all 22 of its existing fields, including `cache` (§4.5).
- **Recovery and capture keep their real branches (§5.3).** Live capture keeps three fence outcomes. Offline backup keeps the `LOCK` it acquired, moved into `PreparedCapture`. Writer recovery keeps its incomplete-segment case.
- **Claimant (§6).** A `HoldGuard` resource, admitted with its cleanup storage before the Hold PUT, owns the prospective token until the H request is irrevocably handed to the object lane or the hold is abandoned. On cancellation its finalizer issues the Abandon at the stream root. Only a strictly higher register incarnation releases a token without Abandon and enters the no-hold scan. Settle reports are bound to the store id and validated before any change; a negative report never unholds. A settle record is retired whenever its directive can no longer arrive (replacement, closed send, slot reap or reuse), so a later register GET can reissue it.
- **Lane 0 is folded into 3048 (§8).** 3048 is a hard predecessor of L1. 3075 lands before L1.

---

## 2. Public Identity Contract (Findings 1 and 3)

### 2.1 Why nothing public may move

Under `PROG-9` (`docs/spec/programs.md:252-299`), `code.identity(code.describe(f))` is `0x01` followed by `D A S`. Every type path in the generic arguments `A` and the header `S` is the definition identity of the nominal type, taken after alias expansion (`docs/spec/programs.md:277`). An application may describe any function whose arguments satisfy Runner's bounds. For example, `fn q(tx: store.Tx, h: H, take a: store.StreamId) -> Result[runner.Answer[()], Capacity]` passes `runner-query.hirsel:138-155`. The same holds for a nested argument such as `List[store.ExportStamp]` and for a generic instantiation at any public store type. Moving any of those declarations changes the key, even if a root alias spells the old name. A public function's own definition identity (`store@1/store/<name>`) appears wherever a client describes it. Hirsel has no re-export that preserves identity (`PROG-1`, `LANG-7`).

### 2.2 Rules

- **R1 — Root keeps every public declaration.**
  - Every `pub` declaration of module `"store"` stays declared in `"store"` for the whole cutover, with its current header. That is 570 declarations: 212 nominal types, 2 aliases, 344 functions and 12 constants.
  - This includes the 33 pinned types listed in 3107. It also includes `StreamId`, `StreamKey`, `ExportStamp`, `KeyedPolicy`, `KeyedCallRecord`, `ScrubRound`, `ScrubHistory`, `IndexState`, `IndexDrift`, `IndexReport`, `Attention`, `RowSchemas`, `PreparedTarget`, `CallRecord`, `DeclaredCollection`, `Binding`, `Generation`, `Image`, `Pass`, `Merge`, `Projection`, `Retain`, `Reclaim*`, `StoreLimits`, `StoreId`, `Caller`, `HostStreamId`, `StreamRefusal`, `TieredRefusal`, `EndpointRefusal`, `Position`, `Options` and every other public type.
  - `Health` and `StreamLatch` stay in `"status"`. Their existing root aliases stay as they are.
  - The full per-file list is §3.4, and `store.hirsel`'s list is §7.1.
  - A file that holds public declarations is split **by declaration**. The public declarations move to a root file named `store-api-<stem>.hirsel` in `"store"`. The private declarations keep the original file name, and the file is reassigned to the owning module. A definition's identity is its module path and name, not its file, so this move changes no identity.
- **R2 — Lower modules never name a root type.**
  - Each lower module defines the internal representation it needs. Examples:
    - `types.Failure` for `Error`, with variant payloads `types.FaultRep`, `types.CapacityRep` and so on;
    - `txn.TxState` behind `Tx`;
    - `read.ReaderState` behind `Reader`;
    - `capture.SharedState` behind `SharedCapture`.
  - The root converts at the boundary.
  - The public `Error` keeps its exact variants and its root payload types (`Fault`, `StoreCapacity`, `Limit`, `Position`, `EndpointRefusal`, `HostStreamId`, `StreamRefusal`, `TieredRefusal`, `StreamLatch`, `StoreLimits`). v2's claim that `Error` may carry `types.*` payloads is withdrawn. Clients match on those payloads and pass them back to root operations.
  - Public compound results (`Outcome`, `Done`, `Undone`, `Page`, `Resume`, `ServeError`, `Served`, `Pending`, `Parked`, `Exported`, `Recorded` and the rest) stay at the root. They are built there from internal outcomes.
- **R3 — Live-`Db` orchestration stays at the root.**
  - A private function whose header takes `Db`, `DiskState` or `Preparation` stays in `"store"` at its file's landing. The exception is a function that its lane row (§10) rewrites over named neutral inputs.
  - The signature scan behind this rule is the "Root-held" column of §3.4.
  - A function whose header takes `Tx`, `Declare`, `Migration` or another root handle moves down only after it is rewritten over that handle's internal state (`txn.TxState`, `catalog.DeclareState` and so on). It stays at the root if it invokes a root-typed callback.
- **R4 — Public contracts, not only names, are preserved.**
  - `Reader` stays `pub opaque resource type Reader: send` with its finalizer (`store-read.hirsel:16`). Its constructor stays fallible (`Error(Full(Allocation))`), and the reader grant stays inside it: `{ inner: read.ReaderState }`, where `ReaderState` owns the `ReaderPool` grant.
  - `ReadOnly` stays `pub opaque type ReadOnly: shareable + send` (`store-read.hirsel:759`).
  - `SharedCapture` keeps `shareable + send + cross` and its capture identity.
  - `RowView` keeps its `in row` anchors.
  - `range`, `fetch_bytes` and the other public functions with anchored results keep their `in tx` / `in row` results at the root.
  - The `Fold` wrapper invokes the application's `StaticFn[fn(mut Tx, E, Delta[K, V]) -> Result[(), A]]` (`store-projector.hirsel:108-113`) with the exact root `Tx`, `Delta`, `Merge` and `Limits` values. Maintenance owns the walker and the pure fold bookkeeping, and gives the root driver neutral delta material, which the root turns into `Delta`/`Merge` before the call.

```hirsel unbuilt
// Root wrappers keep the declared kind, bounds and finalizer; only fields change.
pub opaque resource type Reader: send { inner: read.ReaderState } drop(owned_reader) {}
pub opaque type ReadOnly: shareable + send { inner: read.ReadOnlyState }
pub opaque scoped type Tx { inner: txn.TxState }
pub opaque type Pending { inner: txn.PendingRep }
pub type Served = Committed(Receipt) | Kept(Receipt) | Failed(Pending, Stop) | NoRoom(Pending, StoreCapacity) | Parked(Parked)
```

### 2.3 `code.revision` is separate

`code.revision(s)` hashes the checked IR reached from `s` (`docs/spec/programs.md:304-325`, `store-declare.hirsel:336`). When a root public function becomes an adapter, its body changes. So does the revision of any versioned extractor or `StaticFn` that calls it, which means `Migrate` rebuilds that index and `Open` reports `Changed`. Pure extractors that call no `store.*` function keep both identity and revision. Each lane that turns a function reachable from an index extractor into an adapter records the revision change in its ticket. Identity must not change at all.

### 2.4 Identity oracle (O1)

Pre-split keys are frozen as literal bytes from the pre-L1 tip, registered in the wire fixture and resolved through Runner (§9.2 O1). Every new key is a function the fixture actually registers in `place()` (`core/programs/wire/admission.hirsel:77-84`), so the pre-split build places it:

1. **Existing (keep):** `tools/tests/wire_admission.py:55-62` sends literal transaction and query identities that name `store@1/store/Tx` and `store@1/store/Caller`, each with its real argument and result fingerprints, and checks placement (`:181-187`). The dev client at `:526-537` subscribes with a literal root-`Tx` key. `core/programs/runner/steps_laws.hirsel:441-460` replays a literal old callback identity.
2. **New — plain query with a StreamId argument:** registered with `runner.query` as `stream_q(store@1/store/Tx,wire@1/admission/H,take store@1/store/StreamId)->core@1/core/Result[runner@1/runner/Answer[()],core@1/core/Capacity]`, sent with the argument fingerprint of `StreamId`'s codec and `READ_RESULT_FP`.
3. **New — caller-aware query with a nested nominal argument:** registered with `runner.query_as`, whose callback type is `fn(store.Tx, H, Caller, take A)` (`runner-query.hirsel:142`): `stamps_q(store@1/store/Tx,wire@1/admission/H,store@1/store/Caller,take core@1/core/List[store@1/store/ExportStamp])->core@1/core/Result[runner@1/runner/Answer[()],core@1/core/Capacity]`. Runner takes the identity of that exact descriptor (`runner-query.hirsel:154-158`); v3's header without `Caller` named a function nothing registers.
4. **New — schema and selector descriptor bytes:** `schema` and the slot selector are not Hello entries. `schema` is passed to `launch_server`/`place_head` (`admission.hirsel:196,237`) and the selector is `query_table(H) -> Table[u64,u32]`, bound through `runner.table_slot("data", code.describe(query_table))` (`:57-63`). O1 freezes their full descriptor bytes, `schema(mut store@1/store/Declare)->core@1/core/Result[wire@1/admission/H,store@1/store/Error]` and `query_table(wire@1/admission/H)->store@1/store/Table[u64,u32]`, and compares them at the launch and slot-binding fixtures, not through Hello placement.

The fingerprints in items 2 and 3 are computed from the codecs at the pre-L1 tip and pasted as literals, the way `ENTRIES` holds them today. They are the oracle; a later lane may not regenerate them.

---

## 3. Acyclic Module Map (Finding 3)

### 3.1 Naming and visibility

- No module name equals a lowercase `pub` function or constant in `"store"` (`LANG-7`), or a `core`/`std` module.
- `PROG-2` has no package-private level. Any `pub` declaration in a lower module is reachable by a dependent as `store.<module>.X` (`LANG-7`, e.g. `store.wal.append`).
- Inter-module operations are therefore `pub` functions over internal state. Each invariant-bearing state type is `pub opaque` in its owning module, so its fields and constructors stay hidden (`PROG-2`).
- Constructors that would forge an internal state are not exported. O4 (§9.2) checks this with named refusal diagnostics.

### 3.2 Layers

Every edge goes to a lower layer, except `types -> status` within Layer 0. No lower module names a declaration of `"store"` (R2). The layer list alone does not make a plan acyclic; §3.5 holds the declaration-level placement that does.

```text
Layer 0:   status   types        (health/stream status; internal leaf reps: Failure, FaultRep, CapacityRep, limits checks)
Layer 1:   format                (frame/journal/stream/XINDEX/tiered codecs over internal reps)
Layer 2:   pages                 (PageTree, PageRoots, HolderTable, DataRows, pure appliers over resolved roots)
Layer 3:   catalog   manifest    (pure SchemaPlan + handle states; pack/manifest/location/scrub data over neutral inputs)
Layer 4:   indexes   pager       (registry + pure planning; tiered pager, cache, park records)
Layer 5:   journal               (Directory queue/worker/acks, recovery, JournalReplay assembly, capture fences)
Layer 6:   capture   history     (live/locked capture + SharedState; objects@1 history, register, pack cache)
Layer 7:   host_stream           (claim with HoldGuard, register lane, holds, service, settle over a session view)
Layer 8:   object_writer         (ObjectWriterState, object queue, slot relay from L9, reclaim, levels, followers)
Layer 9:   read                  (ReaderState, ReadOnlyState, cursors, changes, visitors over neutral inputs)
Layer 10:  txn                   (TxState, PendingRep, accounting, call streams, commit drafting over passed limits)
Layer 11:  transfer  maintenance (copy/lease/import state; walkers, pure fold work, expiry)
Layer 12:  store                 (all 570 public declarations, Db/DiskState, coordinators, R3 orchestration, adapters)
Layer 13:  main
```

### 3.3 Cycle resolution

| Conflict | Source | v3 division |
|---|---|---|
| Public results vs Layer 0 | `ServeError`/`Served` carry root `Pending`; `Served.Parked` carries `Parked`; `Page.next: Option[Resume]` (`store.hirsel:393,403,852,863`, `store-mux.hirsel:30`) | All stay at the root (R1). `txn` returns `txn.Outcome`/`txn.PendingRep`; the root wraps them. |
| `keyed_time(db)` in format | `store-stream-key.hirsel:57` reads the clock and main head | `keyed_time` is public and stays at the root. `format` takes the private `parse_stream_key` only. |
| `pages.read_tail` | takes root `Tx`, a projector declaration and a typed table (`store-tail.hirsel:93`). `ProposedMain` carries `HistoryActivation` (`store.hirsel:143`, `store-object-cut.hirsel:22`) | `read_tail`, `key_codec` and `Tail` are public and stay at the root. `pages` keeps `TailBase`/`TailBases` and functions over resolved roots. `ProposedMain` stays at the root (§7.2). |
| `catalog` vs registrations | `Declare.projectors`/`StagedMigration.projectors` hold `DeclaredProjector`; `SchemaPlan.bindings` holds `IndexRegistration` (`store-declare.hirsel:12-27`, `store-migration.hirsel:40-55`, `store-activation.hirsel:1-10`) | Root `Declare`/`StagedMigration` own live registrations next to `inner: catalog.DeclareState`. `catalog` produces a pure `catalog.SchemaPlan` (collections, ids, fences), a new type built by the L3 rewrite. The root pairs it with registrations. Today's private `SchemaPlan`, which holds the `IndexRegistration` bindings, lands in `indexes` (L10, §3.5). |
| `manifest` orchestration | `fill_cut_manifest(.., db: Db, ..)`, `raise_materialized_pin(.., TieredCutStage)`, `begin_scrub(mut db)` (`store-cut-participants.hirsel:117,175`, `store-scrub.hirsel:54`) | `begin_scrub` (public) and `disk_cut_state` stay at the root. `fill_cut_manifest` and `cut_participants` are rewritten over a `manifest.CutInput` (heads, pins, lease spans) that the root builds. `raise_materialized_pin` stays at the root (its body names root cut state; §3.5). |
| `pager` vs schema and park | `publish_tiered_residency(mut db)` calls `schema_plan(db)`; the park queue is in `store-mux.hirsel:5-27` | Park records go to `pager` at L5. `publish_tiered_residency` and the other residency functions that take `Db` stay at the root (R3). |
| Common journal message | the queue carries Object session, export and slot payloads (`store-journal-worker.hirsel:1,268`) | L6 introduces a Directory-only `journal.DirJournalMsg`. Until L9, the root owns an explicit interim `JournalSender = Dir(Sender[journal.DirJournalMsg]) \| Object(Sender[JournalMsg])` and picks the sender per backend. |
| Slot relay | `slot_relay` owns `Sender[JournalMsg]` and builds `StreamAnswer` (`store-stream-slot.hirsel:414-425`) | At L8 the relay and `SlotRelayInput` move to the root beside the current worker. At L9 they move with the object queue to `object_writer`. |
| `reclaim_preview` / `open_object_writer` | they call root durability, pause via a live `Db`, and build and checkpoint the root `Db` (`store-reclaim.hirsel:526-542`, `store-object-writer.hirsel:252,315-318`) | Both stay at the root (R3). `object_writer` receives owned state and inputs. |
| Tx-bound index reads | `registered_index` borrows from `Tx`; `probe_index_keys` and `totals` read `Tx` (`store-index-registry.hirsel:138,187`, `store-aggregate.hirsel:109`) | L10 extracts the registry and the pure functions. The Tx adapters stay with `Tx` and are rewritten over `txn.TxState` at L12. |
| `staged_reader(Preparation)` | is public in `store-read.hirsel:59` and reads `source.db` (`store-export.hirsel:607`) | `read` takes a neutral `read.ReaderInput` (roots, catalog, limits, pool). `staged_reader` stays at the root. |
| Commit path | `available_journal(db,..)`; `settle_draft` calls root `cut_install` (`store-keyed-journal.hirsel:172-182`, `store-commit.hirsel:286,326`) | `txn` takes limits and an accounting snapshot as arguments. `settle_draft` returns `txn.CutDecision` (`None \| PreCut \| Reseal`) to the root coordinator, which runs the cut and the handoff. |
| `Preparation` | it owns a full `Db`; `begin_copy` calls checkpoint and durable; activation installs projectors (`store-export.hirsel:391-412`, `store-activation.hirsel:203-229`) | The prepared `Db` and the publication and install orchestration stay at the root. `transfer` owns copy and lease state. |
| Verify and repair | `verify_release` invokes a root-typed declaration on a `Db`; `verify_step` takes the `Db` (`store-inspect.hirsel:97-124`) | Both are public and stay at the root. `maintenance` owns `IndexWalk` and the pure fold and walk steps. |
| Pack cache | `ObjectsCache` and `history_read_pack` (`store-object-cache.hirsel:2,284`) are used by history; `objects_cache` builds the public authority option | The `ObjectsCache` type goes to `pager` (L5), which stores it; cache I/O goes to `history` (L7), and scan, trim, write and `history_read_pack` to `object_writer` (L9) beside the pager worker loop (§3.5). Public `objects_cache` stays at the root. |
| Offline backup | `prepare_backup` / `backup_offline` / `backup_with_writer` (`store-backup-offline.hirsel:15-64`) | All five public declarations stay at the root. `backup_lock` and `copy_backup_journal` go to `capture` at L7. `backup_with_writer` keeps opening the writer at the root. |

### 3.4 Per-file inventory (all 127 production files, by declaration)

Rules for reading the table:
- **Private declarations go to** names the module that receives the file's private declarations, except those listed in the last two columns. A file in `"store"` keeps everything.
- **Public declarations kept in `"store"`** move to `store-api-<stem>.hirsel` when the file is reassigned (R1).
- **Root-held** lists private functions whose headers take `Db`, `DiskState` or `Preparation`. They stay at the root under R3 unless the lane row names their neutral inputs.
- The `store.hirsel` row is expanded in §7.
- **v3.1:** this table and §7.2 give each file's default owner. §3.5 overrides them declaration by declaration; where they disagree, §3.5 wins.
- The "Root-held" column is a header-signature scan (`inventory_table.py`: single-line `fn` headers naming `Db`, `DiskState` or `Preparation`; no store header wraps). A body-level use of a root type in a moved function is handled by the R2 rewrite its lane performs, not by this column.

| # | File | Decls (pub/priv) | Private declarations go to | Public declarations kept in `"store"` (root adapters) | Root-held Db-bound privates (R3) | Other private exceptions |
|---|---|---|---|---|---|---|
| 1 | `store.hirsel` | 108/269 | §7.2 (by declaration) | 108 declarations; see §7.1 | see §7.2 | — |
| 2 | `store-limits.hirsel` | 5/13 | `types` (L1) | `FRAME_LIMIT`, `VALUE_LIMIT`, `StoreLimits`, `default_limits`, `store_limits` | — | `replay_within_limits` → format |
| 3 | `store-stream-key.hirsel` | 9/1 | `format` (L1) | `StreamId`, `StreamKey`, `SUBJECT_MAX`, `STREAM_RETENTION_MS`, `STREAM_FUTURE_SLACK_MS`, `caller_key`, `stream_key_bytes`, `stream_record_matches`, `keyed_time` | — | — |
| 4 | `store-keyed-journal.hirsel` | 0/16 | `format` (L1) | — | — | `materialize_keyed_edits` → pages, `keyed_lookup` → pages, `keyed_draft_live` → pages, `resident_with_credit` → txn, `available_resident` → txn, `available_journal` → txn, `draft_credit` → txn |
| 5 | `store-keyed-claims.hirsel` | 15/39 | `txn` (L12) | `CLAIM_DONE_MAX`, `KEYED_OP_RECORD_MAX`, `ClaimKey`, `KeyedPolicy`, `ClaimAdmission`, `ClaimCompletion`, `claim_ticket`, `claim_key_bytes`, `claim_reply_expiry`, `check_keyed_policy`, `keyed_policy`, `admit_claim`, `conclude_claim`, `KeyedCallRecord`, `keyed_completion` | `validate_claim_completion`, `rebuild_keyed_credit`, `keyed_completion_credit`, `check_keyed_completion` | — |
| 6 | `store-keyed-lifecycle.hirsel` | 15/4 | `txn` (L12) | `OpenClaimCursor`, `open_claim_cursor`, `next_open_claim`, `ClaimRecovery`, `recovery_policy`, `claim_clock`, `recover_claim`, `has_keyed_ops`, `ClaimExpiry`, `DroppedClaimOp`, `check_dropped_claim_ops`, `ClaimSweep`, `claim_sweep_start`, `ClaimSweepWork`, `sweep_claims` | `check_keyed_open` | — |
| 7 | `store-streams.hirsel` | 11/40 | `txn` (L12) | `reclaim_streams`, `AnonymousCursor`, `anonymous_cursor`, `reclaim_startup_streams`, `sweep_streams`, `Completion`, `ticket`, `Admission`, `release_stream_admit`, `admit`, `admit_while` | `stream_control`, `admit_stream`, `stream_changes`, `reclaim_front`, `stream_admit_held`, `admit_existing`, `admit_next`, `admit_commit`, `stream_live` | — |
| 8 | `store-attempts.hirsel` | 1/14 | `txn` (L12) | `look` | `staged_until_decided`, `call_pin_root`, `deliver_call_miss`, `draft_call`, `metered_draft` | — |
| 9 | `store-commit.hirsel` | 0/19 | `txn` (L12) | — | `prepare_edited_roots`, `prepare_direct_edit`, `direct_data_edits`, `prepare_run_commit`, `reserve_tiered_freed`, `commit_prepared`, `draft_row_account`, `settle_draft`, `settle_admit`, `frame_draft`, `over_bound` | — |
| 10 | `store-commit-wire.hirsel` | 0/14 | `format` (L1) | — | — | `DataOrder` → txn, `data_order` → txn, `ordered_row` → txn, `stored_prior` → txn |
| 11 | `store-mux.hirsel` | 6/17 | `txn` (L12) | `Parked`, `park_id`, `park_notices`, `poll_parked`, `cancel_parked`, `resume_parked` | `attach_tiered_pager`, `clear_parked`, `revoke_parked`, `send_parked_demand`, `install_parked`, `call_page_source`, `drive_served`, `decide_served` | `ParkEntry` → pager, `ParkQueue` → pager, `empty_park_queue` → pager, `park_slot` → pager, `revoke_entry` → pager, `first_unsent` → pager, `parked_in_flight` → pager |
| 12 | `sizes.hirsel` | 26/48 | `store` (unchanged) | `Kind`, `Options`, `Overrides`, `Shortage`, `ROW_KEY_LIMIT`, `small_place_limits`, `WriterPlan`, `derived_sizes`, `sized`, `sized_with`, `directory_writer_need`, `PlacePlan`, `planned`, `planned_options`, `planned_bytes`, `planned_sim_bytes`, `planned_capture`, `writer_plan`, `writer_bytes`, `FollowerPlan`, `follower_plan`, `plan_bytes`, `service_bytes`, `follower_starter_bytes`, `sim_level_reservation`, `resident_row_bytes` | — | — |
| 13 | `store-journal-worker.hirsel` | 0/20 | `journal` (L6) | — | — | — |
| 14 | `store-journal-status.hirsel` | 1/6 | `journal` (L6) | `close` | `journal_receive`, `journal_ack`, `journal_poll`, `journal_poll_groups`, `close_ack` | — |
| 15 | `store-journal-recover.hirsel` | 0/23 | `journal` (L6) | — | — | — |
| 16 | `store-capture.hirsel` | 7/28 | `capture` (L7) | `Purpose`, `Part`, `Need`, `BOOTSTRAP`, `PreparedCapture`, `capture_need`, `prepare` | — | — |
| 17 | `store-capture-check.hirsel` | 3/22 | `capture` (L7) | `CheckTotals`, `CheckError`, `check_capture` | — | — |
| 18 | `store-backup-offline.hirsel` | 5/2 | `capture` (L7) | `BackupError`, `PreparedBackup`, `prepare_backup`, `backup_offline`, `backup_with_writer` | — | — |
| 19 | `store-capture-check-journal.hirsel` | 0/11 | `capture` (L7) | — | — | — |
| 20 | `store-capture-journal.hirsel` | 0/12 | `capture` (L7) | — | — | — |
| 21 | `store-capture-pages.hirsel` | 0/15 | `capture` (L7) | — | — | — |
| 22 | `store-capture-catalog.hirsel` | 0/11 | `capture` (L7) | — | — | — |
| 23 | `store-capture-describe.hirsel` | 4/15 | `capture` (L7) | `DescribeEntry`, `DescribeTotals`, `DescribeLine`, `describe_capture` | — | — |
| 24 | `store-pages.hirsel` | 0/51 | `pages` (L2) | — | — | — |
| 25 | `store-changes.hirsel` | 4/14 | `read` (L11) | `Trimmed`, `Changes`, `ChangePage`, `changes_page` | — | — |
| 26 | `store-tiered.hirsel` | 0/15 | `pager` (L5) | — | — | — |
| 27 | `store-tiered-frames.hirsel` | 0/7 | `format` (L1) | — | — | — |
| 28 | `store-tiered-packs.hirsel` | 0/6 | `pager` (L5) | — | — | — |
| 29 | `store-tiered-ranges.hirsel` | 0/16 | `pager` (L5) | — | — | — |
| 30 | `store-tiered-cache.hirsel` | 1/22 | `pager` (L5) | `READER_PIN_MIN` | — | — |
| 31 | `store-tiered-pager.hirsel` | 0/32 | `pager` (L5) | — | — | — |
| 32 | `store-tiered-pager-batch.hirsel` | 0/8 | `pager` (L5) | — | — | — |
| 33 | `store-tiered-attempt.hirsel` | 0/51 | `pager` (L5) | — | — | — |
| 34 | `store-tiered-plan.hirsel` | 0/11 | `pager` (L5) | — | — | — |
| 35 | `store-tiered-point-read.hirsel` | 0/13 | `pager` (L5) | — | — | — |
| 36 | `store-tiered-count.hirsel` | 0/3 | `pager` (L5) | — | — | — |
| 37 | `store-tiered-scan.hirsel` | 0/9 | `pager` (L5) | — | — | — |
| 38 | `store-tiered-span.hirsel` | 0/6 | `pager` (L5) | — | — | — |
| 39 | `store-tiered-tx-read.hirsel` | 2/10 | `txn` (L12) | `TxIndexTiered`, `fetch_many` | — | — |
| 40 | `store-tiered-initial-changes.hirsel` | 0/2 | `pager` (L5) | — | — | — |
| 41 | `store-tiered-changes.hirsel` | 0/24 | `pager` (L5) | — | — | — |
| 42 | `store-tiered-pins.hirsel` | 0/8 | `pager` (L5) | — | — | — |
| 43 | `store-tiered-overlay.hirsel` | 0/12 | `pager` (L5) | — | — | — |
| 44 | `store-tiered-replay.hirsel` | 0/11 | `pager` (L5) | — | — | — |
| 45 | `store-tiered-fold.hirsel` | 0/28 | `pager` (L5) | — | — | — |
| 46 | `store-tiered-emission.hirsel` | 0/13 | `pager` (L5) | — | — | — |
| 47 | `store-tiered-publish.hirsel` | 0/17 | `pager` (L5) | — | `stage_tiered_cut`, `drain_cut`, `install_tiered_holders` | — |
| 48 | `store-cut-participants.hirsel` | 0/8 | `manifest` (L4) | — | `disk_cut_state` | — |
| 49 | `store-fork.hirsel` | 1/3 | `txn` (L12) | `fork` | `plan_fork`, `fork_live` | — |
| 50 | `store-tiered-probe.hirsel` | 0/5 | `pager` (L5) | — | — | — |
| 51 | `store-tiered-probe-index.hirsel` | 0/7 | `pager` (L5) | — | — | — |
| 52 | `store-tiered-promote.hirsel` | 0/19 | `pager` (L5) | — | `promote_cut_holder` | — |
| 53 | `store-tiered-compact.hirsel` | 0/22 | `pager` (L5) | — | `compact_cut` | — |
| 54 | `store-tiered-frees.hirsel` | 0/16 | `manifest` (L4) | — | — | — |
| 55 | `store-tiered-lineage.hirsel` | 0/31 | `manifest` (L4) | — | — | — |
| 56 | `store-tiered-residency.hirsel` | 2/15 | `pager` (L5) | `Residency`, `set_residency` | `admit_tiered_plan`, `start_residency_pager`, `stop_residency_pager`, `residency_to_tiered`, `publish_tiered_residency`, `residency_to_memory` | — |
| 57 | `store-manifest.hirsel` | 0/62 | `manifest` (L4) | — | — | — |
| 58 | `store-list-read.hirsel` | 0/12 | `manifest` (L4) | — | — | — |
| 59 | `store-manifest-wire.hirsel` | 0/29 | `manifest` (L4) | — | — | — |
| 60 | `store-page-recovery.hirsel` | 0/5 | `manifest` (L4) | — | — | — |
| 61 | `store-locations.hirsel` | 0/40 | `manifest` (L4) | — | — | — |
| 62 | `store-packs.hirsel` | 0/33 | `manifest` (L4) | — | — | — |
| 63 | `store-pack-index.hirsel` | 0/16 | `manifest` (L4) | — | — | — |
| 64 | `store-scrub.hirsel` | 4/13 | `manifest` (L4) | `Scrub`, `ScrubReport`, `begin_scrub`, `scrub_step` | — | — |
| 65 | `store-scrub-pace.hirsel` | 13/10 | `manifest` (L4) | `ScrubRound`, `ScrubHistory`, `ScrubConfig`, `ScrubStatus`, `ScrubStepResult`, `ScrubPacer`, `scrub_history`, `last_complete_scrub_round`, `record_scrub_round`, `begin_scrub_pacer`, `scrub_pacer_status`, `scrub_pacer_step`, `scrub_pacer_stop` | — | — |
| 66 | `store-cut-retirement.hirsel` | 0/1 | `manifest` (L4) | — | — | — |
| 67 | `store-history.hirsel` | 0/31 | `history` (L7) | — | — | — |
| 68 | `store-stream-frames.hirsel` | 0/54 | `format` (L1) | — | — | — |
| 69 | `store-stream-types.hirsel` | 6/0 | `store` (unchanged) | `HostStreamId`, `StreamField`, `StreamRefusal`, `StreamLatch`, `stream_refusal_text`, `stream_latch_text` | — | — |
| 70 | `store-stream-service.hirsel` | 0/32 | `host_stream` (L8) | — | — | — |
| 71 | `store-stream-slot.hirsel` | 12/32 | `host_stream` (L8) | `Batching`, `StreamLimits`, `StreamPlan`, `StreamRoot`, `JournalSlot`, `JournalService`, `stream_service_bytes`, `journal_service`, `journal_slot`, `streamed`, `stream_reader`, `close_journal_service` | — | `slot_relay` → store (L8), object_writer (L9), `SlotRelayInput` → store (L8), object_writer (L9) |
| 72 | `store-stream-task.hirsel` | 0/31 | `host_stream` (L8) | — | — | — |
| 73 | `store-stream-poll.hirsel` | 0/17 | `host_stream` (L8) | — | — | — |
| 74 | `store-stream-claim.hirsel` | 0/21 | `host_stream` (L8) | — | — | — |
| 75 | `store-stream-settle.hirsel` | 0/6 | `host_stream` (L8) | — | — | — |
| 76 | `store-stream-gc.hirsel` | 0/28 | `host_stream` (L8) | — | — | `XIndexEntry` → format, `XIndex` → format, `stream_xindex_encode` → format, `stream_xindex_decode` → format |
| 77 | `store-stream-janitor.hirsel` | 0/8 | `host_stream` (L8) | — | — | — |
| 78 | `store-stream-follow.hirsel` | 0/9 | `host_stream` (L8) | — | — | — |
| 79 | `store-stream-archive.hirsel` | 0/12 | `host_stream` (L8) | — | — | — |
| 80 | `store-history-read.hirsel` | 0/19 | `history` (L7) | — | — | — |
| 81 | `store-history-witness.hirsel` | 0/2 | `history` (L7) | — | — | — |
| 82 | `store-history-packs.hirsel` | 0/31 | `history` (L7) | — | — | — |
| 83 | `store-reclaim.hirsel` | 10/29 | `object_writer` (L9) | `RECLAIM_FORMAT_VERSION`, `RECLAIM_TOOL_VERSION`, `ReclaimKind`, `ReclaimItem`, `ReclaimStatus`, `ReclaimList`, `PurgeReport`, `ReclaimError`, `reclaim_preview`, `reclaim_purge` | `record_object_lane_failure` | — |
| 84 | `store-register.hirsel` | 0/29 | `history` (L7) | — | — | — |
| 85 | `store-object-authority.hirsel` | 2/3 | `history` (L7) | `EndpointRefusal`, `endpoint_refusal_text` | — | — |
| 86 | `store-object-cache.hirsel` | 1/17 | `history` (L7) | `objects_cache` | — | — |
| 87 | `store-object-open.hirsel` | 0/14 | `object_writer` (L9) | — | — | — |
| 88 | `store-object-import.hirsel` | 1/1 | `transfer` (L13) | `import_authority` | `history_import_base` | — |
| 89 | `store-object-export.hirsel` | 0/10 | `transfer` (L13) | — | `begin_object_export_copy`, `object_export_capture` | — |
| 90 | `store-export.hirsel` | 17/34 | `transfer` (L13) | `ExportStamp`, `Exported`, `ExportCopy`, `DeployError`, `export_copy`, `begin_export`, `begin_export_copy`, `begin_backup`, `export_leased`, `release_export`, `ExportTail`, `ExportRound`, `advance_export`, `copy_export_tail`, `Preparation`, `open_export`, `restore` | `leased_step_base`, `record_leased_step`, `begin_copy`, `open_copied` | — |
| 91 | `store-prepared-check.hirsel` | 0/2 | `transfer` (L13) | — | — | — |
| 92 | `store-catchup.hirsel` | 1/7 | `transfer` (L13) | `catch_up_export` | — | — |
| 93 | `store-rounds.hirsel` | 5/32 | `transfer` (L13) | `KeptImport`, `ValidatedRound`, `keep_import`, `validate_round`, `apply_round` | `import_pin_slot`, `orphan_import_pin`, `hold_import`, `install_account` | — |
| 94 | `store-install.hirsel` | 3/4 | `transfer` (L13) | `cutover_settled`, `perform_cutover`, `perform_bound_cutover` | `accept_cutover`, `install_prepared`, `perform_cutover_with`, `forget_import` | — |
| 95 | `store-object-cut.hirsel` | 0/20 | `history` (L7) | — | — | — |
| 96 | `store-activation.hirsel` | 11/17 | `transfer` (L13) | `StagedMigration`, `staged_install_snapshot`, `stage_rewrite`, `stage_activation`, `activation_install_allowed`, `install_activation`, `install_bound_activation`, `prepared_rewrites`, `ImportSettlement`, `import_settlement`, `import_migration` | `staged_migration`, `install_activation_layout`, `publish_schema_plan`, `publish_schema_plan_binding`, `validate_rewrite_base` | — |
| 97 | `store-object-journal.hirsel` | 0/16 | `object_writer` (L9) | — | — | — |
| 98 | `store-sim-levels.hirsel` | 11/20 | `object_writer` (L9) | `SimLevels`, `LevelWorld`, `LevelSync`, `LevelUntil`, `LevelHold`, `LevelKind`, `LevelEvent`, `level_world`, `level_sync`, `level_mark`, `level_log` | — | — |
| 99 | `store-object-writer.hirsel` | 5/21 | `object_writer` (L9) | `Refusal`, `open_authority`, `claim_of`, `archive`, `fresh_snapshot` | `log_directory`, `object_writer_healthy`, `object_writer_open`, `object_state`, `object_cut_due`, `open_object_writer`, `object_stream_close`, `object_cut_close`, `take_object_journal`, `take_object_journal_with`, `object_cut_state`, `object_cut_work` | — |
| 100 | `store-levels.hirsel` | 6/3 | `object_writer` (L9) | `Level`, `levels`, `level`, `durable_at`, `Position`, `position` | `store_level_name`, `weak_covered`, `durable_weak` | — |
| 101 | `store-health.hirsel` | 3/10 | `object_writer` (L9) | `Health`, `health`, `on_unhealthy` | `reclaim_state` | — |
| 102 | `store-attention.hirsel` | 7/6 | `store` (unchanged) | `Accounts`, `accounts`, `IndexDrift`, `IndexReport`, `Attention`, `attention`, `attention_mark` | — | — |
| 103 | `store-health-types.hirsel` | 2/0 | `status` (unchanged) | `Health`, `StreamLatch` | — | — |
| 104 | `store-followers.hirsel` | 13/30 | `object_writer` (L9) | `Authority`, `log_authority`, `Followers`, `followers`, `Follower`, `follower`, `follower_branch`, `snapshot_follower`, `applied`, `catch_up`, `fresh`, `at_least`, `follower_floor` | — | — |
| 105 | `store-follower-service.hirsel` | 7/17 | `object_writer` (L9) | `FollowerService`, `FollowerOwner`, `FollowerSlot`, `follower_service`, `follower_owner`, `close_follower_service`, `follower_slot` | — | — |
| 106 | `store-object-follower.hirsel` | 5/10 | `object_writer` (L9) | `object_authority`, `sim_authority`, `Claim`, `HeadView`, `follower_head` | — | — |
| 107 | `store-layout.hirsel` | 1/2 | `pages` (L2) | `fixture_broken_handoff` | — | — |
| 108 | `store-reexec.hirsel` | 13/14 | `txn` (L12) | `Difference`, `UnknownReason`, `Verdict`, `ReplaySource`, `reexecute`, `prepare_reexecute`, `declare_replay`, `replay_record`, `reexecute_with`, `ReplayProjection`, `project_recorded`, `RecordedClaim`, `recorded_claim` | `recorded_commit`, `base_roots`, `rerun_attempt`, `rerun`, `replay_source`, `replay_run` | — |
| 109 | `store-tx.hirsel` | 4/39 | `txn` (L12) | `rollback_writes_on_error`, `rollback_writes_on_overrun`, `pop_first`, `pop_last` | — | — |
| 110 | `store-tiered-refusals.hirsel` | 2/6 | `types` (L1) | `TieredRefusal`, `tiered_refusal_text` | `store_has_tiered`, `require_tiered_storage`, `require_archive_authority` | — |
| 111 | `store-index-registry.hirsel` | 1/56 | `indexes` (L10) | `IndexState` | — | — |
| 112 | `store-index-build.hirsel` | 2/21 | `maintenance` (L14) | `index_build_step`, `rebuild_index` | `tiered_rows_after`, `tiered_rows_after_mode`, `building_index`, `demote_drifted` | — |
| 113 | `store-aggregate.hirsel` | 5/10 | `indexes` (L10) | `Aggregate`, `Totals`, `aggregate`, `totals`, `rebuild_aggregate` | — | — |
| 114 | `store-verify.hirsel` | 11/18 | `maintenance` (L14) | `WalkFault`, `WalkFinding`, `WalkReport`, `WalkMode`, `IndexWalk`, `begin_verify`, `WalkCoverage`, `IndexCoverage`, `walk_coverage`, `verify_step`, `allow_unverified_teardown` | `walk_index`, `verify_step_read`, `walk_sums`, `walk_sum_entries`, `walk_rows`, `walk_entries`, `teardown_hook`, `teardown_check_bucket`, `verify_teardown` | — |
| 115 | `store-expiry.hirsel` | 3/7 | `maintenance` (L14) | `expire_rows`, `Swept`, `sweep_expired` | `due_rows` | — |
| 116 | `store-inspect.hirsel` | 8/2 | `capture` (L7) | `EntryForm`, `CatalogEntry`, `snapshot_identity`, `catalog_entries`, `VerifyOutcome`, `verify_release`, `RepairOutcome`, `repair_indexes` | — | `OPERATOR_ROWS` → maintenance, `OPERATOR_SCRUB_BYTES` → maintenance |
| 117 | `store-handle-check.hirsel` | 0/4 | `catalog` (L3) | — | — | — |
| 118 | `store-declare.hirsel` | 31/26 | `catalog` (L3) | `Mode`, `Declare`, `TableDecl`, `declared_table`, `declare_verify`, `seal_verify`, `declare`, `bind_declaration`, `has_table`, `declare_read_only`, `seal_read_only`, `bind_read_only`, `DeclaredCollection`, `describe_declarations`, `declare_follower`, `declare_migration`, `finish_migration`, `declare_export`, `finish_export_declaration`, `declare_table`, `declare_release_table`, `declare_activation_table`, `index`, `covering`, `covering_row`, `finish`, `seal`, `seal_follower`, `migrate_table`, `drop_named_table`, `rename_table` | `declaration_base` | — |
| 119 | `store-migration.hirsel` | 12/11 | `catalog` (L3) | `Migration`, `CollectionKind`, `CollectionInfo`, `list_collections`, `stage_migration`, `run_migration`, `ConflictKeys`, `BackfillOutcome`, `MigrationBackfill`, `MigrationChange`, `MigrationPreview`, `preview_migration` | `schema_plan`, `stage_migration_plan` | — |
| 120 | `store-projector.hirsel` | 27/53 | `maintenance` (L14) | `Binding`, `Generation`, `Image`, `Pass`, `Merge`, `Projection`, `Live`, `Cadence`, `cadence`, `Source`, `ProjectionDecl`, `Delta`, `Fold`, `Stepped`, `ProjectError`, `projection`, `projector`, `main_commit_version`, `projector_names`, `project_step`, `binding`, `live`, `projection_row`, `rebuild`, `pinned`, `pin`, `unpin` | `projector_run`, `source_reader`, `step_declared`, `projector_slot`, `read_projection`, `project_target`, `project_binding`, `borrowed_projection_base`, `pin_projection_target`, `project_commit`, `discard_projection_pass`, `discard_merge`, `retire_projection`, `rebind_projection`, `publish_projection_page`, `continue_projection`, `merge_projection`, `tend_projection`, `step_projector`, `pin_index`, `owned_pin`, `replace_projector_pin`, `pin_shares_head`, `change_pin`, `pin_delta`, `remove_pin` | — |
| 121 | `store-tail.hirsel` | 3/8 | `pages` (L2) | `key_codec`, `Tail`, `read_tail` | — | — |
| 122 | `store-read.hirsel` | 38/37 | `read` (L11) | `table_identity`, `Reader`, `reader`, `follower_reader`, `read_only_reader`, `staged_reader`, `copy_env`, `SpanRows`, `open_span_changes`, `next_span_change`, `span_changes`, `Rows`, `get`, `count_at`, `scan`, `scan_reverse`, `find`, `next`, `range`, `range_reverse`, `range_prefix`, `count`, `TxRows`, `next_in`, `IndexRows`, `IndexEntries`, `I8Code`, `primary_key`, `lookup`, `lookup_prefix`, `entries`, `next_indexed`, `next_entry`, `next_i8_entry`, `ReadOnly`, `read_only_table`, `read_only_get`, `read_only_snapshot` | — | — |
| 123 | `store-read-pages.hirsel` | 4/8 | `read` (L11) | `scan_page`, `find_page`, `scan_next`, `find_next` | — | — |
| 124 | `store-cell.hirsel` | 9/21 | `txn` (L12) | `Cell`, `cell_identity`, `cell_schemas`, `declare_cell`, `cell_fetch`, `cell_get`, `cell_set`, `cell_clear`, `published_cell` | — | — |
| 125 | `store-log.hirsel` | 20/23 | `txn` (L12) | `Log`, `LogBounds`, `log_identity`, `log_schemas`, `Retain`, `declare_log`, `retain_step`, `append`, `bounds`, `log_fetch`, `last`, `log_bounds`, `log_get`, `log_last`, `log_scan`, `log_range`, `trim_before`, `published_log`, `tail_window`, `window_changes` | `bind_retention` | — |
| 126 | `store-edge.hirsel` | 7/1 | `txn` (L12) | `Edge`, `declare_edge`, `edge_put`, `edge_delete`, `edge_out`, `edge_out_prefix`, `edge_in` | — | — |
| 127 | `main.hirsel` | 1/0 | `main` (unchanged) | `main` | — | — |

### 3.5 Declaration-level placement (v3.1, Findings 5–7 of 3115)

**v3.2 (L2).** `Head` carries branch data. The root keeps each live `Latest[Snapshot]` in `Db.followers`, keyed by head id and removed with the head. The root computes holder record sizes using `branch_root_bytes`, `pin_root_bytes` and `keyed_root_record_bytes`. Those helpers keep their L4 owners. `pages.HolderTable` takes the computed byte counts and updates them in the same operation as the holder change.

**L4 compaction ruling.** The pack allocator, locations, lists, owners and scrub leases move into `manifest.PackDiskState`. The root keeps `TieredCompaction` separately on `Db` through L4. L5 moves it and `TieredProbeCursor` into `pager.DiskPagerState`. The coordinator installs the candidate pack state and compaction progress after MANIFEST or Object checkpoint publication, in the existing cut order.

**L4 boundary rewrites.** `cut_participants`, `fill_cut_manifest`, `visit_cut_heads`, `visit_cut_pins`, `cut_manifest_size`, `cut_lineage_rows` and `settle_lineage_kills` move over `manifest.CutInput`. The root supplies holder views, the proposed delta and the cut scalars. `BranchReplacements` moves beside `ReplacedFrame`. Recovery supplies the locator's pack references and receives a `LoadedPackInventory`. Scrub helpers whose signatures name `Scrub`, `ScrubHistory` or `ScrubConfig` stay at the root under the public-type ruling.

v3 assigned owners by file and checked acyclicity by layer name. A body-level scan of the code v3 moves (`edge_scan.py` in `hirsel-briefs/store-modules-amend-v31-3121/`) finds 441 upward or same-layer references, 110 references to root-held privates and 47 to root public functions; 3115's eight rows are a sample of these. v3.1 replaces hand-listed exceptions with a computed placement, `place.py` in the same directory. It starts from v3's owners and a short list of fixed owners, then reaches a fixpoint:

- **Fixed owners.** 3103's write-path cells (`DataRows`, `push_data_row`, `op_slot`, `apply_collection_batch` in `pages`) and the page tree with the plain data it names (`PageTree`, `PageRoots`, `Roots`, the tiered read inventory and replay-branch types, `Pin`, `PinOwner`, `Head`, `Tip`, `State`), which `pages.HolderTable` holds. `data_order` reads `DataRows` and is fixed beside it in `pages`. Nothing else is fixed.
- **P1 pull-down.** A private helper that is referenced from a lower module moves down to the lowest using module, if its own references allow it. This includes a root-held helper that names no root handle and is not R3-bound.
- **P2 push-up.** A declaration that still references a higher layer moves to the module of that reference. It moves to the root instead when it references the root, a root handle, or a root public function that cannot be split. Root handles are `Db`, `DiskState`, `Preparation` and `Migration`, plus `Tx`, `Declare`, `StagedMigration`, `Reader` and `ReadOnly` outside their owning module.
- **A-split.** A root public function or constant named from a lower module keeps its public declaration at the root (R1), as an adapter over an internal body in the lowest using module, if that body's references allow it.
- **Landing.** Each declaration lands at the latest of its module's lane and the landings of everything it references.
- **Check.** No moved declaration references a higher layer or a different module of its own layer, and none lands before something it references. Public types named below are R2 rewrites and are not edges. A function name used as a value (a callback, a `?=>` handler or a spawned task) counts as a reference.

Result at `3f6679cc`: 0 violations. 395 private declarations change owner, 265 to another module and 130 to the root, and 15 public declarations are A-split. No declaration lands later than its module's lane, so no temporary root adapter is needed, and the owner below is each declaration's **final** owner. Every lane is therefore landable alone, in order. In particular, L1 (`types`, `format`) references only `status`, `types`, `format`, `core` and `std`.

L1 makes `TxIndexTiered` opaque so its `pins` field can use `format.TieredFrame`. Its root type name and scoped kind stay public.

The scan is token-based and conservative. A lane may move a declaration lower than this table only after it rewrites the body over neutral inputs, as its §10 row proposes (for example `fill_cut_manifest` over `manifest.CutInput`), the placement gate (§9.2) still passes, and the lane records the move in its ticket. A lane may never move a declaration higher, or to a later landing, without amending this section.

Owner changes from §3.4/§7.2 (every other private declaration keeps its §3.4/§7.2 owner; `placement.tsv` lists every declaration with its owner and landing; the public types `Health` and `StreamLatch`, named in both `status` and the root, share one row each):

| Module (lane) | v3.1 decls | Moved in (from its v3 owner) |
|---|---|---|
| `types` (L1) | 24 | from manifest: `META_LIMIT` |
| `format` (L1) | 136 | from history: `history_append_header`, `history_number`, `history_seal`; from host_stream: `ROLE_XINDEX`, `StreamPin`; from journal: `JournalInput`, `journal_decode_fork`; from manifest: `CLAIM_BYTES`, `PACK_INDEX_LIMIT`, `disk_fault`, `page_edge_order`; from pager: `TieredDataDelta`; from pages: `DataOp`, `StagedValue`, `staged_bytes`; from store: `round8`; from txn: `CLAIMS`, `DataOrder`, `stored_prior`, `word_be` |
| `pages` (L2) | 106 | from format: `encode_fork`, `old_data_op_size`, `stream_control_slot`; from history: `HistoryRef`; from indexes: `malformed`; from maintenance: `Pin`, `PinOwner`, `projector_pin_name`, `share_pin`; from manifest: `LocatedRoots`, `PagePack`; from pager: `ReplayBranch`, `ReplayOverlay`, `ReplayRow`, `ReplayTrimEvidence`, `ReplayValue`, `TieredPagerPack`, `TieredReadInventory`; from store: `branch_record_bytes`, `pin_record_bytes`, `same_name`; from txn: `CellEdit`, `LogState`, `apply_collection_batch`, `data_order`, `inline_cell`, `owned_cell`; from types: `holders_tiered`, `tiered_collections` |
| `catalog` (L3) | 48 | from indexes: `IndexProjection`, `ValueSchema`, `record_public`; from pages: `bulk_collection`, `collection_cursor`, `count_page_rows`, `define_collection`, `drop_collection`, `next_page_range`, `tiered_source`; from store: `named`; from txn: `LogRetention`, `next_bytes` |
| `manifest` (L4) | 283 | from format: `disk_check_frame_kind`, `disk_frame_bytes`, `disk_kind_limit`, `disk_read_frame`, `disk_read_frame_head`, `disk_read_header`, `disk_stream_payload`, `disk_stream_payload_counted`, `journal_prefix`, `journal_read_frame`, `journal_read_prefix`, `read_tiered_frame`, `stream_batch_check`, `stream_batch_head`, `stream_read_header`, `stream_register_decode`, `stream_xindex_decode`; from history: `history_frame`; from journal: `disk_io`, `disk_path`, `disk_remove_optional`, `disk_write_synced`, `journal_chunk`, `journal_wait_shared`, `remove_numbered`; from maintenance: `pin_order`, `pin_owner_tag`; from pager: `ReplacedFrame`, `fold_page`, `replaced_blob`, `replaced_page`, `tiered_leased`; from pages: `branch_root_bytes`, `capture_tail_bases`, `pin_root_bytes`, `resident_images`; from transfer: `ExportLease`, `TargetLimits`, `pack_leased`; from txn: `raised_capacity` |
| `pager` (L5) | 330 | from format: `encode_data_ops`, `encode_frame`, `encode_staged`, `encode_stream_ops`; from history: `ObjectsCache`, `history_fill_key`, `history_missing_object`; from journal: `JournalReplay`, `journal_replay`; from manifest: `drain_cut_queue`, `drain_queue_row`, `queue_page_children`; from read: `ChangeOutput`, `admit_change`, `change_output`, `finish_row`, `scan_page_after`; from txn: `descriptor_bounds`, `detach_answer`, `log_key`, `ordered_row` |
| `journal` (L6) | 56 | from host_stream: `SlotAck`; from manifest: `check_list_pack`, `cut_retirement`, `scrub_next`, `select_page_manifest`, `validate_directory_graph`, `validate_leased_cut`, `validate_page_manifest`, `validate_store_manifest`, `walk_directory_graph`; from store: `JOURNAL_BULK_GROUP`, `JOURNAL_CHUNK_BYTES`, `JOURNAL_WINDOW_STEP`; from transfer: `ObjectExportFetch`; from txn: `apply_log_append` |
| `capture` (L7) | 116 | from store: `read_only_manifest`, `stored_row_charge`; from transfer: `copy_export_prefix`, `scratch_result` |
| `history` (L7) | 144 | from manifest: `disk_loaded`, `rebuild_page_store`, `recovered_tiered_inventory`; from object_writer: `HistoryOpenBase`, `HistoryRetention`; from pager: `checked_tiered_pager_target`, `plan_bucket_batch`, `prepare_tiered_install`, `read_tiered_bucket_window`, `read_tiered_bucket_windows`, `tiered_window_cache_discard`, `tiered_window_cache_key`, `tiered_window_cached`, `unpack_bucket_window`; from store: `MIB`, `size_or` |
| `host_stream` (L8) | 176 | from object_writer: `HistoryName`, `ObjectAuthorityConfig`, `ObjectFollowHead`, `copy_follow_state`, `history_advance_claim`, `history_group_append`, `history_group_context`, `wrap_object_authority` |
| `object_writer` (L9) | 181 | from history: `history_check_list_pack`, `history_load_data_pack`, `history_open_checkpoint`, `history_read_pack`, `history_validate_manifest`, `history_write_pack_run`, `history_write_page_cut`, `pack_cache_room`, `pack_cache_scan`, `pack_cache_stage`, `pack_cache_trim`, `pack_cache_write`, `validate_bucket_graph`; from host_stream: `stream_follow_make_capture`, `stream_poll_head`, `stream_settle`, `stream_settle_publish`, `stream_settle_sealed`; from journal: `JournalChannels`, `JournalIdleTimer`, `JournalMsg`, `JournalWorker`, `journal_channels`, `journal_idle_timer`, `journal_loop`, `journal_worker`; from pager: `fetch_bucket_batch`, `read_checked_bucket_window`, `read_tiered_count`, `read_tiered_index_frame`, `read_tiered_point`, `read_tiered_point_direct`, `read_tiered_point_with`, `retry_tiered_read`, `retry_tiered_read_with`, `run_tiered_pager`, `serve_tiered_miss`, `serve_tiered_miss_with`, `snapshot_tiered_span_step_direct`, `snapshot_tiered_span_step_with`, `start_tiered_pager`, `start_tiered_pager_link`, `tiered_pager_bucket_batch`, `tiered_pager_frame`, `tiered_pager_handle`, `tiered_pager_point`, `tiered_pager_point_frame`, `tiered_pager_read_edges`, `tiered_window_cache_store`; from store: `check_read_only_manifest`, `open_checks`, `sim_level_bytes`; from transfer: `ObjectExportLease`, `object_export_read` |
| `indexes` (L10) | 54 | from catalog: `SchemaPlan`, `catalog_descriptions`, `catalog_entry_in`, `catalog_handle`, `catalog_index_handle`, `keep_schema`, `preview_catalog_changes`, `rebuild_cause`; from txn: `DataRefund`, `edit_frame` |
| `read` (L11) | 48 | from pager: `read_tiered_scan`, `tiered_snapshot_row`; from txn: `cell_walk_value`, `lower_bytes`, `reverse_byte_cursor` |
| `txn` (L12) | 211 | from catalog: `catalog_recharge`, `remove_declared_index`, `stage_create_table`, `stage_drop_table_key`, `stage_table`; from format: `keyed_drafted_charge`; from indexes: `add_chunk_totals`; from journal: `journal_apply`, `journal_apply_commit`, `journal_apply_ops`, `journal_collection_bulk`; from maintenance: `IndexStop`, `take_drift`; from pager: `replay_overlay_trim`, `replay_row_edit`, `replay_tiered_op`, `replay_trim_edit`, `settled_tiered_bytes`; from pages: `keyed_draft_live`; from read: `next_index_page`; from store: `KEY_LIMIT`, `Publication`, `entry` |
| `transfer` (L13) | 96 | — |
| `maintenance` (L14) | 49 | from catalog: `migrate_build`, `stage_migrate` |
| `store` (root, final) | 957 | from format: `decode_keyed_edits`, `journal_decode`, `journal_decode_commit`; from pages: `HolderDelta`, `branch_index`, `branch_slot`, `install_head`, `install_holder_delta`, `prepare_holder_slot`, `remove_head`, `retaining`, `state_with_delta`; from catalog: `MigrationJob`, `active_table`, `build_declared_record`, `declare_table_inner`, `define_index`, `existing_index_record`, `new_declaration`, `prepare_migration`, `read_only_base`, `settle_index_record`, `validate_local_projector`; from manifest: `cut_lineage_rows`, `cut_manifest_size`, `cut_participants`, `drain_tiered_frees`, `enqueue_tiered_free`, `fill_cut_manifest`, `note_fork_row`, `plan_page_cut`, `plan_paged_lists`, `raise_materialized_pin`, `settle_lineage_kills`, `tiered_drain_page`, `transfer_deferred_rows`, `validate_cut_pages`, `validate_new_page_manifest`, `visit_cut_heads`, `visit_cut_pins`; from pager: `snapshot_changes`, `snapshot_collection_rows`, `snapshot_tiered_initial_changes`, `snapshot_tiered_span`, `snapshot_tiered_span_step`, `tiered_changes`, `tiered_diff_deepen`, `tiered_diff_expand`, `tiered_diff_fill`, `tiered_diff_spans`, `tiered_row_changes`, `tiered_span_changes`; from journal: `capture_scan`, `disk_rotate`, `disk_session`, `disk_start`, `journal_recover`, `journal_scan`; from capture: `check_call_fields`, `check_journal`, `check_journal_commit`, `check_keyed_ops`, `check_prepared`, `check_stream_ops`; from history: `history_apply_entry`, `history_apply_suffix`, `history_paged_capture`, `history_paged_load`; from host_stream: `stream_apply_sealed_tail`, `stream_claim_attempt`, `stream_claim_hold_scope`, `stream_follow_apply_sealed_range`, `stream_follow_capture`, `stream_follow_load_fresh`, `stream_janitor_close_place`, `stream_load_settled`, `stream_no_hold_scan`, `stream_open_attached`, `stream_tail_apply`, `stream_tail_batch`, `stream_tail_prescan_xindex`, `stream_tail_scan`; from object_writer: `ObjectWriterOpen`, `advancing_follow_capture`, `follower_apply`, `follower_apply_task`, `follower_observe`, `follower_prepare_fresh`, `follower_service_loop`, `follower_step`, `history_load_admitted`, `history_open_existing`, `history_open_register`, `history_open_with`, `history_rebase_suffix`, `initial_follow_capture`, `log_follow_capture`, `object_follow_capture`, `object_follow_replay`, `prepare_object_writer`, `provision_follower`; from indexes: `plan_aggregate_edit`; from txn: `ForkPlan`, `available_journal`, `available_resident`, `check_table_write`, `claim_address`, `declare_value_collection`, `draft_body`, `draft_credit`, `finish_served`, `pop_table_edge`, `recorded_call`, `registered_completion`, `stage`, `stage_call`, `stage_with`, `window_insertions`, `writable`; from transfer: `matches_export`, `removed_pin`, `replay_export`; from maintenance: `DeclaredProjector`, `Projector`, `apply_projection_change`, `expiry_drift`, `forget_projector`, `held_projection_target`, `projection_page`, `write_projection` |

**L3 catalog boundary.** Keep declarations that require public root types in `store`. This includes `index_payload_readers`, `readers`, `RowDecoder`, `PageRows`, `row_value`, `index_decoder`, `typed_bound`, `index_bound`, `byte_cursor`, `index_revision`, `copied_state`, `catalog_positions`, `declare_projector_name` and `complete_declaration`. Catalog operations use `types.Failure` per R2. Internal records use their neutral L1 field types where available. `catalog.DeclarationBase.store` is `types.StoreKey`. The root passes plain capture fields to `catalog.follower_base`.

A-split public functions and constants (the public declaration stays at the root as an adapter; its body moves to the named module and lands with it):

- `BOOTSTRAP` → `capture` (L7)
- `follower_floor` → `maintenance` (L14)
- `CLAIM_DONE_MAX` → `format` (L1)
- `KEYED_OP_RECORD_MAX` → `txn` (L12)
- `claim_sweep_start` → `txn` (L12)
- `FRAME_LIMIT` → `types` (L1)
- `VALUE_LIMIT` → `types` (L1)
- `default_limits` → `types` (L1)
- `RECLAIM_FORMAT_VERSION` → `object_writer` (L9)
- `RECLAIM_TOOL_VERSION` → `object_writer` (L9)
- `last_complete_scrub_round` → `capture` (L7)
- `SUBJECT_MAX` → `format` (L1)
- `stream_record_matches` → `format` (L1)
- `READER_PIN_MIN` → `pager` (L5)
- `version` → `maintenance` (L14)

Single owners for the declarations whose v3 rows disagreed (3115 #7):
- `raise_materialized_pin`, `fill_cut_manifest` and `cut_participants` stay at the root.
- From `store-object-open.hirsel`:
  - `HistoryOpenBase` goes to `history` (L7);
  - `history_check_open` goes to `object_writer` (L9);
  - `history_load_admitted`, `history_open_with`, `history_open_existing`, `history_rebase_suffix` and `history_open_register` stay at the root, because open dispatch reaches L8's `stream_open_attached`, which also stays at the root.
- `disk_session` stays at the root, and `disk_lock` goes to `journal`.
- `history_import_base` is listed once, at the root.
- `law_call_reserve`'s helper closure (`test_db`, `reserve_tx`, `reserve_settles_pin`, `reserve_pinned_data`, `typed_put`) stays in production at the root (§7.2).

---

## 4. Owned Subsystem State (Finding 7)

### 4.1 Composition roots

```hirsel unbuilt
pub opaque type Db {
    store: StoreId,
    clock: Clock,
    limits: StoreLimits,
    attention_seq: u64,
    holders: pages.HolderTable,
    followers: List[BranchFollower],
    schema: catalog.SchemaState,
    indexes: indexes.IndexOwner,       // registry + drift; public IndexState/IndexDrift stay root (R1)
    pager: pager.PagerState,
    accounting: txn.AccountingState,
    call_streams: txn.CallStreamState,
    projectors: List[DeclaredProjector],   // root: registrations name root-typed StaticFns (§3.3)
    frame_seq: u64,
    disk: Option[DiskState]
}

type Backend = DirBackend(journal.DirJournalState) | ObjectBackend(object_writer.ObjectWriterState)

type DiskState {
    open: u64,
    options: Options,
    tasks: Tasks,
    retention: journal.RecordRetention,
    packs: manifest.PackDiskState,
    tiered: pager.DiskPagerState,
    transfer: transfer.TransferState,
    backend: Backend
}
```

`Db` is a root type, so its fields keep root types (`StoreId`, `StoreLimits`). `DeclaredProjector` stays at the root because its fold descriptor names root types (R4). `maintenance` takes neutral projector state from it.

### 4.2 Lifecycles

- **Memory** (`disk == None`): no journal or backend. Tickets use `accounting.changes`. `CallStreamState` and `PagerState` copy their policy fields from `Options`, so a memory open needs no `DiskState`.
- **Directory** → `DirBackend(journal.DirJournalState)`:
  - owns `lock: HostFile`, `d: Dir`, `creation`, `file_seq`/`file_end`, `poison`, the Directory worker, the `DirJournalMsg` queue and `QueueAccounting`;
  - timer and coalescing state are not part of this cutover. 3103 (STORE-SPEC v2.150) adds no timer task, channel or queue slot. Its gather and END deadlines are locals of the worker loop, waiting in `channel.receive_until`, and `DirJournalMsg` gains a payload-less `Flush`.
- **Object / Sim** → `ObjectBackend(object_writer.ObjectWriterState)`:
  - the 22 existing fields (§4.5) plus the moved per-backend `DiskState` fields (§4.4);
  - stream modes: standalone, attached with a live stream, and attached with `stream == None`.
- **Host-stream service:** a separate lifetime that owns `RegisterLane`, `HoldTracker`, GC state and slots.
- **`ReadOnly`, `SharedCapture`, `Follower`/`FollowerService`:** independent of `Db`.

### 4.3 `Db` (all 39 fields)

L2 adds the root-owned follower list described in §3.5. Replacing or deleting a head removes the root's handle when the head id leaves the holder table.

| # | Current `Db` field | Type | New owner | Invariant |
|---|---|---|---|---|
| 1 | `store` | `StoreId` | **`store` (stays on `Db`)** | Immutable 128-bit store identity fixed at genesis/open. |
| 2 | `clock` | `Clock` | **`store` (stays on `Db`)** | Injected host clock capability. |
| 3 | `limits` | `StoreLimits` | **`store` (stays on `Db`)** | Initialized from `named_limits(o.limits)` on memory open (`store.hirsel:609`) and overwritten with the selected MANIFEST's recorded limits on durable open (`store.hirsel:3919`, `store-object-writer.hirsel:218`). Immutable after open finishes. |
| 4 | `disk` | `Option[DiskState]` | **`store` (stays on `Db`)** | `None` iff `memory@1`; `Some` iff `log@1`/`objects@1`/`sim@1`. |
| 5 | `attention_seq` | `u64` | **`store` (stays on `Db`)** | Monotonic sequence bumped via `bump_attention(mut db)` on any operator-visible signal change across `indexes` (`count_drift`), `object_writer` (reclaim), or `transfer` (activation). |
| 6 | `frame_seq` | `u64` | **`store` (stays on `Db`)** | Minted on durable commit/fork installation BEFORE `handoff` (`store-commit.hirsel:228`); `0` on `memory@1` (memory tickets use `AccountingState.changes`, `store-commit.hirsel:254`). Once 3075 lands (before L1), its empty-commit skip does not increment it. `backend.journaled <= frame_seq`. |
| 7 | `pager` | `Option[Sender[TieredPagerMessage]]` | `pager.PagerState` | `Some` iff Tiered pager is active; paired with `pager_writer` and `park_notices`. |
| 8 | `pager_writer` | `Option[Sender[TieredPagerMessage]]` | `pager.PagerState` | One-slot writer demand channel into the same pager worker. |
| 9 | `park_notices` | `Option[Receiver[()]]` | `pager.PagerState` | Coalescing wake receiver handed out at most once via `park_notices(mut db)`; taking it leaves `None` while the pager (and `pager`/`pager_writer`) stays live. |
| 10 | `parked` | `ParkQueue` | `pager.PagerState` | Entries strictly ascending by `id < next_id`; at most one in-flight writer demand; cleared on close/poison. |
| 11 | `writer_pin_bytes` | `u64` | `pager.PagerState` | Effective policy copied from `Options.writer_pin_bytes` at open (works on memory opens too). |
| 12 | `indexes` | `IndexRegistry` | `indexes.IndexOwner` | Maps `(table, index, schema)` to extractors and `LogRetention`; installed through owner methods on both `seal` (`store-declare.hirsel:681-686`) and `install_activation` (`store-activation.hirsel:221`). |
| 13 | `drift` | `List[IndexDrift]` | `indexes.IndexOwner` as `List[indexes.DriftRep]` | At most one entry per `(table, index)`; demotes drifted index before subsequent writes (`§I.3`). The root builds public `IndexDrift` from it. |
| 14 | `heads` | `List[Head]` | `pages.HolderTable` | **Authoritative** live branch list: slot `0` is `main` (`id == 0`), slots `1..` ascending by `id`. Mutating `heads` (including keyed-root updates on commit, `store-commit.hirsel:239-240`) updates `record_bytes` and `branch_names` atomically inside `HolderTable`. |
| 15 | `branch_names` | `Map[String, u32]` | `pages.HolderTable` | **Derived view** of `heads[1..]`: exact bijection refreshed atomically inside `HolderTable` on every branch add/remove. |
| 16 | `pins` | `List[Pin]` | `pages.HolderTable` | **Authoritative** active pin list (application, projector, import); unique names per owner class. |
| 17 | `record_bytes` | `u64` | `pages.HolderTable` | Exact sum of `branch_root_bytes(h)` + `pin_root_bytes(p)` (including `keyed_root_record_bytes` updated on commit!); `<= limits.meta`. |
| 18 | `next_pin` | `u64` | `pages.HolderTable` | Strictly monotonic pin ordinal. |
| 19 | `next_branch` | `u32` | `pages.HolderTable` | Strictly greater than every live and historical branch id. |
| 20 | `next_node` | `u64` | `pages.HolderTable` | Strictly `>=` every page-node stamp reachable from any root in `heads` or `pins`. |
| 21 | `bases` | `TailBases` | `pages.HolderTable` | **Derived view** of projector-pin roots; rebuilt atomically inside `HolderTable` on every `pins` mutation. |
| 22 | `fence` | `u32` | `catalog.SchemaState` | Catalog/activation fence (`0` or `1`). **Mutable with rollback:** `install_activation` temporarily proposes `fence = 1` and restores `old_fence` on refused publication (`store-activation.hirsel:214, 239`). |
| 23 | `next_table` | `u32` | `catalog.SchemaState` | Strictly greater than every allocated collection/index id in `heads[0].roots.catalog`; committed atomically with `ReplaceMain` root publication. |
| 24 | `file_seq` | `u64` | Split: `journal.DirJournalState.file_seq` & `object_writer.ObjectWriterState.head_entry_seq` (a **new** field, §4.5) | On `DirBackend`: active `log@1` segment sequence (monotonic across rotations). On `ObjectBackend`: initialized to `head.entry.seq` (`store-object-writer.hirsel:315`), updated to `seq` on every object cut (`:627`), and returned by `archive(mut db, on)` (`:642`). |
| 25 | `file_end` | `u64` | Split: `journal.DirJournalState.file_end` & `object_writer.ObjectWriterState.draft_offset` (a **new** field, §4.5) | On `DirBackend`: exact byte offset in active `log@1` segment (`>= 72`, reset to `72` on rotate). On `ObjectBackend`: draft framing offset starting at `72`, advanced by `list.len(frame)` on commits/forks (`store-commit.hirsel:223`, `store-fork.hirsel:84`) before `object_journal_add` rebinds frames into GROUP entries (`store-object-journal.hirsel:104`), and reset to `72` on object cut (`store-object-writer.hirsel:628`). |
| 26 | `resident` | `u64` | `txn.AccountingState` | Conservative logical Memory byte count (exact when `settled == changes`, I4); checked against `available_resident`. |
| 27 | `tiered` | `u64` | `txn.AccountingState` | Logical Tiered byte count updated by prior deltas and settled on cuts. |
| 28 | `page_resident` | `u64` | `txn.AccountingState` | **Derived charge** (`resident_location_charge(packs.pages.locations)`), refreshed atomically by the root coordinator on open and every cut; reported beside `resident`, never admitted against `ceiling`. |
| 29 | `changes` | `u64` | `txn.AccountingState` | Monotonic count of non-skipped commits and cut-made mutations; also supplies `memory@1` commit ticket sequences (`store-commit.hirsel:254`). |
| 30 | `settled` | `u64` | `txn.AccountingState` | `changes` value at which `resident` was last exact (`settled <= changes`). |
| 31 | `ceiling` | `u64` | `txn.AccountingState` | **Mutable** effective resident admission ceiling: initialized to `options.resident_limit`, lowered by `reserve_import` (`store.hirsel:3262`), and restored by `forget_import` (`store-install.hirsel:84`). Exposed via `reserve_ceiling` / `restore_ceiling`. |
| 32 | `checkpointed` | `u64` | `txn.AccountingState` | Latest durable cut generation installed (`0` on memory before cut, `>= 1` on disk/object). |
| 33 | `keyed_credit` | `KeyedCredit` | `txn.CallStreamState` | Exact resident/padding/journal credit reserved for open keyed claims. |
| 34 | `admits` | `Map[Bytes, OpenAdmit]` | `txn.CallStreamState` | Active unreplied `Admitted(p)` call-stream entries committed during this open (`§1.8`). |
| 35 | `stream_retention_ms` | `u64` | `txn.CallStreamState` | Effective policy copied from `Options` on open (including memory opens). |
| 36 | `streams_per_caller_max` | `u64` | `txn.CallStreamState` | Effective per-caller call-stream cap copied from `Options`. |
| 37 | `streams_max` | `u64` | `txn.CallStreamState` | Effective global call-stream cap copied from `Options`. |
| 38 | `stream_sweep_after` | `Option[StreamSweepCursor]` | `txn.CallStreamState` | Incremental cursor for bounded call-stream expiry sweeps. |
| 39 | `projectors` | `List[DeclaredProjector]` | **`store` (stays on `Db`)** | Registered local/follower projectors (`§1.14`); their `Fold` descriptors name root `Tx`/`Delta` (R4), so they stay at the root; `maintenance` receives neutral walk state. |

### 4.4 `DiskState` (all 34 fields)

| # | Current `DiskState` field | Type | New owner | Invariant |
|---|---|---|---|---|
| 1 | `open` | `u64` | **`store` (stays on `DiskState`)** | Open incarnation/epoch stamped into every `Ticket` and `Snapshot` (`seq` on Directory, `head.pair.epoch` on Object). |
| 2 | `options` | `Options` | **`store` (stays on `DiskState`)** | Immutable validated `Options` snapshot fixed at open (also source for `forget_import` restoring `AccountingState.ceiling`). |
| 3 | `tasks` | `Tasks` | **`store` (stays on `DiskState`)** | Task capability shared across `journal`, `pager`, `object_writer` (same capability previously duplicated on `ObjectWriterState.tasks`). |
| 4 | `record_cut` | `u64` | `journal.RecordRetention` *(on `DiskState`)* | First coordinate of retention floor: first live `log@1` segment or `objects@1` checkpoint entry seq. |
| 5 | `recorded_after` | `u64` | `journal.RecordRetention` *(on `DiskState`)* | Second distinct coordinate of retention floor: main-branch version at `record_cut`. Both coordinates installed together on every successful cut for either backend. |
| 6 | `pages` | `PageDisk` | `manifest.PackDiskState` | **Authoritative** pack allocator/location/list/owner state. The root installs it only after MANIFEST or Object checkpoint publication succeeds, alongside the `AccountingState.page_resident` refresh. Compaction progress lives on `Db` in L4 and moves to `pager.DiskPagerState` at L5. |
| 7 | `scrubs` | `List[Arc[ScrubLease]]` | `manifest.PackDiskState` | Active scrub leases pinning referenced packs against AP21 reclamation. |
| 8 | `pager_task` | `Option[Task[()]]` | `pager.DiskPagerState` | Owned Tiered pager worker task; cancelled and joined on `close`. |
| 9 | `tiered_dirty` | `TieredDirty` | `pager.DiskPagerState` | Dirty Tiered collections/overlays awaiting cut emission; reset atomically on cut publish. |
| 10 | `restamp` | `Option[u32]` | `pager.DiskPagerState` | Collection id queued for `Memory -> Tiered` re-stamping on schema cut. |
| 11 | `unload` | `Option[u32]` | `pager.DiskPagerState` | Collection id queued for `Tiered -> Memory` unload on schema cut (mutually exclusive with `restamp`). |
| 12 | `lease` | `Option[Arc[ExportLease]]` | `transfer.TransferState` | Active online-export lease pinning segments/packs against removal. |
| 13 | `steps` | `List[LeaseStep]` | `transfer.TransferState` | Ordered schema commits on `main` since `lease` last advanced (`STEPS`). |
| 14 | `imported` | `Option[KeptImport]` | `transfer.TransferState` as `Option[transfer.KeptImportRep]` | Active kept-import pin and delta-round cursor; cleared by `forget_import` along with `AccountingState.ceiling` restore. Public `KeptImport` stays at the root and is built from the rep. |
| 15 | `reserving` | `Option[Inline[u8, 32]]` | `transfer.TransferState` | Digest of currently reserved `ValidatedImport`. |
| 16 | `target` | `Option[TargetLimits]` | `transfer.TransferState` | Target limits enforced on cuts while cutover is staged. |
| 17 | `lock` | `Option[HostFile]` | `journal.DirJournalState` *(non-optional `HostFile`)* | Exclusive `LOCK` held from before recovery through worker shutdown on `close` (`store-journal-status.hirsel:156`). |
| 18 | `d` | `Option[Dir]` | `journal.DirJournalState` *(non-optional `Dir`)* | Directory capability for `log@1` segments, packs, and `MANIFEST`. |
| 19 | `creation` | `Bytes` | `journal.DirJournalState` | Pre-allocated 4 KiB-aligned scratch block for `O_DIRECT` segment creation (never allocated on `ObjectBackend`). |
| 20 | `acks` | `Receiver[Ack]` | Split: `DirJournalState` (`Receiver[DirAck]`) & `ObjectWriterState` (`Receiver[ObjectAck]`) | Strictly ordered durability/error acks; missing worker ack on closed channel poisons (`Dir`) or latches terminal failure (`Object`). |
| 21 | `journaled` | `u64` | Owned per backend (`QueueAccounting`) | Highest `frame_seq` durably persisted (`journaled <= Db.frame_seq`). |
| 22 | `inflight` | `u64` | Owned per backend (`QueueAccounting`) | Unacknowledged bytes in flight (`<= db.limits.frame`, the open's recorded limit, §5.1 step 4); charged **once** after successful `channel.send`, subtracted **once** via `bytes_out - acked_bytes` on ack. |
| 23 | `uncovered` | `u64` | Owned per backend (`QueueAccounting`) | **Distinct from `since_cut`!** On Directory reopen (`store.hirsel:3937`), initialized to recovered journal bytes (not `0`); cleared to `0` **only after** `MANIFEST` or Object `head` publication succeeds (`store.hirsel:4109`, `store-object-writer.hirsel:614`). Checked against `available_journal`. |
| 24 | `since_cut` | `u64` | Owned per backend (`QueueAccounting`) | **Distinct from `uncovered`!** Starts at `0` after reopen; on Directory cut, reset to `0` **before** draining (`store.hirsel:4020`) so refused cuts do not tight-loop; on Object cut, reset to `0` after publication (`store-object-writer.hirsel:615`). Triggers `maybe_checkpoint` when `> options.checkpoint_bytes`. |
| 25 | `acked_bytes` | `u64` | Owned per backend (`QueueAccounting`) | Cumulative acknowledged byte watermark; prevents `inflight` drift. |
| 26 | `written` | `Receiver[WriteNotice]` | Owned per backend (`DirJournalState` & `ObjectWriterState`) | Receives `Written` barrier confirmations before `recorded`/`reexec` reads active journal. |
| 27 | `written_send` | `Sender[WriteNotice]` | Owned per backend (`DirJournalState` & `ObjectWriterState`) | Cloned sender for worker `Barrier` replies. |
| 28 | `synced` | `Option[Receiver[()]]` | Owned per backend (`DirJournalState` & `ObjectWriterState`) | Coalesced group-sync notification handle sent **after** `Journaled`/`ObjectJournaled` ack (and on terminal failure). |
| 29 | `poison` | `Option[IoError]` | Owned per backend: `DirJournalState.poison` & `ObjectWriterState.poison` | Sticky first I/O failure (`Closed` on dead outbound queue or worker channel EOF, or Directory I/O error). On `ObjectBackend`, coexists with `ObjectWriterState.failed: Option[Error]` for typed non-I/O failures (`Fenced`, `Latched`, `Remote`, `Undecided`), preserving covered-ticket-before-error! |
| 30 | `journal` | `Sender[JournalMsg]` | Split: `DirJournalState` (`Sender[DirJournalMsg]`) & `ObjectWriterState` (`Sender[ObjectJournalMsg]`) | Bounded outbound queue at its current depth on both backends; any depth change belongs to 3103, not this cutover. |
| 31 | `pending` | `Option[Bytes]` | Owned per backend (`QueueAccounting`) | At most one staged commit/fork frame awaiting immediate `handoff` after commit install. |
| 32 | `journal_task` | `Task[Result[(), Error]]` | Split: `DirJournalState` & `ObjectWriterState` | Owned background journal worker task; joined on `close` before `LOCK` release (Directory). |
| 33 | `journal_idle_task` | `Option[Task[()]]` | `object_writer.ObjectWriterState` *(never on `DirBackend`)* | Idle head-poll timer (standalone `objects@1`) or `slot_relay` task (attached host-stream Place); cancelled and joined on `close`. |
| 34 | `object` | `Option[ObjectWriterState]` | `object_writer.ObjectWriterState` *(the `ObjectBackend` arm of `Backend`)* | All 22 existing fields are listed in §4.5, including `cache` and `tasks` (folded into `DiskState.tasks`). Fields moved in from `Db`/`DiskState` (`head_entry_seq`, `draft_offset`, acks, queue accounting, `poison`) and v2's `authority_kind` are additions. `attached == true, stream == None` still forces an open-time cut. |

### 4.5 `ObjectWriterState` (all 22 existing fields, `store-object-writer.hirsel:1-24`)

| # | Field | Type | v3 owner | Note |
|---|---|---|---|---|
| 1 | `workers` | `Option[std.object.Workers]` | `ObjectWriterState` | Upload worker pool. |
| 2 | `tasks` | `Tasks` | **Consolidated into `DiskState.tasks`** | The same capability; it is not duplicated. |
| 3 | `health` | `Latest[Health]` | `ObjectWriterState` | Holds root `Health` from `"status"`, which the lower module may name. |
| 4 | `reclaims` | `Latest[ReclaimFailures]` | `ObjectWriterState` | |
| 5 | `unhealthy` | `Option[Receiver[()]]` | `ObjectWriterState` | Handed out once by `on_unhealthy`. |
| 6 | `export_lifetime` | `Option[Sender[()]]` | `ObjectWriterState` | Cleared by `forget_import` through an owner operation. |
| 7 | `watch` | `Option[Task[()]]` | `ObjectWriterState` | |
| 8 | `terminal` | `Sender[()]` | `ObjectWriterState` | |
| 9 | `locator` | `HistoryLocator` | `ObjectWriterState` | |
| 10 | `levels` | `List[String]` | `ObjectWriterState` | |
| 11 | `weak` | `Option[WeakWaiters]` | `ObjectWriterState` | Sim only. |
| 12 | `pair` | `HistoryPair` | `ObjectWriterState` | |
| 13 | `recovered` | `HistoryRef` | `ObjectWriterState` | |
| 14 | `entries` | `u64` | `ObjectWriterState` | |
| 15 | `failed` | `Option[Error]` | `ObjectWriterState` as `Option[types.Failure]` | The root maps it to `Error` at the boundary (R2). It coexists with `poison`. |
| 16 | `stream` | `Option[HostStreamId]` | `ObjectWriterState` as an internal stream-id rep | `HostStreamId` stays public at the root. |
| 17 | `attached` | `bool` | `ObjectWriterState` | Can be `attached && stream == None`, which still forces an open-time cut. |
| 18 | `has_sealed` | `bool` | `ObjectWriterState` | |
| 19 | `drop_attach_on_cut` | `bool` | `ObjectWriterState` | |
| 20 | `archive_on` | `u64` | `ObjectWriterState` | |
| 21 | `pending_archive_on` | `Option[bool]` | `ObjectWriterState` | |
| 22 | `cache` | `Option[ObjectsCache]` | `ObjectWriterState` holding `pager.ObjectsCache` | Set at construction (`store-object-writer.hirsel:317`) and passed into cut emission (`:506-518`). History reads it again on reopen (`store-history-packs.hirsel:111-122`). It stays disposable: there is no `Backend` arm, and reopen works after the cache is deleted. L9 must carry it through construction and cut. |

Fields moved from `DiskState`/`Db` into the object arm are additions, not part of this count: `file_seq` → `head_entry_seq`, `file_end` → `draft_offset`, `acks`, `journal`, `journal_task`, `journal_idle_task`, the `QueueAccounting` fields, `written*`, `synced` and `poison`. v2's `authority_kind` is also an addition.

---

## 5. Coordinator, Paused-Worker Settlement and Recovery (Findings 4, 5, 6)

### 5.1 Foreground commit and checkpoint coordinator (root)

The root is the only foreground coordinator.

1. **Draft:** `txn` stages the transaction and returns its outcome and a `txn.CutDecision`. On drift, the root records it in `indexes.IndexOwner` (as an internal `DriftRep`), bumps `attention_seq` and returns `Failed(Drift)`.
2. **3075 empty-commit skip (once 3075 lands, before L1):**
   - On Directory, there is zero I/O and no increment.
   - On Bucket or host-stream, the root runs a fresh head check even when the ticket is covered. A changed head or a lost claim returns `Fenced`.
   - Neither path cuts. This uses 3075's proven check as landed (§8).
3. **Admission and optional pre-commit cut:** check `uncovered + len(frame) <= available_journal(limits, state)`. `txn` receives that bound as an argument. On `CutDecision.PreCut`, the root runs `cut_install(NoHolderChange)`, then reseals the frame against the post-cut `(file_seq, file_end)`.
4. **Headroom:** `wait_for_frame_room` waits while `inflight > db.limits.frame - size` (`store.hirsel:1385-1394`). `db.limits.frame` is the open's recorded frame limit, and a store may record a limit smaller than `FRAME_LIMIT`. Using `FRAME_LIMIT` would let more bytes into flight than the open's budget allows.
5. **Install:**
   - update `CallStreamState`;
   - admit `tiered_dirty`;
   - advance the backend offset;
   - **on disk opens only**, mint `frame_seq += 1`;
   - stash `pending`;
   - install holder roots;
   - update `AccountingState`;
   - publish `Latest[Snapshot]`.

   Memory opens never touch `frame_seq`. Their tickets use `changes`.
6. **Handoff:** take `pending` and send it. Charge `inflight`, `uncovered` and `since_cut` only after the send succeeds (`store.hirsel:1142-1148`). A closed channel poisons the writer (Directory) or records the failure (Object).
7. **Post-handoff:** `maybe_checkpoint` runs (it is skipped on `Refused(Journal)` and on the 3075 skip). It may run a second `cut_install`, and it swallows `Full(_)`.

**Directory cut order:**
1. Check health.
2. Set `since_cut = 0` before draining.
3. Drain to the final ticket.
4. Reserve the holder slot.
5. Prune leases.
6. Build the participants from the root's `manifest.CutInput`.
7. Stage the tiered cut.
8. In the retry loop, rotate. A retry reuses an already-created segment (`store.hirsel:4070`).
9. Write the packs.
10. Validate.
11. Sync the directory, write and sync `MANIFEST.tmp`, rename it, and sync the directory again.
12. Install pages, retention, `uncovered = 0`, tiered, accounting and holders.
13. Remove retired files. A removal error after publication poisons the writer (`store.hirsel:4132`).

### 5.2 Settlement under `ObjectPause`

The worker hands its `ObjectJournalSession` over `handover` and blocks on `resuming`. Settlement runs on a `host_stream.SettleView`, and that view never holds a sender to the paused worker. The view carries:
- `client`, `register`, `authority`, `buffers`, `archive_on`, `op_ms`;
- the batch cursor;
- **the optional slot identity and incarnation** `(sid, inc)` that `stream_poll_head` reads from `lane.stream` (`store-stream-settle.hirsel:23-35`).

**Report delivery stays outside the view.** The cut path in `object_writer` returns the session, then delivers the cutoff or detach report to the service.

**Return path:**
- On every normal exit (applied, fenced, not applied, undecided, error), the cut path sends the session back on `resuming`. It latches `failed` when `register.terminal` is set.
- **Cancellation and closed-channel alternatives:**
  - if the foreground task waiting on the cut is cancelled, it unwinds through `CONC-7`, and dropping `resuming` closes the channel;
  - a worker whose `resuming` receive returns `Closed` terminates with its terminal signal;
  - O3 observes the resumed worker, the fence verdict, or termination shown by joining the worker task (§9.2).

Only `HistoryApplied` installs the new head, attach, locator and retention. Every other outcome restores the prior attach. Sealed tails settle before the live tail. Direct-service CLAIM and slot CLAIM stay distinct.

On `SlotFenced`, a covered prefix is acknowledged before `Fenced`, and a missing prefix returns `Undecided`. A latch maps every outstanding attached ticket to `Undecided`.

### 5.3 Recovery and capture entry points

1. **Directory writer open** (`disk_session`, root orchestration over `journal`):
   - read the `MANIFEST` header;
   - take an exclusive `LOCK` before recovery;
   - check `EXPORT`;
   - remove orphan `.end` files and sync the directory before creating any successor;
   - replay middle segments up to the successor's `prev_end` and the final segment up to `END`;
   - only physical torn-tail faults at or after the final `END` end cleanly; logical frame errors propagate.

   **Incomplete segment creation stays a separate case.** A successor that is missing or has a short header after a crash (`store-journal-recover.hirsel:99-110`) is handled by its own rule, not by the torn-tail rule. Close holds `LOCK` through the worker join (`store-journal-status.hirsel:127-162`).
2. **Live capture** (`store-capture.hirsel:253-300` and `store-journal-recover.hirsel:273-305`): first try the shared terminal lock (`try_lock_shared`). Three outcomes:
   - **(a) The try succeeds.** Sync the directory, check that the `MANIFEST` bytes are unchanged, then sync, stat and scan the frames past `END` under the lock, and unlock.
   - **(b) The try fails and the writer holds `LOCK`** (`capture_lock_free` returns false). Use only the recorded `END`, or 72 when there is none, without waiting for the terminal lock. In the journal walk, `torn_tail = false`.
   - **(c) The try fails and `LOCK` is free.** A dead writer may have direct I/O in flight. Poll every 1 ms, up to the supplied deadline (`Remote(Retriable(Deadline))`), then retry the try-lock.

   In every branch, keep the retained segment descriptors, the directory sync and the exact `MANIFEST` recheck. Capture never blocks on `LOCK`.
3. **Offline backup** (`store-backup-offline.hirsel`):
   - `backup_lock` try-locks `LOCK`;
   - `prepare_backup` moves the acquired `HostFile` into `prepare_capture(from, c, Copy, Some(take lock))`, which becomes `PreparedCapture.lock`;
   - `backup_offline` checks and copies under that lock;
   - on an orphan pin or a missing terminal, the backup releases the lock and `backup_with_writer` opens the writer at the root.

   No second locked-recovery entry exists. v2's `disk_recover_locked` does not exist in the source and is removed from the plan.

---

## 6. Claimant and Service (Findings 2 and 8)

### 6.1 Lane 0 is folded into 3048

The candidate-build gap at `store-stream-claim.hirsel:585-606` is fixed inside 3048, which is running and rewrites the same builder:
- catch a construction failure while the hold is still owned;
- keep the H write outside that handler;
- keep the failure bounded so cleanup can still issue its request;
- report "abandon attempted and failed" apart from "row gone".

3048 is a hard predecessor of L1. L8 builds on the builder 3048 lands.

### 6.2 `HoldGuard`: one owner for the prospective token (v3.1)

Today the token is minted (`disk_identity(r)`) before the Hold PUT (`store-stream-claim.hirsel:544-564`). The generic `Error(take problem)` arm of the Hold result drops it. `stream_reg_write` reaches that arm after an earlier uncertain attempt, for example on a later fatal error or a failed read after a mismatch. The revision lookup that follows `Written` can fail too. The guard gives the token one owner from before issue until the H request is irrevocably handed to the object lane, or until the hold is abandoned.

```hirsel unbuilt
// host_stream. A resource: dropping it on unwind runs its finalizer (CONC-7, FAIL-11) under
// a fixed grant (MEM-27); everything else the cleanup needs was reserved by hold_begin.
pub opaque resource type HoldGuard: send grant bytes HOLD_CLEANUP_GRANT {
    client: std.object.Client,          // the claim's client
    lane: StreamLane,                   // S/reg state and ETag
    buffers: TailBuffers,
    token: types.StoreKey,              // minted before the Hold PUT
    against: u64,                       // register incarnation the Hold was issued against
    cleanup: AbandonScratch,            // register key at the stream root, body and register buffers
    cleanup_ms: u64
} drop(hold_guard_drop) {
    // May wait, sees no cancellation, uses only these fields and the grant, no `?`.
    let _ = abandon_prepared(mut client, mut cleanup, token, deadline_after_ms(client, cleanup_ms))
}

// Admits the guard and its cleanup before any Hold I/O. A refusal issues no Hold.
pub fn hold_begin(take client: std.object.Client, take lane: StreamLane, take buffers: TailBuffers, token: types.StoreKey, against: u64, cleanup_ms: u64) -> Result[HoldGuard, (std.object.Client, StreamLane, TailBuffers, types.Failure)]
pub fn hold_issue(mut guard: HoldGuard, deadline: u64) -> HoldIssue waits
pub fn hold_reconcile(mut guard: HoldGuard, deadline: u64) -> HoldIssue waits
pub fn hold_abandon(take guard: HoldGuard, deadline: u64) -> (std.object.Client, StreamLane, TailBuffers, AbandonResult) waits
pub type HoldIssue = Held(rev_hold: u64) | Superseded(inc: u64) | Refused(types.Failure) | Unknown(types.Failure)
pub type AbandonResult = Abandoned | AbandonFailed(types.Failure)

// Private to host_stream: only the claim transitions below may disarm a guard.
fn hold_release(take guard: HoldGuard, proof: Released) -> (std.object.Client, StreamLane, TailBuffers)
fn hold_handoff(take armed: Armed, precondition: std.object.Precondition, take candidate: Bytes, deadline: u64) -> Handoff waits
type Released = Superseded(inc: u64) | Refused(types.Failure)
type Armed = Guarded(HoldGuard) | Unguarded(std.object.Client, StreamLane, TailBuffers)
type Handoff = Issued(Disarmed, std.object.PendingStore) | NotIssued(Armed, Bytes, types.Failure)
// The guard's parts after the lane owns H. `hold` keeps the token and its cleanup scratch for the
// one post-handoff abandon, which runs only on a confirmed Conflict.
type Disarmed { client: std.object.Client, lane: StreamLane, buffers: TailBuffers, hold: Option[(types.StoreKey, AbandonScratch)] }
fn hold_conflict_abandon(mut d: Disarmed, deadline: u64) -> AbandonResult waits
```

**Ownership, from issue to H:**

1. **Admit, then wrap.** The claim mints the token and calls `hold_begin` before any Hold I/O. `hold_begin` reserves the `AbandonScratch` from the claim's limit: the absolute register key at the stream root, a register buffer for `max_rows` rows and an Abandon body buffer of the matching size. The guard's `grant bytes` covers only fixed-size request bookkeeping. A refusal returns the parts and the failure, and no Hold is issued. From here on every pre-H operation reaches the client only through the guard: Hold issue, reconciliation, revision lookup (`stream_hold_rev`), the delta scan, the final head GET and candidate construction.
2. **Issue.** `hold_issue` returns one of:
   - `Held(rev)`: the write returned `Written` and the revision lookup found the row.
   - `Superseded(inc)`: a successful register read shows an incarnation `inc > against` and no row for the token. This is the only evidence that the Hold can never install (`STORE-SPEC.md:4414,4479`, `store-stream-service.hirsel:299-302`).
   - `Refused(e)`: a definitive refusal that installed nothing, such as a full register.
   - `Unknown(e)`: `Undecided`, a generic error after an uncertain attempt, a failed lookup, or a successful read at the **same** incarnation without the token. A same-incarnation read can precede a delayed conditional Hold, so absence there proves nothing.

   On `Unknown`, the claim may call `hold_reconcile` until its deadline. Reconcile re-reads `S/reg` and returns `Held` when the token is present, `Superseded` only on a strictly higher incarnation, and `Unknown` otherwise. When the deadline passes on `Unknown`, the claim calls `hold_abandon` and returns the failure (`FailedPreH`).
3. **Release without Abandon.** Only `Superseded` and `Refused` disarm a guard without an Abandon, through the private `hold_release`. `Superseded` continues to the no-hold scan, which may write TERM. `Refused` returns its failure to the caller **without** entering the scan. Neither `Unknown` nor any other outcome reaches the scan, so no TERM and no H follow an uncertain Hold at an unchanged incarnation.
4. **Explicit abort.** Any decision before the handoff not to proceed calls `hold_abandon`: a confirmed move, a candidate-build failure, an authorization refusal, a deadline. It returns `Abandoned` or `AbandonFailed(e)`. The second means the row may still exist and service rule (a) will remove it. An Abandon over an absent row counts as present (`store-stream-service.hirsel:171-182`), so abandoning an unissued token is harmless.
5. **Cleanup address.** Explicit and finalizer Abandons use `abandon_prepared`, which addresses the register through the scratch's absolute stream-root key and sets the client prefix to the stream root itself before each request. It never relies on `stream_at_root` restoring the prefix, because that restore is skipped when cancellation unwinds a Place-head read (`store-stream-claim.hirsel:274-279`). It reads the register into the scratch buffer and applies the intent in place, so it does not clone the register (`store-stream-service.hirsel:224-236,310-325`).
6. **Cancellation or failure unwind before the handoff.** If the claim task is cancelled at any `waits` point before the handoff, or an unexpected `?` unwinds the scope, the guard is dropped and its finalizer issues one Abandon bounded by `cleanup_ms`. It may wait, observes no cancellation and allocates only from its grant (`docs/spec/tasks.md:150-176`, `docs/spec/budgets.md` MEM-27).
7. **Handoff.** `hold_handoff` is the one transition that ends guard ownership. It takes the still-armed guard (or, on the no-hold path, the unguarded parts) and the finished candidate, and issues the H PUT through `std.object.store_if_issue`. That function is required to have no cancellation point other than the entry of its one `channel.send`. If the send succeeds, the object lane owns the request. `hold_handoff` then disarms the guard in pure code, which is never cancelled (CONC-7). It returns `Issued` with the pending store and a `Disarmed` record holding the client, lane, buffers, token and cleanup scratch. If cancellation is observed at the send entry, the guard is still armed and its finalizer abandons. If the lane is closed, `NotIssued` returns the armed guard, the candidate and the failure, and the claim abandons explicitly.
8. **After the handoff** the claim finishes with `std.object.store_if_finish(mut disarmed.client, pending)`. On `Conflict`, which confirms that H did not install, it calls `hold_conflict_abandon` with the `Disarmed` record. On `Refused`, `Undecided` or a cancellation while the lane owns the PUT, it does not abandon and the row stays `Held` (rule e). After `Applied` it never abandons. On every outcome other than `Conflict`, the claim keeps `disarmed.client`, `lane` and `buffers` for its next step and drops the token and scratch; nothing else reads them.
9. **Process death.** No finalizer runs. Service rule (a) (`first_seen_ns + hold_ns`) removes the row, as it does today.

**Prerequisite lane (`std.object`, before L8).** Split `store_if_retry` at its first `submit` (`core/std/object.hirsel:2552-2580`) into `store_if_issue(mut Client, key, pre, take body, deadline) -> Result[PendingStore, (Bytes, Error)] waits`, which returns once the lane has accepted the request, and `store_if_finish(mut Client, take PendingStore) -> (Result[Conditional, Error], Bytes) waits`, which receives the reply and runs the existing retry rules. `store_if_retry` becomes issue then finish. `store_if_issue` must build the request before its `channel.send` and do nothing that waits after it. The lane writes that property as a law over the split. `history_write_head` (`store-register.hirsel:14-16`) gains the same split. This lane changes no wire behaviour.

`ClaimMachine[T]` keeps v2's per-phase payloads and its four post-H outcomes (§6.3). It is a `resource` type because the `Acquiring`, `Held` and `ReadyToBuild` phases own a `HoldGuard`. Phases after `Issued` own no guard, so no code path can abandon a hold after the H request reached the lane.

### 6.3 Claim phases after the guard

```hirsel unbuilt
pub type ClaimPhase[T] =
    Init
  | Acquiring(guard: HoldGuard, head: history.HistoryHead, attach: format.Attach, tail: format.StreamRef, replay: pager.JournalReplay, archive_on: u64)
  | Held(guard: HoldGuard, rev_hold: u64, head: history.HistoryHead, attach: format.Attach, tail: format.StreamRef, cursor: TailScan, replay: pager.JournalReplay, archive_on: u64)
  | NoHold(client: std.object.Client, lane: StreamLane, buffers: TailBuffers, head: history.HistoryHead, attach: format.Attach, tail: format.StreamRef, cursor: TailScan, replay: pager.JournalReplay, archive_on: u64)
  | ReadyToBuild(armed: Armed, head_tag: std.object.ETag, attach_next: format.Attach, claimed: history.HistoryHead, replay: pager.JournalReplay, admitted: T, archive_on: u64)
  | HIssued(disarmed: Disarmed, pending: std.object.PendingStore, attach_next: format.Attach, claimed: history.HistoryHead, authority: Bytes, replay: pager.JournalReplay, admitted: T, archive_on: u64)
  | HeadPublished(register: history.HistoryRegister, authority: Bytes, replay: pager.JournalReplay, admitted: T, archive_on: u64)
  | Claimed
  | FailedPreH(types.Failure, Option[AbandonResult])
  | HAmbiguous(hold: Option[types.StoreKey], err: types.Failure)
  | FailedPostH(types.Failure)

pub type ClaimStep[T] = Continue | ConflictRestart | ConflictLost(history.HistoryPair) | AmbiguousH(types.Failure) | ClaimPublishFailed(types.Failure) | Done(ClaimedPlace[T])
```

`ReadyToBuild` holds `Armed`: `Guarded` on the hold path and `Unguarded` on the no-hold path (entered only from `Superseded`), so both paths reach the same handoff and neither needs a separately constructed commit value. Candidate construction (including 3048 `ORIGIN`) runs in `ReadyToBuild` while the guard is armed; a failure there abandons. `FailedPreH` carries `None` when the guard was released by `Refused` and so needed no Abandon.

These guarantees are kept from v2:
- the 3048 `ClaimAuth` check runs before `TERM`, Hold or H;
- a fresh physical holder is used per open;
- `granted` is retained across retries within one open's retry loop (it starts empty for each open, `store-stream-claim.hirsel:296-308,534-539`);
- `head_tag` comes from the step-5 final head GET.

The four H outcomes:
- `Applied` → `HeadPublished`, with no abandon;
- `Conflict` → `hold_conflict_abandon`, then `ConflictRestart` (same pair, `granted` retained) or `ConflictLost`;
- `Refused`/`Undecided`, or cancellation after the lane accepted H → `HAmbiguous` with the row left `Held`;
- a failed step-7 CLAIM → `FailedPostH`, with no abandon.

### 6.4 Service trackers and directive validity (v3.1)

`RegisterLane` (with `pending` and `stalled`), `HoldTracker` (`observed_rows`, `tickets`, `active_settles`, `stall_polls`, `next_settle_id`, `last_reg_ns`) and `StreamService` keep v2's field lists. `SlotPlace` keeps `{store, epoch, fenced, settled_from, answers}` (`store-stream-task.hirsel:30-42`). The slot attach also passes the recovered frame limits (`1b1fc30f`), and L8 carries that through. Four behaviour changes:

1. **Completion reports are bound to the place.** `ReportSettled`, `ReportFenced` and `ReportDetached` carry the place's `store` id as well as slot and epoch (`store-stream-slot.hirsel:42`). Their senders (`store-object-journal.hirsel:377-389,426-435`, `store-object-writer.hirsel:576-578`, `store-object-export.hirsel:83,89`, `store-stream-archive.hirsel:246`) pass the store id of the attachment they settle. A report changes nothing unless `slot < len(slots)` and `slots[slot].place == Some(p)` with `p.store` and `p.epoch` equal to the report's. A late report for store A cannot fence or clear store B at the same numbered epoch. The L8 lane edits these senders in place; the files themselves move in L9 and L13 as §3.5 places them.
2. **`ReportSettled` is validated, then takes one of two branches.** Validation needs, besides check 1: an `ActiveSettle` with `id == settle_id`, `active.slot == slot`, `active.store == p.store`, `active.epoch == p.epoch` and `!p.fenced`. A report that fails validation is stale and changes nothing: not the cursor, the record or any hold.
   - **Negative** (`!was_applied`): remove the `ActiveSettle` only. No token is unheld, because the claimant's H may still win.
   - **Applied:** remove the `ActiveSettle`, set `settled_from = max(settled_from, new_from_seq)`, and, if the register lane is live, `Unhold` exactly `active.tokens`: the fixed token set taken from the pre-settle register GET that issued this settle (`STORE-SPEC.md:4434-4435,4443`).
3. **A settle record lives only while its directive can still arrive.** Invariant: an `ActiveSettle(id)` exists only while `SlotSettle(id)` has been delivered or is its slot's `unsent` answer. `stream_answer` becomes a service operation (`stream_answer(mut s, slot, take answer)`). `ActiveSettle(id)` is retired, without unholding its tokens, when:
   - an unsent `SlotSettle(id)` is overwritten by a later answer (`store-stream-task.hirsel:326-336`);
   - delivery of `SlotSettle(id)` hits `Closed`;
   - the slot is reaped (`stream_reap_closed_slots`, `store-stream-poll.hirsel:244-254`, which clears `unsent` directly);
   - the slot is reallocated to a new place (`stream_new_slot`, `store-stream-task.hirsel:249-286`).

   After retirement the `(store, epoch)` is eligible again (`stream_has_active_settle`, `store-stream-poll.hirsel:351-363`). The next completed **register** GET issues a fresh settle with a new id and a token set taken from that GET (M1); a Place head GET carries no register token set (`store-stream-poll.hirsel:535-540`). A late report for the old id fails validation and is ignored. Flushing one slot on a later wake is not relied on to recover a discarded directive.
4. **Wake order:** each wake starts by flushing acks. The single service alarm is armed at the earliest idle, stall-poll or rule-(a) deadline.

---

## 7. `store.hirsel` by Declaration (Finding 9)

`store.hirsel` has 4,816 lines and 377 top-level declarations: 108 public and 269 private. The split is by declaration name. `store_split.py` checks that every private declaration appears exactly once.

### 7.1 Public declarations (108) — all stay in `"store"` (R1)

`StoreId`, `StorageKind`, `Caller`, `Limit`, `Why`, `Fault`, `StoreCapacity`, `Error`, `Db`, `Branch`, `CallId`, `CallInfo`, `Mark`, `Ticket`, `Read`, `StoreFailure`, `Outcome`, `Undone`, `Done`, `Bound`, `Limits`, `Resume`, `Page`, `Resumed`, `Tx`, `Snapshot`, `splitmix64`, `memory`, `fence_level`, `is_disk`, `identity`, `main_branch`, `snapshot`, `snapshot_of`, `branch`, `adopt`, `delete_branch`, `version`, `follow`, `Receipt`, `Stop`, `Plan`, `CallReserve`, `call_reserves`, `ServeError`, `Reserve`, `Pending`, `Served`, `next_call_info`, `stream_retention_ms`, `untried`, `serve`, `conclude`, `info`, `run`, `run_on`, `run_with_reply`, `durable`, `sync_notices`, `durable_now`, `body_capacity`, `RequestDigest`, `request_digest`, `Stored`, `RowSchemas`, `Readers`, `Table`, `IndexKind`, `Index`, `row_schemas`, `fetch_bytes`, `fetch`, `key_span`, `published_value`, `reads_key`, `system_table`, `table`, `put`, `delete`, `Looked`, `Seen`, `Change`, `changes`, `stage_export_migration`, `export_system_get`, `staged_snapshot`, `PreparedTarget`, `validate_preparation`, `write_prepared`, `ValidatedImport`, `validate_prepared`, `Imported`, `ImportWork`, `reserve_import`, `import_trees`, `imported_digest`, `prepared_matches`, `identity_words`, `Cutover`, `retained_main_after`, `open`, `open_read_only`, `checkpoint`, `CallRecord`, `StopKind`, `Recorded`, `recorded`, `law_call_reserve`.

Within `"store"` they may be distributed across `store.hirsel`, `store-api-types.hirsel` and `store-coordinators.hirsel` by declaration. No file exceeds its `tools/file_length_ratchet.json` entry, which is 5,153 lines for `store.hirsel`. `law_call_reserve` stays public at the root, allow-listed in `tools/check_boundaries.py`.

### 7.2 Private declarations (269)

| Destination | Count | Declarations |
|---|---|---|
| types (L1) | 7 | `same`, `make_id`, `capacity`, `copied`, `append_be`, `key_word`, `disk_identity` |
| format (L1) | 55 | `DISK_FORMAT`, `DiskContext`, `DiskFrame`, `DiskHeader`, `BINDING_BYTES`, `put_binding`, `disk_check_start`, `seal_frame`, `disk_check`, `disk_seal`, `disk_kind_limit`, `DiskFrameHead`, `disk_read_frame_head`, `disk_check_frame_kind`, `disk_read_frame`, `disk_frame_bytes`, `disk_header_bytes`, `disk_read_header`, `disk_number`, `disk_count`, `disk_name`, `disk_name_bytes`, `disk_put_name`, `encode_fork`, `disk_blob`, `le`, `blob`, `read_le`, `encode_data_op`, `encode_log_trim`, `encode_stream_op`, `stream_control_slot`, `encode_stream_ops`, `old_data_op_size`, `data_op_size`, `Settlement`, `CallSection`, `call_code_size`, `JournalTrim`, `JournalOp`, `JournalCommit`, `JournalFork`, `JournalEvent`, `journal_event_kind`, `journal_ops`, `decode_settlement`, `journal_decode_commit`, `journal_decode`, `journal_torn`, `journal_prefix`, `journal_read_prefix`, `JournalFrame`, `journal_read_frame`, `disk_stream_payload`, `disk_stream_payload_counted` |
| pages (L2) | 37 | `Roots`, `empty_roots`, `share_roots`, `Head`, `main_head`, `retaining`, `Tip`, `State`, `HolderDelta`, `main_state`, `tip_of`, `state_with_delta`, `prepare_holder_slot`, `install_head`, `remove_head`, `install_holder_delta`, `state_roots`, `state_account`, `branch_slot`, `branch_index`, `DataOp`, `StagedValue`, `stage_value`, `staged_bytes`, `DataRows`, `new_data_rows`, `data_row`, `push_data_row`, `DataOps`, `SCANNED_OPS`, `NO_FRAME_TAIL`, `OPEN_FRAME_TAIL`, `empty_ops`, `Staged`, `op_slot`, `branch_root_bytes`, `pin_root_bytes` |
| catalog (L3) | 28 | `decoded`, `encode_key`, `prefix_after`, `table_key`, `index_prefix`, `catalog_index_key`, `catalog_entry_in`, `stamp_after`, `define_store_collection`, `bulk_store_collection`, `remove_store_row`, `remove_store_collection`, `stage_create_table`, `stage_table`, `catalog_handle`, `catalog_index_handle`, `stage_drop_table_key`, `Built`, `over_ceiling`, `insert_row`, `migrate_build`, `same_bytes`, `catalog_insert`, `stage_migrate`, `catalog_recharge`, `prefix`, `prefixed`, `logical_key` |
| store (root, L3 ruling) | 7 | `PageRows`, `RowDecoder`, `row_value`, `typed_bound`, `index_decoder`, `index_bound`, `readers` |
| manifest (L4) | 7 | `PackDiskState`, `DiskLoaded`, `disk_publish_manifest`, `remember_roots`, `located_owners`, `manifest_state`, `check_expected_store` |
| journal (L6) | 26 | `Ack`, `disk_io`, `disk_path`, `disk_write_synced`, `disk_remove_optional`, `SegmentSpan`, `DiskRemovals`, `removal_spans`, `remove_numbered`, `remove_cut_files`, `disk_read_end`, `disk_end`, `disk_exists`, `DiskStartup`, `disk_start`, `DiskSession`, `disk_lock`, `disk_session`, `JournalChannels`, `journal_channels`, `disk_rotate`, `journal_scan`, `JournalReplay`, `journal_replay`, `journal_account`, `journal_following` |
| txn (L12) | 27 | `ServeReply`, `registered_completion`, `serve_before`, `serve_after`, `cause_number`, `save_reply`, `stage_reply`, `finish_served`, `Ran`, `Stage`, `Finished`, `Prepared`, `stage`, `stage_call`, `draft_body`, `finish_stage`, `reserve_ids`, `note_read`, `fetch_logical`, `digest_part`, `transaction_stream`, `newest_layer`, `writable`, `publish_row`, `check_table_write`, `call_checked`, `check_completion_record` |
| transfer (L13) | 20 | `PreparedMeta`, `CatalogChange`, `Inventory`, `PreparedRow`, `catalog_changes`, `RangeChange`, `changed_ranges`, `entries_of`, `checksum_row`, `range_inventory`, `ValidatedRange`, `read_valid_range`, `ImportedRange`, `matches_export`, `TargetLimits`, `catalog_bytes_equal`, `planned_catalog`, `planned_ranges`, `pack_leased`, `CALL_FIXED_STEPS` |
| store (root, live-Db orchestration) | 45 | `ProposedMain`, `current_state`, `memory_cut_account`, `record_memory_cut`, `head_snapshot`, `snapshot_from`, `call_info`, `memory_at`, `empty_db`, `disk_state`, `edit_disk`, `write_disk`, `journal_poison`, `same_name`, `checked_name`, `this_open`, `cut_install`, `check_manifest_addition`, `Publication`, `prepare_publication`, `publish`, `run_on_with_reply`, `store_body_on`, `store_body_at`, `append_transaction_at`, `handoff`, `maybe_checkpoint`, `wait_for_frame_room`, `durable_groups`, `covered`, `DiskState`, `open_number`, `healthy`, `open_heads`, `read_only_manifest`, `check_read_only_manifest`, `open_checks`, `open_started`, `disk_wait_cut`, `cut_io_error`, `PreparedDiskCut`, `CutTiming`, `disk_cut_timed`, `disk_cut_with_limit`, `db_target` |
| store (root, production closure of public `law_call_reserve`) | 5 | `test_db`, `typed_put`, `reserve_tx`, `reserve_pinned_data`, `reserve_settles_pin` |
| tests (store_law_schema.hirsel, [tests.sources] "store") | 4 | `bucket`, `OldAccount`, `NewAccount`, `upgrade` |

Notes:
- A private declaration that names a root type is rewritten in its lane over the internal representation (R2) before it moves. For example, `JournalCommit` carries a `format.CallRecordRep` and the root keeps public `CallRecord`.
- `call_info` builds the public `CallInfo`, so it stays at the root.
- `ProposedMain` carries `HistoryActivation`, so it stays at the root and `pages` gets resolved roots.
- Only the four test-only helpers move to `store_law_schema.hirsel` (`[tests.sources]`, module `"store"`). Public `law_call_reserve` (called by `runner-call-outcomes.hirsel:614`) is production code, so the helpers it reaches stay in production at the root; the `tools/check_boundaries.py` allow-list entry stays.
- §3.5 overrides rows of this table declaration by declaration (for example `disk_session` stays at the root and `JournalReplay` lands with `pager`).

---

## 8. Concurrent Lanes

| Lane | State at `ebcc4887` | Coordination |
|---|---|---|
| **3048** (running) | ClaimAuth, 49-byte `ORIGIN`, Format 11, `retirement_root` removal; **Lane 0 folded in** | **Hard predecessor of L1.** L1, L4, L7, L8 and L9 rebase onto the tags and layout 3048 lands. Keep authorization before TERM or Hold, and a fresh physical holder per open. L8 builds the `HoldGuard` around 3048's candidate builder. |
| **3075** (not landed) | v2.147 empty-commit skip | **Lands before L1** and before the L6, L8, L9 and L12 ticket and probe paths. Keep the no-mutation predicate, the prior ticket, the no-cut rule and the fresh head fence with a covered ticket. Use its landed head check. `Done.info` follows its landed type. |
| **3079** (landed `2d9e5e50`) | offline backup, check before stamp, writer fallback, listed-pack restore, damaged-backup laws | Placed as landed (§3.3, §5.3). Backup internals go to `capture` at L7; the public backup API stays at the root; `restore` stays public at the root with the listed-pack logic in `transfer` at L13. `store_backup_laws.hirsel:346,480` stay in `"store"`. |
| **3080** (landed `979875cf`) | Bucket request counters in `std.object` | No store move touches the counters. Keep the `bucket_request_counts` fixture exact when tests move. No lane adds a local capture-counter display. |
| **3082** (landed `f601b90a`) | NVMe pack cache | The `ObjectsCache` type goes to `pager` at L5, cache I/O to `history` at L7, and writer state (`cache`), trim and the pager worker loop to `object_writer` at L9 (§3.5, §4.5). Public `objects_cache` stays at the root. The cache stays disposable. |
| **3103** (design, STORE-SPEC v2.150) | write-path v2 | Coarse owners only: options and bytes in L1 (as root `Options` plus a `types` rep), page application in L2, Directory scheduling and acks in L6, transaction input in L12, and sizing at the root. The details are in 3103's DESIGN-v2.1.md §9 (amended by 3124, STORE-SPEC v2.153): the `ascending` flag as bit 63 of `DataRows.count` behind one accessor, kept by `push_data_row`, and prior-carrying `std.page.*_source` edits in L2; `DirJournalMsg.Flush` with the `2g` queue unchanged, and worker-loop deadlines in L6; `put_ordered` in `txn` (DESIGN-v2.2.md §7, STORE-SPEC v2.159: an opt-in checked batch, three validating passes with a private overlay then `put_impl` per row; not built after the L95 ship gate failed in v2.162; every page source entry builds `apply_sorted_batch`'s layout, and replay rejects a bad prior before any copy); no sizing change (0 B). The runtime's `channel.receive_until` is a prerequisite lane. This cutover adds none. |
| **3062 / 3076** (spec text) | v2.148 read path, v2.149 write path | Public `SharedCapture`, `RowView` and `Visit` are declared at the root. The L95 ship gate failed for `put_ordered` and `Error.OutOfOrder` (v2.162). Internals: shared backing in `capture` (L7), follower material in `object_writer` (L9), reader and visitors in `read` (L11), Tx range in `txn` (L12), and `DataRows.ascending` in `pages` (L2; it was `sorted` in v2.149). |
| **3081** | platform seams | `platform.local` is an external consumer of L8/L9. Keep its standalone and streamed cases. |
| `1b1fc30f`, `10d42c7e` (landed) | slot frame limits; recorded stream quotas | L8 carries the slot frame-limit pass-through. `check_stream_quota` (`store-streams.hirsel:19`) moves with `txn` (L12). Its callers `disk_start` (`store.hirsel:3631`) and `prepare_object_writer` (`store-object-writer.hirsel:210-212`) stay at the root (§3.5), so the check still runs before writer admission and no earlier lane calls into `txn`. |
| **`std.object` issue/finish** (new, v3.1) | prerequisite of L8 | Splits `store_if_retry` and `history_write_head` at the lane handoff (§6.2). No wire change. |

---

## 9. Tests and Law Obligations

### 9.1 Placement (`PROG-4`)

- A test assigned to module `M` sees only `M`'s private members.
- Mixed and end-to-end laws (`store_format12_laws`, `bucket_stream_claim_laws`, `BucketSim`/`Db` fixtures) stay in `"store"`.
- A pure single-module white-box assertion splits by declaration into a test file of the owning module, and its `[tests.runs]` key is updated.
- Literal corpus bytes stay literal.
- Exact admission numbers (`L90-Q4`, `L70`, `L85`, `L92`/`L93`) stay exact.

### 9.2 Obligations

| Obligation | Existing fixture extended | Bug caught (red outcome) | Lanes |
|---|---|---|---|
| **O1 — identity** | `tools/tests/wire_admission.py` `ENTRIES` (`:55-62`) and its `:181-187` placement check; the launch and slot-binding fixtures; dev client (`:526-537`); `core/programs/runner/steps_laws.hirsel:441-460` | Before L1, on the unchanged tree, the wire fixture registers `stream_q` with `runner.query` and `stamps_q` with `runner.query_as` (§2.4 items 2–3), and `ENTRIES` gains both literal keys with their real argument and result fingerprints. Hello sends them and asserts `('placed', 0, slot)`. The literal `schema` and `query_table` descriptor bytes (item 4) are compared where `launch_server`/`place_head` and `table_slot` consume them. **Red:** any of them answers `('placed', 2)` or its descriptor bytes differ. v2's L1 (moving `StreamId` to `format`) is red on `stream_q` while the Tx/Caller entries stay green. The steps law keeps replaying its old identity. | Every lane L1–L14 |
| **O2 — claimant** | `bucket_stream_claim_laws.hirsel:52-85,106-135` (Q4 and the L90 histories); `bucket_stream_unhold_laws.hirsel:412-422,531-563` (L90-X deaths) | Each case follows the original token at the real register key, with the claim client on a nonempty Place prefix. **(a) [3048]** Candidate-build failure after the final head check; count **all attempted** head PUTs. **Red:** an attempted H PUT, or no Abandon for the token. **(b) [L8]** Explicit pre-H abort through the guard. **Red:** no Abandon. **(c) [L8]** Ambiguous Hold that did install: the sim installs the row (the fixture's register read shows the token) and answers uncertainly; reconciliation then fails until the claim deadline. **Red:** the token is never abandoned and the row survives until rule (a). **(d) [L8]** Same-incarnation absence (3115 #1): the sim answers the Hold uncertainly, the reconcile GET shows the same incarnation without the token, then the delayed Hold lands. A second variant answers the Hold with a full register. **Red:** any TERM or H PUT attempted by this claimant. **(e) [L8]** Conflict restart within one open: H answers `Conflict`, the claim restarts. **Red:** no Abandon for the first token, or the restart does not reuse `granted`. **(f) [L8]** Cancellation before the handoff: the claim task is cancelled during a Place-head read (and, separately, while the H send waits on a full lane). **Red:** no finalizer Abandon for the token, an Abandon at any key other than the stream-root register key, an H PUT, or the token still present after cleanup. **(g) [L8]** Cancellation after the lane accepted H (the sim holds the accepted PUT). **Red:** any Abandon for that token. **(h) [L8]** `hold_begin` refused by a limit that cannot hold the cleanup scratch. **Red:** a Hold PUT is attempted. The existing L90-X histories gain only these assertions. | 3048 (a), L8 (a–h) |
| **O3 — paused worker** | `object_journal_laws.hirsel:107-130`; `bucket_stream_exit_laws.hirsel:108-124` | Drive a real settle on the handed-over session, three outcomes: **(i) resumed** (applied or not applied): the session is handed back and a later commit is acknowledged; **(ii) fenced**: the losing ticket gets `Fenced`, the earlier covered ticket keeps its ack, and the cutoff/fence verdict is as §5.2 specifies (no later ack is required); **(iii) cancelled**: the waiting task is cancelled, the worker returns its session and exits on the closed `resuming` channel (`store-object-writer.hirsel:419-433`, `store-object-journal.hirsel:498-508`), and joining the worker task returns. **Red:** no later ack in (i), a wrong verdict or a lost covered ack in (ii), a join that never returns in (iii). Termination is shown by the join, not by a health signal, which the worker emits only on `Error`. Prefix and uncovered-ticket assertions stay with their current owners (`bucket_stream_unhold_laws.hirsel:125-127`, `bucket_stream_janitor_laws.hirsel:386`). | L8, L9 |
| **O5 — settle validity** | `bucket_stream_service_laws`; white-box cases in a `host_stream` test file for forged reports | **(a)** Full answers channel: the journal worker is busy, the relay blocks, a rule-(a) `SlotSettle` stays unsent and a later poll overwrites it. **Red:** no fresh settle with a new id after the next register GET, so the dead claimant's row is never settled. **(b)** The late report for the overwritten id arrives. **Red:** any change to the cursor or holds. **(c)** The slot is reaped, and separately reallocated, while its settle is unsent. **Red:** the `(store, epoch)` never gets another settle. **(d)** A negative `ReportSettled`. **Red:** any token unheld. **(e)** A `ReportFenced` or `ReportDetached` for store A at epoch `e` delivered to a slot now holding store B at epoch `e`. **Red:** B fenced or cleared. | L8 |
| **O4 — boundary** | `core/tools/store_tests.py:113-140` refusal controls (`look_put_refused`, `look_info_refused`, `index_rows_escape_refused`, `index_rows_write_refused`, `completion_unreachable`, `view_tx_write_refused`); the literal format corpus | Keep every existing control with its diagnostic (`PROG-2` "`store@1/store/Completion` is private or opaque", `LANG-13` "scoped[view] fn results with anchors are reserved", `OWN-10` "`tx` is viewed", `OWN-5` "`tx` is not writable", `PROG-2` "`Tx` is opaque outside its module"). Each lane that introduces an internal state adds one control that constructs that state from a dependent package (e.g. `store.txn.TxState(..)` at L12, `store.read.ReaderState(..)` at L11, `store.capture.SharedState(..)` at L7). **Red:** the control compiles, or the diagnostic is not `PROG-2` "… is private or opaque". L11 also adds a `RowView` escape control (`LANG-13`). | L1–L14 at each new state type |

**Root surface gate (lane step, not a law):** L1's first commit adds `tools/store_public_contract.py`, which prints the full public contract of every `pub` declaration of modules `"store"` and `"status"`, sorted by name: kind and capability bounds, generic parameters and bounds, every parameter with its mode and type, the result with its anchors, `waits`, the finalizer and its grant, and the fields and variants of every non-opaque public type. Opaque types contribute their kind, bounds and finalizer only; their private backing is ignored, and so are file and line. Each lane runs it before and after; the outputs must be byte-identical. v3's `inventory.py` comparison printed only file, module, kind, visibility, name and line, so a changed parameter or public field passed it.

**Placement gate (lane step, not a law):** L1's first commit also adds `tools/store_placement.py`. It reads each declaration's module from the tree's real `hirsel.toml` file assignments (the module column of `inventory.py`). It then compares that module with `placement.tsv`, committed beside it, for every declaration whose landing is at or before the current lane. It fails on any mismatch, unless the declaration is listed in the lane's recorded lower-owner moves (§3.5). The build's own module-dependency check remains the acyclicity oracle; this gate keeps lanes from drifting from §3.5 on the way.

---

## 10. Cutover Sequence

Lanes land in order on top of each other. Each lane:
- updates `hirsel.toml`;
- regenerates `hirsel.laws` (`hirsel laws write core/programs/store --libraries core`);
- runs its laws, O1 and its O4 control;
- runs the placement gate and the root surface gate (§9.2);
- passes `python3 tools/buck2/fmt.py --check`.

**v3.1 placement.** Each lane's "Move" and "Stays" lists below are the file-level summary. The declaration-level owners are §3.5's, and where they differ §3.5 wins. Every lane is landable alone in order: no landing references a declaration that a later lane moves, and the computed placement needs no temporary root adapter. The lane rows name the §3.5 changes that resolve 3115's concrete edges.

**Predecessors:** 3048 (with Lane 0) and 3075 land before L1. The `std.object` issue/finish lane (§6.2) lands before L8.

**Reds that may ride along:** none in O1, O4 or the corpus. Ordinary law reds follow the landing policy. Durability laws block.

### L1 — `types`, `format`
- **Move.**
  - Private parts of `store-limits.hirsel` go to `types`, with `replay_within_limits` going to `format`.
  - `store-tiered-refusals.hirsel` privates go to `types`, except `store_has_tiered`, `require_tiered_storage` and `require_archive_authority`, which take `Db` and stay at the root (R3).
  - Private codec declarations go to `format`: `store-stream-frames.hirsel`, `store-tiered-frames.hirsel`, `parse_stream_key`, the wire half of `store-keyed-journal.hirsel`, the pure writer in `store-commit-wire.hirsel`, and `XIndex`/`XIndexEntry`/`stream_xindex_*`.
  - The §7.2 `types` and `format` rows move.
- **Stays.** `StreamId`, `StreamKey`, `keyed_time`, `caller_key`, `stream_key_bytes`, `stream_record_matches`, `ServeError`, `Served`, `Page`, `Resume`, `Done`, `Options` and every other public declaration stay. `store-stream-types.hirsel` stays whole at the root. No status move.
- **New internal reps.** `types.Failure`, `types.FaultRep`, `types.CapacityRep`, `types.LimitsRep`, `types.StoreKey`, `format.CallRecordRep`. The root conversion functions are total `take` moves.
- **v3.1.** L1 depends only on `status`, `types`, `format`, `core` and `std`. `stored_prior`, `DataOrder`, `DataOp`, `StagedValue`, `staged_bytes`, `word_be`, `CLAIMS`, `round8` and the other §3.5 "into `format`" declarations come down with it. `DataRows`, `data_row` and `data_order` stay with `pages` (L2). The frame writers that need ordering or page rows (`encode_frame`, `encode_staged`, `encode_data_ops`, `ordered_row`) land with `pager` at L5. `decode_keyed_edits`, which calls the L12 claim parser, stays at the root. `holders_tiered` and `tiered_collections` (over `Roots`) land with `pages`. `stream_record_matches`, `SUBJECT_MAX`, `CLAIM_DONE_MAX`, `FRAME_LIMIT`, `VALUE_LIMIT` and `default_limits` stay public at the root as adapters over bodies in `format`/`types` (A-split).
- **Laws.** `store_format12_laws` and the corpus, `store_size_laws`, `store_journal_laws`, `bucket_stream_laws`; O1 (adds the §2.4 keys as the lane's first commit, on the unchanged tree); O4.

### L2 — `pages`
- **Move.** `store-pages.hirsel`, `store-layout.hirsel` (except public `fixture_broken_handoff`), `store-tail.hirsel` (privates only; `read_tail`, `key_codec` and `Tail` stay at the root), and the §7.2 `pages` row. `Pin` stays where §3.4 puts it, with `store-projector` privates.
- **Data passed down.** Resolved roots only. `ProposedMain` stays at the root.
- **State.** `holders: pages.HolderTable` (8 fields, refreshed atomically). The `DataRows.ascending` hook exists for 3103.
- **v3.1.** `PageTree` names the tiered read inventory and the replay branch (`store-pages.hirsel:1-8`). Their closures are plain data (`TieredReadInventory`, `TieredPagerPack`, `PagePack`, `HistoryRef`; `ReplayBranch`, `ReplayOverlay`, `ReplayRow`, `ReplayValue`, `ReplayTrimEvidence`), so they come down into `pages` with the page tree. The plain holder pin (`Pin`, `PinOwner`), `Head`, `Tip` and `State` are in `pages` too, so `pages.HolderTable` names nothing above Layer 2. `HolderDelta`, whose `ReplaceMain` takes root `ProposedMain`, stays at the root. 3103's cells (the `DataRows` ascending flag, `push_data_row`, `op_slot`, `apply_collection_batch`) are fixed in `pages`. `data_order` reads `DataRows`, so it sits here beside it. 3103's DESIGN-v2.1 §9 named `format` for it, which would be an upward edge, and the predicate is unchanged.
- **Laws.** `store_pages_paged_laws`, `store_layout_laws`, `store_prior_laws`, `store_fork_laws`; O1; O4 (`HolderTable` control).

### L3 — `catalog`
- **Move.** Privates of `store-handle-check.hirsel`, `store-declare.hirsel` and `store-migration.hirsel`, and the §7.2 `catalog` row. Functions that take `Db` (`declaration_base`, `schema_plan`, `stage_migration_plan`) stay at the root.
- **State.** `schema: catalog.SchemaState` (`fence` with `propose_fence`/`restore_fence`, `next_table`). Root `Declare`/`StagedMigration` own the live `DeclaredProjector` and `IndexRegistration` lists. `catalog` returns a pure `SchemaPlan`.
- **v3.1.** `MigrationJob` and `prepare_migration` construct root `Migration` and invoke the application's descriptor, so they stay at the root; `catalog` returns the pure plan.
- **Laws.** `store_schema_laws`, `store_declare_laws`, `store_lookup_laws`, `store_law_schema`; O1 with the revision note (§2.3); O4 (`DeclareState` control).

### L4 — `manifest`
- **Move.** The 13 manifest, pack, location, cut, retirement, frees, lineage and scrub files (privates), and the §7.2 `manifest` row.
- **Data passed down.** `fill_cut_manifest`, `cut_participants`, `visit_cut_*`, `cut_manifest_size` and the lineage functions take a root-built `manifest.CutInput` (heads, pins, lease spans, limits).
- **Stays.** `disk_cut_state` and `begin_scrub` (public) stay at the root, and so does `raise_materialized_pin`. The pure `fill_cut_manifest` and `cut_participants` bodies use the root-built `manifest.CutInput`. `cut_retirement` (which builds `DiskRemovals`) lands with `journal` at L6. `ExportLease` and `pack_leased` come down into `manifest` here (§3.5). Page recovery returns neutral `LoadedPackInventory` over `PackObjectRef`.
- **State.** `packs: manifest.PackDiskState`.
- **Rebase** on 3048's Format 11.
- **Laws.** `store_manifest_page_laws`(`_2`), `store_locations_disk_laws`, `store_packs_disk_laws`, `store_scrub_laws`, `tiered_lineage_laws`; O1; O4.

### L5 — `pager`
- **Move.** The 25 `store-tiered*.hirsel` pager files (privates). `READER_PIN_MIN`, `Residency` and `set_residency` stay public at the root. The park records from `store-mux.hirsel` (`ParkEntry`, `ParkQueue`, `empty_park_queue`, `park_slot`, `revoke_entry`, `first_unsent`, `parked_in_flight`) also move.
- **Stays.** The residency functions that take `Db` (`publish_tiered_residency`, `admit_tiered_plan`, `start_/stop_residency_pager`, `residency_to_*`) and the tiered-publish orchestration (`stage_tiered_cut`, `drain_cut`, `install_tiered_holders`, `promote_cut_holder`, `compact_cut`) stay at the root (R3).
- **Data passed down.** `store-tiered-replay.hirsel` returns neutral `TieredManifestRestore`.
- **State.**
  - `pager: pager.PagerState` (`pager`, `pager_writer`, `park_notices`, `parked`, `writer_pin_bytes`);
  - `tiered: pager.DiskPagerState`.
  - Taking `park_notices` clears the receiver while the pager stays live (§4.3 row 9).
- **v3.1.** The `ObjectsCache` type comes down into `pager`; the pager worker loop (`start_tiered_pager`, `run_tiered_pager`), which calls the cache's directory and trim operations, lands with `object_writer` at L9 beside `pack_cache_trim` (§3.5), so L5 never reaches L7. 3082's hash-verify and refetch move unchanged. `JournalReplay` and `journal_replay` also land here.
- **Laws.** The tiered laws listed in v2; O1; O4.

### L6 — `journal`
- **Move.** Privates of `store-journal-worker.hirsel`, `store-journal-status.hirsel` and `store-journal-recover.hirsel` (including the `capture_*` fence functions), and the §7.2 `journal` row.
- **Stays.** `journal_receive`, `journal_ack`, `journal_poll*` and `close_ack` take `Db` and stay at the root until each is rewritten over `QueueAccounting`. `disk_session` and `disk_start` stay at the root (§3.5), so the `check_stream_quota` admission call stays in a root caller; `disk_lock` and the segment primitives move.
- **State.**
  - `retention: journal.RecordRetention`;
  - `journal.DirJournalState` (lock, dir, creation, offsets, `DirJournalMsg` queue, `DirAck`, `QueueAccounting`, written, synced, poison, pending, task).
- **Interim Object path.** The root's explicit `JournalSender = Dir(..) | Object(Sender[JournalMsg])` (§3.3) chooses the sender for each backend. The common `JournalMsg` carrying session, export and slot payloads stays at the root until L9.
- **Preserve.** The writer recovery rules of §5.3.1, including the incomplete-segment case and `LOCK` held through join. Timers and coalescing are left to 3103.
- **Laws.** `store_journal_laws`…`_7`, `store_disk_laws`(`_2`), `store_crash_laws`, `store_ace_crash_laws`, `store_replay_laws`, `store_sim_disk_laws`; O1; O4.

### L7 — `history` (7a), `capture` (7b)
- **7a.** History, register, authority and object-cut privates, plus the `store-object-cache.hirsel` privates §3.5 assigns to `history` (`OBJECTS_CACHE_DIR`, `pack_cache_directory`, `pack_cache_read`, `pack_cache_open` and the other cache I/O; `ObjectsCache` itself is in `pager` from L5). `objects_cache` stays public at the root. `EndpointRefusal` and `endpoint_refusal_text` stay at the root. From `store-object-open.hirsel`, only `HistoryOpenBase` moves here; `history_open_with`, `history_load_admitted`, `history_open_existing`, `history_rebase_suffix` and `history_open_register` stay at the root, because open dispatch reaches L8's `stream_open_attached`, which also stays at the root (§3.5). History consumes `pager.JournalReplay` (it lands with `pager`, whose replay code builds it; §3.5) and neutral pack inventory, including the cache and its caller (`store-history-packs.hirsel:104-122`). Keep 3082's hash-verify and refetch behaviour.
- **7b.** Capture privates, `store-inspect.hirsel` private constants to `maintenance` (L14), and `store-backup-offline.hirsel` privates (`backup_lock`, `copy_backup_journal`). Public `PreparedCapture`, `prepare`, `capture_need`, `BackupError`, `PreparedBackup`, `prepare_backup`, `backup_offline`, `backup_with_writer`, `catalog_entries`, `CheckTotals` and `check_capture` stay at the root. Keep the three live-capture branches (§5.3.2) and the `LOCK` transfer into `PreparedCapture` (§5.3.3). The 3062 shared backing is `capture.SharedState`.
- **Laws.** `authority_laws`, `bucket_laws`, `bucket_head_vector_laws`, `bucket_order_laws`, `bucket_q5_laws`, `object_cut_laws`, `location_laws`, `store_capture_laws`, `store_backup_laws` (3079, unchanged oracles), `store_crash_laws`; O1; O4 (`capture.SharedState` control).

### L8 — `host_stream`
- **Move.** Privates of the ten stream files. `slot_relay` and `SlotRelayInput` move to the root beside the current worker (they own `Sender[JournalMsg]` and build `StreamAnswer`). The public `JournalSlot`, `JournalService`, `StreamPlan` and the rest stay at the root.
- **Implement:**
  - `HoldGuard` and the token ownership rules (§6.2);
  - `ClaimMachine[T]` as a resource with the phases of §6.3, around 3048's builder;
  - report binding to the store id at every sender, the two `ReportSettled` branches, and settle retirement on replacement, closed send, reap and reuse (§6.4);
  - `SettleView` with `(sid, inc)` and the batch cursor, with reports delivered outside the view (§5.2);
  - plain `StreamFollowMaterial`;
  - the `1b1fc30f` frame-limit pass-through.
  - the guard's admission, cleanup scratch and stream-root addressing, and the handoff over the `std.object` issue/finish split (§6.2).
- **Laws.** `bucket_stream_laws`, `_service_`, `_claim_`, `_unhold_`, `_election_`, `_gc_`, `_janitor_`, `_direct_follow_`, `_exit_laws`; O2 (b–h); O3; O5; O1; O4.

### L9 — `object_writer` and `Backend`
- **Move.** Object open, journal, writer, reclaim, sim-levels, levels, health, followers, follower-service and object-follower privates. The slot relay moves from the root into `object_writer` with the object queue.
- **v3.1.** `history_check_open` moves here from `store-object-open.hirsel`; `prepare_object_writer` stays at the root with its `check_stream_quota` call.
- **Stays.** `open_object_writer`, `reclaim_preview` and every other writer function that takes `Db` (`object_writer_open`, `object_cut_state`, `object_cut_work`, `take_object_journal*` and so on) stay at the root. Public `Level`, `Position`, `durable_at`, `Refusal`, `open_authority`, `archive`, `fresh_snapshot`, `claim_of` and `Follower` stay at the root.
- **State.** Switch `DiskState` to `Backend` in one landing. Carry all 22 fields of `ObjectWriterState` (§4.5), including `cache` across construction and cut. Preserve 3081's platform cases and 3075's attached fresh probe.
- **Laws.** `object_journal_laws`, `store_reclaim_laws`, `follower_laws`, `follower_crash_laws`, `replication_laws`, `assembly_laws`; O3; O1; O4.

### L10 — `indexes`
- **Move.** `store-index-registry.hirsel` registry, catalog-record codec and pure planning privates, and `store-aggregate.hirsel` pure privates.
- **Stays.** `registered_index`, `probe_index_keys`, `apply_edit(s)`, `put_impl`, `delete_impl`, `plan_delete`, `apply_delete_plan`, `bound_indexes`, `preflight_index_edits`, `edit_stamp_cost`, `write_stamp_cost`, `append_index_remove`, `drift_stop`, `plan_index_put`, `plan_aggregate_edit` (Tx-bound) and the Tx-reading part of `totals` stay with `Tx` at the root until L12. Public `IndexState`, `Aggregate`, `aggregate` and `totals` stay at the root.
- **State.** `indexes: indexes.IndexOwner` (registry and `DriftRep` list) with owner install operations. Public `IndexState` and `IndexDrift` are built at the root from it.
- **Laws.** `store_lookup_laws`, `store_index_crash_laws`, `store_put_laws_2`; O1; O4.

### L11 — `read`
- **Move.** `store-read.hirsel`, `store-read-pages.hirsel` and `store-changes.hirsel` privates, over a neutral `read.ReaderInput`.
- **v3.1.** `next_index_page` calls L12's `tx_tiered_index_row`, so it lands with `txn` (§3.5).
- **Stays.** `check_table_read`, `open_range`, `open_range_bounds`, `check_index_read` and `open_lookup` (Tx-bound) stay with `Tx`. `staged_reader` and the prepared-export adapter stay at the root. `Reader`, `ReadOnly`, `RowView`, `Visit`, `SharedCapture` and all 38 public `store-read.hirsel` declarations stay at the root with R4's contracts. The index entry cursor of v2.155 (3101) follows the same split: its private `EntryCursor`, `step_entry`, `entry_parts`, `lends_codes` and `lends_i8_rows` move with the `read` privates, while `IndexEntries`, `I8Code`, `entries`, `next_entry` and `next_i8_entry` stay at the root.
- **Laws.** `store_read_laws`, `store_cursor_laws`, `store_changes_laws`, `store_changes_page_law`, `store_diff_laws`; O1; O4 (`ReaderState` control and `RowView` escape).

### L12 — `txn`
- **Move.** `store-tx.hirsel`, commit, attempts, the rest of mux, fork, cell, log, edge, streams (including `check_stream_quota`), keyed-claims, keyed-lifecycle, reexec, tiered-tx-read and the §7.2 `txn` row. Tx-bound functions are rewritten over `txn.TxState`. `available_resident`, `available_journal` and `draft_credit` take limits and accounting as arguments.
- **Stays.** Callback invocation over nominal `Tx`, `stage_call`'s use of the root callback, the pre-commit cut, the final handoff, `settle_admit`'s handoff and the commit functions that take `Db` (`commit_prepared`, `settle_draft`'s cut call, `prepare_run_commit` and so on) stay at the root. `settle_draft` returns `txn.CutDecision` to the root.
- **State.** `accounting: txn.AccountingState` (mutable `ceiling`) and `call_streams: txn.CallStreamState`.
- **Laws.** `store_put_laws`(`_2`), `store_tx_range_laws`, `cell_log_laws`, `store_claim_crash_laws`, `store_claim_quota_laws`, `store_reexec_laws`, `store_swarm_laws`, `store_trust_laws`; O1; O4 (`TxState` control).

### L13 — `transfer`
- **Move.** Export, object-export, object-import, prepared-check, catch-up, rounds, install and activation privates, and the §7.2 `transfer` row.
- **Stays.** The prepared `Db`, `begin_copy`, `open_copied`, `leased_step_base`, `record_leased_step`, `install_prepared`, `accept_cutover`, `perform_cutover_with`, `forget_import`, `install_activation_layout`, `publish_schema_plan*`, `staged_migration`, `validate_rewrite_base`, `import_pin_slot`, `orphan_import_pin`, `hold_import`, `install_account`, `history_import_base` and the projector install after publication stay at the root. `ExportStamp`, `Exported`, `ExportCopy`, `Preparation`, `ValidatedImport`, `Imported`, `ImportWork`, `PreparedTarget`, `Cutover` and `restore` stay public at the root.
- **State.** `transfer: transfer.TransferState`.
- **Laws.** `store_backup_laws`, `store_catchup_laws`, `store_crash_install_laws`, `import_read_laws`; O1 (`ExportStamp` nested key); O4.

### L14 — `maintenance` and root completion
- **Move.** Index-build, verify, expiry and projector privates that are not Db-bound, plus the `store-inspect.hirsel` private constants.
- **Stays.** `verify_release`, `repair_indexes`, `verify_step`'s live driver, the `Fold` descriptor invocation (R4), `DeclaredProjector` and every Db-bound projector function stay at the root. `maintenance` owns `IndexWalk` and the walk, fold and expiry pure work.
- **Finish.** Measure line counts and update `tools/file_length_ratchet.json` from the measured values.
- **Laws.** `store_projector_laws`, `store_index_crash_laws`, `store_trust_laws`; O1; O4; downstream `runner`, `platform`, `vector` and `search` builds.

---

## 11. Prior Art

Carried from v2; citations are against local clones in `/tmp/ref-*`.

| Peer Engine (Clone & Commit) | Boundary Architecture & Citations | Verdict | Wins, Weaknesses & How Hirsel Uses It |
|---|---|---|---|
| **redb** (`/tmp/ref-redb`, `2fc790d`) | `Database` (`src/db.rs:798-803`) is a 3-field composition root (`mem`, `transaction_tracker`, `closed`). Acyclic stack: `StorageBackend` → `page_store` (`TransactionalMemory`) → `tree_store` (`Btree` + `TableTree`) → `transactions` (`WriteTransaction`) → `db` façade. | **ADOPTED** | **Win:** Tiny composition root with single-owner subsystem handles. **Weakness:** `WriteTransaction` (`src/transactions.rs:935-957`) holds 18 raw fields. **Hirsel:** Adopts thin composition roots (`Db`/`DiskState`) while keeping `TxState`, `AccountingState`, and `CallStreamState` cleanly separated and pinning root nominal `PROG-9` types at Layer 12. |
| **fjall** (`/tmp/ref-fjall`, `8700604`) | Splits `DatabaseInner` (`src/db.rs:35-61`) from `SupervisorInner` (`src/supervisor.rs:16-36`), with dedicated `journal/`, `flush/`, `compaction/`, `keyspace/`, `poison.rs` (`src/poison.rs:8-41`). | **ADAPTED** | **Win:** Dedicated `journal/manager.rs` and `PoisonSignal` isolate WAL retention and poison state. **Weakness:** Mutual `Arc` cycles between `Supervisor` and `Keyspace` (`src/db.rs:82-93`). **Hirsel:** Enforces a strictly acyclic 14-layer DAG so cyclic ownership cannot form. |
| **SQLite** (`/tmp/ref-sqlite`, `795e28a`) | Strict opaque-header stack: VFS → WAL (`src/wal.h:53-75`) → Pager (`src/pager.h:12-43`) → B-Tree (`src/btree.h`) → VDBE (`src/vdbe.h`). | **ADOPTED (improved)** | **Win:** WAL/format changes stay isolated from B-Tree and execution. **Weakness:** `struct Pager` (`src/pager.c:619-654+`) grew to ~80 fields. **Hirsel:** Splits Pager into `manifest` (packs/locations), `pager` (Tiered cache/eviction), and `journal` (WAL segments) enforced at compile time via `pub opaque type` (`PROG-2`). |
| **TigerBeetle** (`/tmp/ref-tigerbeetle`, `a2a3c706`) | Separates VSR WAL (`src/vsr/journal.zig`), block Grid (`src/vsr/grid.zig:30-85`), LSM Forest/Manifest (`src/lsm/forest.zig:30-90`), and deterministic `StateMachineType(Storage)` (`src/state_machine.zig:223-258`) via capability injection. | **ADOPTED** | **Win:** Explicit capability injection enables deterministic simulation without passing storage-root god objects or re-entering workers. **Hirsel:** Uses explicit capability arguments in `host_stream` (the `HoldGuard` owns its client) and the non-reentrant `SettleView` (zero access to worker queues). |
| **FoundationDB** (`/tmp/ref-foundationdb`, `58f80b1`) | Separates durability log (`TLogServer`), commit proxy (`CommitProxyServer`), sequencer, and storage engine (`StorageServer`). | **ADAPTED** | **Win:** Orthogonal evolution of commit coordination, log durability, and storage layout. **Hirsel:** Preserves the same separation in-process via acyclic modules and a single foreground coordinator at the root. |
