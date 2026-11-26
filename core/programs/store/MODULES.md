# Hirsel Store Module Architecture (`store@1`, v2)

**Status:** Design only (no `.hirsel` source moved). Normative cutover brief for `core/programs/store/` (revised for Astra review `3085`, rebased onto `origin/main` `06ad1735`, integrating `perf-store-read-design-3062` (`e9ca4d65`, STORE-SPEC v2.148) and `store-write-path-design-3076` (`06ad1735`, STORE-SPEC v2.149)).  
**Supersedes:** `1f71060f` (`store-modules-design-3071`).  
**Wholehog principle:** Every cutover lane lands its final module seam and owned subsystem state in one pass, with a strictly acyclic module DAG at every intermediate commit, byte-identical `PROG-9` persisted identities, and zero dual-API shims.

---

## 1. Executive Summary

Today `core/programs/store/hirsel.toml` places 123 of its 125 production source files (49,000+ lines) in the single module `"store"` (only `store-health-types.hirsel` in `"status"` and `main.hirsel` in `"main"` sit outside it). As a result:

1. Every subsystem can read and mutate every other subsystem's private fields and invariants.
2. `Db` (`store.hirsel:40-92`, 39 fields) and `DiskState` (`store.hirsel:3695-3736`, 34 fields) are monolithic structs that interleave Directory (`log@1`) and Object (`objects@1`/`sim@1`) optional fields.
3. `store.hirsel` (4,846 lines) mixes leaf types, binary wire codecs, page-root holder management, schema catalog mechanics, Directory I/O and cuts, transaction execution, and export/import preparation.
4. The Bucket host-stream claim and service lifecycle (`§12.11`) threads 11–17 arguments across six files and has an existing pre-`H` abandon-bypass bug (`store-stream-claim.hirsel:585-606`).

The v2 end state partitions `core/programs/store` into **19 strictly acyclic modules across 14 layers (Layers 0–13)** with **zero same-layer edges above Layer 0**:

- **`Db`** shrinks from 39 fields to **13** (6 root/shared fields + 7 opaque subsystem owners).
- **`DiskState`** shrinks from 34 fields to **8** (3 root fields + 1 `RecordRetention` pair + 3 opaque subsystem owners + 1 `Backend = DirBackend(journal.DirJournalState) | ObjectBackend(object_writer.ObjectWriterState)` sum).
- **Root module `"store"`** stays split across focused files (`store.hirsel`, `store-public-types.hirsel`, `store-coordinators.hirsel`, `store-attention.hirsel`, `sizes.hirsel`), keeping every file under 1,000 lines while **preserving every persisted `PROG-9` nominal type path (`store@1/store/*`) byte-for-byte** (§2).
- **`host_stream`** encapsulates `§12.11` behind `ClaimMachine[T]` (owned per-phase payloads, `HSent` recorded before I/O, four distinct post-`H` outcomes), complete `RegisterLane` and `HoldTracker`, and a non-reentrant `SettleSession` incapable of calling back into a paused worker (§6).
- **Cutover sequence:** One tiny prerequisite bug-fix lane (**Lane 0**, closing the live pre-`H` abandon gap at `store-stream-claim.hirsel:585-606`) followed by **14 sequential, independently buildable lanes (L1–L14)** in true topological order (§9).

---

## 2. `PROG-9` Persisted Identity Contract (Critical Finding 1)

### 2.1 Why Public Aliases Break Persisted Keys

Under `PROG-9` (`docs/spec/programs.md:241-318`), `code.identity(code.describe(f))` encodes `0x01` followed by `D A S`, where every type path in generic arguments `A` and function header `S` is replaced by the **definition identity of the nominal type after expanding all `pub type` aliases** (`docs/spec/programs.md:277`). Moving the nominal declaration of `store.Tx` to `txn` and leaving `pub type Tx = txn.Tx` in `store` therefore changes **every application's** persisted Runner query/action key (`core/programs/runner/runner-query.hirsel:155`) from `store@1/store/Tx` to `store@1/txn/Tx`, even when the application source still spells `store.Tx`. Because `runner.Caller = store.Caller` (`core/programs/runner/runner.hirsel:61`), moving `Caller` similarly breaks every `runner.transact`, `runner.query_as`, and `runner.room` key. Because schemas (`fn(mut store.Declare) -> Result[H, store.Error]`), migrations (`fn(mut store.Migration) -> Result[(), store.Error] waits`), slot extractors (`fn(H) -> store.Table[K, V]` / `Cell[V]` / `Log[V]`), platform callbacks (`fn(store.ReadOnly, store.Caller) -> Result[bool, store.Error] waits`), and projector folds (`store.Fold`) are all passed to `code.describe`, moving any of their nominal declarations breaks persisted descriptors.

Hirsel has no nominal-identity-preserving re-export (`PROG-1`, `LANG-7`). Therefore **every identity-bearing nominal type MUST remain declared in module `"store"`** (in `store-public-types.hirsel`, assigned to `"store"` in `hirsel.toml`), with subsystem implementation state living behind it in lower modules.

### 2.2 Three-Tier Pinning Classification (Every Public Type Accounted For)

#### Tier A — Pinned Nominally at `store@1/store/<Name>` (Byte-Identical `PROG-9` Identity)

1. **26 Opaque and Scoped Handle Types** (zero-cost single-field wrappers in `store-public-types.hirsel`; zero heap allocations, zero copies, zero module cycles, preserves `OWN-20` root-scoped borrow anchors `in tx` / `in row`):
   - `Db` (root composition struct, stays in `store.hirsel`)
   - `Tx` (`pub opaque scoped type Tx { inner: txn.TxState }`)
   - `TxRows[K, V]` (`pub opaque scoped type TxRows[K, V] { inner: txn.TxRowsState[K, V] }`)
   - `Declare` (`pub opaque type Declare { inner: catalog.DeclareState }`)
   - `TableDecl[K, V]` (`pub opaque type TableDecl[K, V] { inner: catalog.TableDeclState[K, V] }`)
   - `Migration` (`pub opaque type Migration { inner: catalog.MigrationState }`)
   - `StagedMigration` (`pub opaque type StagedMigration { inner: catalog.StagedMigrationState }`)
   - `Table[K, V]` (`pub opaque type Table[K, V]: shareable + send { inner: catalog.TableHandle[K, V] }`)
   - `Index[J, K, V, P]` (`pub opaque type Index[J, K, V, P]: shareable + send { inner: catalog.IndexHandle[J, K, V, P] }`)
   - `Cell[V]` (`pub opaque type Cell[V]: shareable + send { inner: catalog.CellHandle[V] }`)
   - `Log[V]` (`pub opaque type Log[V]: shareable + send { inner: catalog.LogHandle[V] }`)
   - `Edge[A, B]` (`pub opaque type Edge[A, B]: shareable + send { inner: catalog.EdgeHandle[A, B] }`)
   - `Aggregate[G]` (`pub opaque type Aggregate[G]: shareable + send { inner: indexes.AggregateHandle[G] }`)
   - `Readers[V]` (`pub opaque type Readers[V]: shareable + send { inner: catalog.ReadersHandle[V] }`)
   - `Snapshot` (`pub opaque type Snapshot: shareable + send { inner: read.SnapshotState }`)
   - `Reader` (`pub opaque type Reader: shareable + send { inner: read.ReaderState }`)
   - `ReadOnly` (`pub opaque type ReadOnly { inner: read.ReadOnlyState }`)
   - `Follower` (`pub opaque type Follower { inner: object_writer.FollowerState }`)
   - `SharedCapture` *(3062 `e9ca4d65`)* (`pub opaque type SharedCapture: shareable + send + cross { inner: capture.SharedCaptureState }`)
   - `RowView[K, V]` *(3062 `e9ca4d65`)* (`pub opaque scoped type RowView[K, V] { inner: read.RowViewState[K, V] }`)
   - `Preparation` (`pub opaque type Preparation { inner: transfer.PreparationState }`)
   - `Pending` (`pub opaque type Pending { inner: txn.PendingState }`)
   - `Resume` (`pub opaque type Resume: send { inner: types.ResumeState }`)
   - `ValidatedImport`, `Imported`, `ImportWork` (`transfer.*State` wrappers)

2. **10 Transparent Identity-Bearing Value Types** (stay nominally declared in `store-public-types.hirsel`, keeping `store@1/store/<Name>` exact; lower modules use internal mirror types in `types` / `read` / `maintenance` with total zero-copy `take` conversions at the Layer 12 `store` boundary):
   - **`Error`** (`pub type Error: shareable + cross = ... | OutOfOrder` *(3076)* `| ... | Latched(status.StreamLatch)`): internal mirror `types.StoreError` in Layer 0; Layer 12 `to_public_error` / `from_public_error` moves payloads by `take` (zero-copy, exhaustive compiler check). Note: under `PROG-9` (`docs/spec/programs.md:306`), changing payload type paths inside `Error`'s variants does NOT change `code.identity` of functions naming `store.Error`, so `Error` variants may directly carry `types.Fault`, `types.StoreCapacity`, `types.Limit`, `types.Position`, `types.EndpointRefusal`, `types.HostStreamId`, `types.StreamRefusal`, `types.TieredRefusal`, and `status.StreamLatch` without nested conversions (and `LimitsDiffer(StoreLimits)` carries pinned `StoreLimits`).
   - **`Caller`** (`pub type Caller: codec + key + shareable + cross { issuer: u8, subject: Bytes }`): pinned because `runner.Caller = store.Caller` appears in every Runner action, `query_as`, room, and platform access callback. Internal mirror `types.CallerId` (zero-copy `take` move of `subject`).
   - **`StoreId`** (`pub type StoreId: codec + key { hi: u64, lo: u64 }`): `: codec + key`, usable as a Runner query/action arg or generic `StaticFn` type argument. Internal mirror `types.StoreKey` (two `u64`s).
   - **`Branch`** (`pub type Branch { store: StoreId, id: u32 }`): internal mirror `types.BranchRef` (three scalars).
   - **`StoreLimits`** (`pub type StoreLimits: codec + shareable + cross { frame: u64, value: u64, meta: u64 }`): `: codec`, usable in Runner args and `Error.LimitsDiffer`. Internal mirror `types.LimitsSpec` (three `u64`s).
   - **`Limits`** (`pub type Limits: shareable + send { rows: u64, byte_limit: u64 }`): appears directly in `store.Fold.merge` and `store.Fold.drop` `StaticFn` headers (`store-projector.hirsel:111-112`). Internal mirror `types.ScanLimits` (two `u64`s).
   - **`Delta[K, V]`**, **`Merge`**, **`Fold[E, K, V, A]`**: `Delta[K, V]` and `Merge` appear directly in `store.Fold` `StaticFn` headers (`store-projector.hirsel:110-112`). Internal mirrors `maintenance.ProjectorDelta[K, V]` and `maintenance.ProjectorMerge` (zero-copy move).
   - **`Visit`** *(3062 `e9ca4d65`)* (`pub type Visit: key = Continue | Stop`): internal mirror `read.VisitStep` (two-variant tag).

#### Tier B — Already Declared Outside `"store"` (Unchanged)

- `status.Health` (`store@1/status/Health`) and `status.StreamLatch` (`store@1/status/StreamLatch`) remain in `"status"` (`store-health-types.hirsel`), with existing root aliases `pub type Health = status.Health` and `pub type StreamLatch = status.StreamLatch` in `"store"` (duplicate alias in `store-stream-types.hirsel:17` deleted in L1).

#### Tier C — Non-Persisted Operational / Configuration Types (Moved to Owning Submodule + Root Alias)

Operational and configuration types audited across the repository (`core/programs/{runner,platform,fleet,vector,search,store-ops,store}`) to appear in **zero** `StaticFn` headers or persisted `code.identity` keys — including `Options`, `Overrides`, `Kind`, `Shortage`, `WriterPlan`, `PlacePlan`, `FollowerPlan`, `Fault`, `Why`, `Limit`, `StoreCapacity`, `TieredRefusal`, `EndpointRefusal`, `Position`, `HostStreamId`, `StreamField`, `StreamRefusal`, `ClaimAuth` *(3048)*, `HeadView`, `Claim`, `Scrub*`, `Check*`, `Describe*`, `Reclaim*`, `Walk*`, `DeployError`, `Cutover`, `Export*`, `ClaimMachine`, `ClaimConfig`, `ClaimPhase`, `ClaimStep` — move nominally to their owning module and are re-exported as `pub type` aliases in `"store"`. **Architectural rule:** No cutover or future lane may move a Tier A type out of `"store"`, and any future type placed in a persisted `StaticFn` must already be at its final module path before persisting keys.

### 2.3 Separate Contract for `code.revision` (`PROG-9` / `PROG-10`)

`code.revision(s)` (`docs/spec/programs.md:320`, `store-declare.hirsel:336`) hashes the checked IR of every function and codec reached by `s`. It is **explicitly distinct** from `code.identity(s)`:
- Pure index extractors (`fn(K, V) -> Result[List[J], Capacity]`) that do not call `store.*` helpers keep **both** `code.identity` and `code.revision` unchanged.
- Any versioned extractor or `StaticFn` whose body calls a `store.*` function whose lowered IR changes through a root forwarding body will get a new `code.revision` (causing `Migrate` to rebuild the versioned index and `Open` to report `Changed` per `§1.2`). Lanes that introduce root forwarders record this revision expectation explicitly rather than conflating it with `code.identity`.

### 2.4 Pre-Cutover Identity Byte Vectors (Cross-Build Oracle)

Every cutover lane that touches Tier A types or root signatures (specifically **L1, L3, L11, L12, L14**) must preserve exact byte equality against these pre-cutover `PROG-9` vectors (`docs/spec/programs.md:248-318`), frozen in a dedicated cross-build identity test before L1 lands:

1. **Runner application query (`runner.query`):**  
   `fn q(tx: store.Tx, h: AppHandles, take a: u64) -> Result[runner.Answer[u64], Capacity]` in `app@1/main`:  
   `0x01` + `app@1/main/q(store@1/store/Tx,app@1/main/AppHandles,take u64)->core@1/core/Result[runner@1/runner/Answer[u64],core@1/core/Capacity]`
2. **Runner `query_as` / action (`runner.transact`, expanding `runner.Caller`):**  
   `fn act(mut tx: store.Tx, h: AppHandles, c: runner.Caller, take a: u64) -> Result[u64, u32]` in `app@1/main`:  
   `0x01` + `app@1/main/act(mut store@1/store/Tx,app@1/main/AppHandles,store@1/store/Caller,take u64)->core@1/core/Result[u64,u32]`
3. **Schema declaration (`store.Declare` + `store.Error`):**  
   `fn schema(mut d: store.Declare) -> Result[AppHandles, store.Error]` in `app@1/main`:  
   `0x01` + `app@1/main/schema(mut store@1/store/Declare)->core@1/core/Result[app@1/main/AppHandles,store@1/store/Error]`
4. **Runner table slot selector (`store.Table`):**  
   `fn slot(h: AppHandles) -> store.Table[u64, u64]` in `app@1/main`:  
   `0x01` + `app@1/main/slot(app@1/main/AppHandles)->store@1/store/Table[u64,u64]`
5. **Generic instantiations (`store.StoreId` and `store.Tx` + `store.Table` + `store.Error`):**  
   - `fn id_key[T: key](x: T) -> T` at `store.StoreId`:  
     `0x01` + `app@1/main/id_key[store@1/store/StoreId]{key[store@1/store/StoreId]}($0)->$0`
   - `fn read_one[K, V](tx: store.Tx, t: store.Table[K, V]) -> Result[Option[V], store.Error]` at `[u64, u64]`:  
     `0x01` + `app@1/main/read_one[u64,u64](store@1/store/Tx,store@1/store/Table[$0,$1])->core@1/core/Result[core@1/core/Option[$1],store@1/store/Error]`

---

## 3. Strict Acyclic Module Map (19 Modules, 14 Layers, Zero Cycles)

### 3.1 Hirsel Naming and Opacity Rules Verified

- **Zero `LANG-7` root-member collisions:** No module name matches any lowercase `pub` function or constant in `"store"`.
- **Zero accidental `core`/`std` extensions:** No module name matches any `core` or `std` module.
- **Compile-time opacity (`PROG-2`):** Every owned subsystem state struct is `pub opaque type` in its owning module.

### 3.2 True Topological Layering (Layers 0 → 13)

Every call edge strictly descends in layer number (except `types -> status` within Layer 0). **There are zero lateral edges anywhere in Layers 1–13.** Note specifically that `pager` (Layer 4) sits strictly below `journal` (Layer 5), and `capture` sits in Layer 6 (Step 7) strictly above `catalog`, `manifest`, `pager`, and `journal`.

```text
Layer 0:    status  <--  types                      (health/stream status; leaf types, limits, StoreError, TieredRefusal)
               \          /
Layer 1:          format                            (ALL wire/frame codecs: journal, stream, XINDEX, ORIGIN, tiered, catalog)
                    |
Layer 2:          pages                             (PageTree, PageRoots, Pin, HolderTable, DataRows.sorted, pure appliers)
                 /     \
Layer 3:    catalog   manifest                      (pure schema/handles; pack layout, manifest validation/publish, scrub)
              |         |
Layer 4:   indexes    pager                         (sync index plans & aggregates; Tiered pager, cache, pure reads/folds)
              |         |
Layer 5:      |      journal                        (log@1 DirJournalState, DirJournalMsg+timers, recovery fences, Replay assembly)
              |       /   \
Layer 6:      |  capture  history                   (verified dir capture + 3062 SharedCapture; stateless objects@1 + HistoryOpenBase)
              |     |        |
Layer 7:      |     |   host_stream                 (§12.11 ClaimMachine[T], RegisterLane, HoldTracker, session-only settle, stream follow)
              |     |        |
Layer 8:      |     |  object_writer                (ObjectWriterState, ObjectJournalMsg, export fetch handler, reclaim, levels, followers)
              |      \      /
Layer 9:      |        read                         (SnapshotState, ReaderState, ReadOnlyState, cursors, changes, 3062 visitors & shared_read_only)
               \      /   |
Layer 10:        txn------+                         (TxState, AccountingState, CallStreamState, Tx range/visit_range, put_ordered, Tx tiered/index apply)
               /     \
Layer 11:  transfer  maintenance                    (export/import/catchup/activation; index-build/verify/repair/expiry/projectors)
               \     /
Layer 12:       store                               (Db/DiskState root, commit/cut/durable/seal coordinators, pinned PROG-9 nominal types, sizes, attention)
                  |
Layer 13:        main                               (empty benchmark/workload entry point; currently no calls to store)
```

### 3.3 Resolution of Every Existing Implementation Cycle (High Finding 3)

| Boundary Conflict (Review 3085) | Concrete Source Cycle Eliminated | Exact Division of Work in v2 |
|---|---|---|
| `status` vs `types` | `store-stream-types.hirsel:17` re-aliases `StreamLatch`; `Error` holds `EndpointRefusal` (`store-object-authority.hirsel:2`) and `Position` (`store-levels.hirsel:92`); `Pin` holds `Roots` (`store-projector.hirsel:849`); `store-limits.hirsel:70,98` reads `Db` and `JournalCommit`. | Keep `status.StreamLatch` unique in `store-health-types.hirsel`; drop the alias line when moving `store-stream-types.hirsel` to `status` (keep root alias in `store`). Move `EndpointRefusal` and `Position` into `types` (L0). Move `PinOwner` to `types` (L0) and `Pin` (with `Roots`) to `pages` (L2). Split `store-limits.hirsel`: pure limits/checks → `types` (L0), `replay_within_limits` → `format` (L1), `store_limits(db)` → `store` (L12). |
| `format` vs `pages` / `pager` | `store-commit-wire.hirsel:224,245` reads `DataRows`, `PageTree`, `TieredDataDelta`, calls `collection_kind`/`get_page_row`; `empty_roots` (`store.hirsel:30`) builds `PageRoots`; `Tx` (`store.hirsel:513`) sat inside 3071's `pages` line range. | `format` (L1) exposes pure byte-level `encode_data_ops_resolved(mut payload, count, emit_row)` taking resolved `(key, value_opt, prior_opt, trim_opt)` via a scoped row emitter (zero second edit list — also aligns with 3076 Perf #10). Move `data_order` and prior resolution above `format` into `txn` (L10). Move `empty_roots`/`share_roots` (`L28-L36`) and `DataRows` (`L411-L512`) to `pages` (L2); split `Tx` (`L513-L560`) to `txn`/`store` at exact declaration boundaries. |
| `catalog` vs `indexes` / `object_writer` / `transfer` / `store` | `seal` (`store-declare.hirsel:675`) installs index/projector bindings onto `Db`; `declare_follower` (`:185`) takes `Follower`; `declare_export` (`:214`) takes `Preparation`; `store-index-registry.hirsel:138,579` borrows/mutates `Tx`. | `catalog` (L3) holds only pure schema/handle/plan logic (`declare_from_base`, `complete_declaration`). Move `declare`/`seal`/`declare_migration` orchestration to `store` (L12), `declare_follower` to `object_writer` (L8), `declare_export`/`finish_export_declaration` to `transfer` (L11). `indexes` (L4) produces pure `List[IndexEdit]` plans; move `Tx` edit application (`apply_edits`) to `txn` (L10). |
| `manifest` vs `pager` / `journal` / `history` / `txn` | `disk_cut_state` (`store-cut-participants.hirsel:168`) calls `disk_cut_timed`; `rebuild_page_store` (`store-page-recovery.hirsel:73`) takes `HistoryLocator` and returns `TieredReadInventory`; `journal_replay_manifest` (`store-tiered-replay.hirsel:13`) builds `JournalReplay`; `store-tiered-tx-read.hirsel:23` reads `Tx`; `disk_cut_with_limit` (`store.hirsel:4015,4031`) dispatches to `object_writer` and calls `durable_groups`. | Keep cross-backend cut dispatch (`disk_cut_state`/`disk_cut_with_limit`) at `store` (L12). Give `manifest` (L3) neutral `PackObjectRef` / `LoadedPackInventory` (no `HistoryLocator` or `TieredReadInventory`). `pager` (L4) wraps neutral inventory into `TieredReadInventory` and returns neutral `TieredManifestRestore`. `journal` (L5, strictly above `pager`!) assembles `JournalReplay`. Move `Tx` tiered read adapters (`store-tiered-tx-read.hirsel`) to `txn` (L10). |
| `capture` vs `maintenance` | 3071 scheduled `capture` in L2 though `store-capture-journal.hirsel:105` needs `catalog`, `manifest`, `pager`, and `journal`; `store-inspect.hirsel:97,120` (`verify_release`, `repair_indexes`) drives `maintenance` walkers on a live `Db`. | Defer `capture` to **Step 7 (Layer 6)** strictly above `catalog`, `manifest`, `pager`, and `journal`. Keep offline capture (`store-capture*.hirsel`, 3062 `SharedCaptureState`/`open_shared`) and pure snapshot `catalog_entries` in `capture`. Move `verify_release` and `repair_indexes` (`store-inspect.hirsel:73-176`) to `maintenance` (L11). |
| `history` vs `host_stream` vs `object_writer` | `ObjectPackRead` (`store-history-packs.hirsel:97`) holds `HistoryOpenBase` (`store-object-open.hirsel:1`); `stream_follow_make_capture` (`store-stream-follow.hirsel:141`) builds `FollowCapture` (`store-followers.hirsel:15`); `stream_xindex_encode` (`store-stream-gc.hirsel:120`) sits in `host_stream`. | Move `HistoryOpenBase`, `history_load_admitted`, `history_check_open`, and `history_open_with` into `history` (L6). Change `host_stream` (L7) `stream_follow_*` to return plain `StreamFollowMaterial`; `object_writer` (L8) builds `Arc[FollowCapture]`. Move `stream_xindex_encode`/`decode` into `format` (L1). |
| `object_writer` / `transfer` / `read` / `txn` / `store` | `ObjectExportRead` (`store-object-journal.hirsel:493`) calls `object_export_read` (`store-object-export.hirsel:14,178`); `follower_snapshot` (`store-followers.hirsel:350`) builds `read.Snapshot`; `durable_at` (`store-levels.hirsel:34`) calls `durable`; `read.range` (`store-read.hirsel:466`) borrows `Tx`; `maybe_checkpoint` (`store.hirsel:1155`) calls `cut_install`. | Move `ObjectExportLease`, `ObjectExportFetch`, and `object_export_read` into `object_writer` (L8). `object_writer` exposes `follower_tip_data` to `read` (L9), which builds `SnapshotState` and 3062 `follower_share`. Keep common `durable`/`durable_at`/`maybe_checkpoint`/`cut_install` dispatch at `store` (L12). Move `Tx` range/count adapters to `txn` (L10) and keep root `pub fn range(...) in tx` in `store` (L12). |

### 3.4 Complete Module Directory (All 125 Existing Production Files + Split Files)

Every production source file in `core/programs/store/hirsel.toml` is assigned to exactly one module below. Placements for **3062 (`e9ca4d65`, v2.148)** and **3076 (`06ad1735`, v2.149)** are explicitly integrated into their v2 owners.

| # | Module | Layer | Assigned Files (125 existing + split files) | Allowed Direct Callees | Narrow Internal / Public Surface |
|---|---|---|---|---|---|
| 1 | `status` | 0 | `store-health-types.hirsel`, `store-stream-types.hirsel` *(minus duplicate `StreamLatch` alias)* | *(none)* | `Health`, `StreamLatch`, `HostStreamId`, `StreamField`, `StreamRefusal`, `stream_refusal_text`, `stream_latch_text` |
| 2 | `types` | 0 | `store-limits.hirsel` *(pure limits/checks)*, `store-tiered-refusals.hirsel`, `store-types.hirsel` *(split)* | `status` | `StoreKey`, `CallerId`, `BranchRef`, `LimitsSpec`, `ScanLimits`, `StoreError` *(incl. 3076 `OutOfOrder`)*, `EndpointRefusal`, `Position`, `TieredRefusal` *(incl. 3062 `Shared \| Scan`)*, `PinOwner`, `Limit`, `Why`, `Fault`, `StoreCapacity`, `StorageKind`, `CallId`, `CallInfo`, `Mark`, `Ticket`, `Read`, `StoreFailure`, `Outcome`, `Undone`, `Done`, `Bound`, `ResumeState`, `Page`, `Resumed`, `Looked`, `Seen`, `Change`, `CallRecord`, `StopKind`, `Recorded`, `Plan`, `CallReserve`, `ServeError`, `Reserve`, `Receipt`, `Stop`, `RequestDigest`, `Stored`, `RowSchemas`, `IndexKind`, `IndexState`, `Options` *(incl. 3076 `group_delay_us`, `end_groups`, `end_interval_ms`)*, `Overrides`, `Kind`, `Shortage`, `ClaimAuth` *(3048)*, `HeadView`, `Claim`, pure limit & allocation helpers (`FRAME_LIMIT`, `VALUE_LIMIT`, `META_LIMIT`, `PAGE_LIMIT`, `BLOB_LIMIT`, `KEY_LIMIT`, `MIB`, `disk_fault`, `disk_allocation`, `capacity`, `raised_capacity`, `same`, `named`, `round8`, `splitmix64`, `call_info`, `make_id`, `check_limits`, `check_named_limits`, `journal_floor`, `open_checks`) |
| 3 | `format` | 1 | `store-format.hirsel` *(split)*, `store-commit-wire.hirsel` *(pure byte writer)*, `store-stream-frames.hirsel` *(+ `stream_xindex_encode`/`decode` from `store-stream-gc`)*, `store-tiered-frames.hirsel`, `store-stream-key.hirsel`, `store-keyed-journal.hirsel` *(L1–L112 wire half)* | `status`, `types` | `DISK_FORMAT` *(10 → 11 per 3048)*, `BINDING_BYTES`, `DiskContext`, `DiskFrame`, `DiskHeader`, `DiskFrameHead`, `seal_frame`, `disk_read_frame*`, `disk_header_bytes`, `disk_read_header`, `encode_data_op`, `encode_data_ops_resolved`, `encode_log_trim`, `encode_stream_op*`, `encode_fork`, `encode_call*`, `encode_keyed_edits`, `decode_keyed_edits`, `JournalEvent`, `JournalCommit`, `JournalFork`, `JournalOp`, `JournalTrim`, `journal_decode*`, `journal_torn`, `replay_within_limits`, `history_append_header`, `history_seal`, `history_frame`, `OriginFrame` *(3048)*, `Attach`, `SealedTail`, `StreamRef`, `StreamRegister`, `RegRow`, `XIndex`, `BatchSlice`, `BatchRow`, `stream_*_encode`/`decode`, `TieredFrame`, `read_tiered_frame`, `StreamId`, `StreamKey`, `caller_key`, `stream_key_bytes`, `parse_stream_key`, `Catalog` wire codec |
| 4 | `pages` | 2 | `store-pages.hirsel`, `store-layout.hirsel`, `store-tail.hirsel`, `store-roots.hirsel` *(split: `L28-L36`, `L99-L323`, `L411-L512` + `Pin` from `store-projector:849`)*, `store-keyed-pages.hirsel` *(L114–L166 of `store-keyed-journal`)* | `types`, `format` | `PageTree`, `PageRoots`, `Roots`, `Pin`, `Head`, `Tip`, `State`, `ProposedMain`, `HolderDelta`, `HolderTable` *(opaque)*, `DataOp`, `StagedValue`, `DataRows` *(+ 3076 `sorted: bool`, binary-search `op_slot`)*, `DataOps`, `Staged`, `TailBases`, `CollectionCursor`, `CollectionDescriptor`, `PageRange`, `empty_roots`, `share_roots`, `put_page_row`, `remove_page_row`, `bulk_collection`, `drop_collection`, `canonical_roots`, `apply_cell`, `apply_log_append`, `trim_log_page`, `materialize_keyed_edits`, `apply_collection_batch` *(3076: dispatches to `std.page.{bulk_from_source, append_sorted_source, apply_sorted_source}`)*, `state_with_delta`, `install_holder_delta`, `read_tail` |
| 5 | `catalog` | 3 | `store-catalog.hirsel` *(split)*, `store-handle-check.hirsel`, `store-declare.hirsel` *(pure schema/declare)*, `store-migration.hirsel` *(pure migration)* | `types`, `format`, `pages` | `Catalog`, `CatalogRecord`, `ValueSchema`, `IndexRecord`, `ReadersHandle[V]` *(opaque)*, `TableHandle[K, V]` *(opaque)*, `IndexHandle[J, K, V, P]` *(opaque)*, `CellHandle[V]` *(opaque)*, `LogHandle[V]` *(opaque)*, `EdgeHandle[A, B]` *(opaque)*, `SchemaState` *(opaque, with `propose_fence`/`restore_fence`)*, `SchemaPlan`, `DeclareState` *(opaque)*, `Mode`, `TableDeclState[K, V]` *(opaque)*, `MigrationState` *(opaque)*, `StagedMigrationState` *(opaque)*, `CollectionKind`, `CollectionInfo`, `declare_from_base`, `complete_declaration`, `catalog_handle`, `catalog_index_handle`, `readers`, `row_value`, `stage_create_table`, `stage_table`, `stage_drop_table_key`, `stage_migrate`, handle validity checks |
| 6 | `manifest` | 3 | `store-manifest.hirsel`, `store-manifest-wire.hirsel`, `store-list-read.hirsel`, `store-page-recovery.hirsel` *(neutral inventory)*, `store-locations.hirsel`, `store-packs.hirsel`, `store-pack-index.hirsel`, `store-cut-participants.hirsel` *(minus `disk_cut_state`)*, `store-cut-retirement.hirsel`, `store-tiered-frees.hirsel`, `store-tiered-lineage.hirsel`, `store-scrub.hirsel`, `store-scrub-pace.hirsel` | `types`, `format`, `pages` | `PackDiskState` *(opaque)*, `PageDisk`, `PageManifest`, `ValidatedPageManifest`, `LoadedPageStore`, `PackObjectRef`, `LoadedPackInventory`, `PageLocations`, `PagedLists`, `CutParticipants`, `PagedCutLayout`, `PackRun`, `DiskIndex`, `Lineage`, `DeferredFree`, `Scrub`, `ScrubConfig`, `ScrubRound`, `disk_read_page_manifest`, `publish_page_manifest_bytes`, `plan_page_cut`, `cut_retirement`, `attach_page_graph`, `validate_directory_graph`, `begin_scrub`, `scrub_step`, `scrub_pacer_*`, `record_scrub_round` |
| 7 | `indexes` | 4 | `store-index-registry.hirsel` *(pure registry/planning, minus `Tx` `apply_edits`)*, `store-aggregate.hirsel` | `types`, `format`, `pages`, `catalog` | `IndexState` *(opaque)*, `IndexRegistry`, `IndexRegistration`, `IndexEdit`, `IndexDrift`, `IndexReport`, `IndexValue`, `IndexStop`, `AggregateHandle[G]` *(opaque)*, `Totals`, `empty_index_state`, `install_registry`, `plan_put_edits`, `plan_delete_edits`, `record_drift`, `demote_drifted`, `aggregate`, `totals` |
| 8 | `pager` | 4 | `store-tiered.hirsel`, `store-tiered-packs.hirsel`, `store-tiered-ranges.hirsel`, `store-tiered-cache.hirsel`, `store-tiered-pager.hirsel`, `store-tiered-pager-batch.hirsel`, `store-tiered-attempt.hirsel`, `store-tiered-plan.hirsel`, `store-tiered-point-read.hirsel`, `store-tiered-count.hirsel`, `store-tiered-scan.hirsel`, `store-tiered-span.hirsel`, `store-tiered-initial-changes.hirsel`, `store-tiered-changes.hirsel`, `store-tiered-pins.hirsel`, `store-tiered-overlay.hirsel`, `store-tiered-replay.hirsel` *(returns neutral `TieredManifestRestore`)*, `store-tiered-fold.hirsel`, `store-tiered-emission.hirsel`, `store-tiered-publish.hirsel`, `store-tiered-probe.hirsel`, `store-tiered-probe-index.hirsel`, `store-tiered-promote.hirsel`, `store-tiered-compact.hirsel`, `store-tiered-residency.hirsel` | `types`, `format`, `pages`, `manifest` | `PagerState` *(opaque)*, `DiskPagerState` *(opaque)*, `TieredPagerMessage`, `TieredPagerLink`, `TieredPins`, `TieredTouchSet`, `TieredTarget`, `TieredDataDelta`, `TieredDirty`, `TieredCutStage`, `TieredReadInventory`, `TieredManifestRestore`, `TieredCompaction`, `start_pager`, `stop_pager`, `park_notices`, `park_enqueue`/`poll`/`cancel`, `read_tiered_point*`, `tiered_scan*`, `tiered_count*`, `tiered_changes`, `tiered_fold_cut`, `tiered_publish_cut`, `tiered_compact_cut`, `plan_residency_move` |
| 9 | `journal` | 5 | `store-journal-worker.hirsel`, `store-journal-status.hirsel` *(dir + common queue helpers)*, `store-journal-recover.hirsel` *(+ `JournalReplay` assembly)*, `store-journal-io.hirsel` *(split)* | `types`, `format`, `pages`, `manifest`, `pager` | `DirJournalState` *(opaque, owns `lock: HostFile`, `d: Dir`, `file_seq`, `file_end`, `creation`, `coalesce_timer_task` & `end_timer_task` per 3076)*, `DirJournalMsg` *(incl. 3076 `CoalesceTick`, `EndTick`, `Close`)*, `DirAck`, `WriteNotice`, `RecordRetention`, `QueueAccounting`, `JournalInput`, `JournalReplay`, `DiskSession`, `DiskStartup`, `disk_session`, `disk_recover_locked` *(3079)*, `start_dir_journal`, `dir_handoff`, `dir_poll_acks`, `dir_wait_ack`, `dir_rotate`, `dir_remove_retired`, `journal_scan`, `journal_apply*`, `journal_following`, `read_recorded_dir`, `close_dir_journal` |
| 10 | `capture` | 6 | `store-capture.hirsel`, `store-capture-check.hirsel`, `store-capture-check-journal.hirsel`, `store-capture-journal.hirsel`, `store-capture-pages.hirsel`, `store-capture-catalog.hirsel`, `store-capture-describe.hirsel`, `store-inspect.hirsel` *(L1–L71 `catalog_entries` only)* | `types`, `format`, `pages`, `catalog`, `manifest`, `pager`, `journal` | `CapturedState`, **`SharedCaptureState` / `SharedBacking` / `SlabNode` / `open_shared` / `share_capture` / `shared_version` / `shared_backing_bytes` / `shared_resident_bytes` / `freeze_roots_to_shared` / `shared_visit_leaves` *(3062 `e9ca4d65`)*,** `CheckTotals`, `DescribeEntry`, `DescribeLine`, `EntryForm`, `CatalogEntry`, `capture_directory`, `check_capture`, `describe_capture`, `capture_need`, `planned_capture`, `catalog_entries` |
| 11 | `history` | 6 | `store-history.hirsel`, `store-history-read.hirsel`, `store-history-witness.hirsel`, `store-history-packs.hirsel`, `store-register.hirsel`, `store-object-authority.hirsel` *(minus `EndpointRefusal`)*, `store-object-cut.hirsel`, `store-history-open.hirsel` *(split from `store-object-open` L1–L100: `HistoryOpenBase`, `history_load_admitted`, `history_check_open`)* | `status`, `types`, `format`, `pages`, `manifest`, `journal` | `HistoryHead`, `HistoryPair`, `HistoryRef`, `HistoryCapture`, `HistoryConfig`, `HistoryLocator`, `HistoryRegister`, `HistoryBuffers`, `HistoryOpenBase`, `HistoryWrite`, `HistoryClaimResult`, `HistoryActivation`, `ObjectPackRead`, `history_fetch`, `history_write_head`, `history_check_open`, `history_check_endpoint`, `history_check_conditions`, `history_register_publish`, `history_replace_claim`, `history_publish_genesis`, `history_publish_checkpoint`, `history_load_admitted`, `history_write_page_cut`, `history_validate_manifest` *(+ 3082 pack cache hash-verify)* |
| 12 | `host_stream` | 7 | `store-stream-claim.hirsel`, `store-stream-service.hirsel`, `store-stream-slot.hirsel`, `store-stream-task.hirsel`, `store-stream-poll.hirsel`, `store-stream-settle.hirsel`, `store-stream-gc.hirsel` *(minus `stream_xindex_encode`)*, `store-stream-janitor.hirsel`, `store-stream-follow.hirsel`, `store-stream-archive.hirsel` | `status`, `types`, `format`, `pages`, `manifest`, `journal`, `history` | **Claim-Machine API (§6):** `ClaimMachine[T]` *(opaque)*, `ClaimPhase[T]`, `ClaimConfig`, `ClaimStep[T]`, `ClaimedPlace[T]`, `claim_new`, `claim_step`, `claim_run`, `claim_abort`; `RegisterLane` *(opaque, incl. `pending` & `stalled`)*; `HoldTracker` *(opaque, full service state)*; `JournalService` *(opaque)*, `JournalSlot` *(opaque)*, `SlotLink`, `SlotAck`, `SlotReport`, `StreamPlan`, `StreamLimits`, `Batching`, `StreamRoot`, **`SettleSession`** *(non-reentrant direct I/O view)*, `SettleOutcome`, `StreamFollowMaterial`, `stream_service_start`, `stream_slot_open`, `stream_settle`, `stream_settle_sealed`, `stream_poll_head`, `stream_follow_capture`, `stream_janitor_step` |
| 13 | `object_writer` | 8 | `store-object-open.hirsel` *(writer startup glue)*, `store-object-journal.hirsel` *(+ `ObjectExportLease`/`Fetch`/`object_export_read` from `store-object-export`)*, `store-object-writer.hirsel`, `store-reclaim.hirsel`, `store-sim-levels.hirsel`, `store-levels.hirsel` *(minus `Position` and root `durable_at`)*, `store-health.hirsel`, `store-followers.hirsel` *(minus `read.Snapshot` constructor)*, `store-follower-service.hirsel`, `store-object-follower.hirsel` | `status`, `types`, `format`, `pages`, `catalog`, `manifest`, `pager`, `journal`, `capture`, `history`, `host_stream` | `ObjectWriterState` *(opaque)*, `ObjectJournalMsg`, `ObjectAck`, `ObjectJournalSession`, `ObjectExportLease`, `ObjectExportFetch`, `Authority`, `Refusal`, `Level`, `SimLevels`, `LevelWorld`, `LevelSync`, `FollowerState` *(opaque)*, `Followers`, `FollowerService`, `FollowerOwner`, `FollowerSlot`, `FollowerTipData`, `open_object_writer`, `object_cut_publish`, `object_stream_close`, `object_cut_close`, `object_fresh_probe`, `object_export_fetch`, `object_handoff`, `object_poll_acks`, `object_wait_ack`, `weak_wait_level`, `follower_tip_data`, `declare_follower_state`, `health`, `on_unhealthy`, `claim_of`, `reclaim_preview`, `reclaim_purge`, `streamed`, `stream_reader`, `follower`, `fresh`, `at_least`, `follower_service` |
| 14 | `read` | 9 | `store-read.hirsel` *(minus `Tx` `range`/`count`)*, `store-read-pages.hirsel`, `store-changes.hirsel` | `types`, `format`, `pages`, `catalog`, `manifest`, `pager`, `capture`, `object_writer` | `SnapshotState` *(opaque)*, `ReaderState` *(opaque)*, `ReadOnlyState` *(opaque)*, `AnonymousCursor`, `Changes`, `ChangePage`, `SpanRows`, `Rows`, `IndexRows`, `IndexEntries`, `I8Entries`, `reader`, `follower_reader`, `follower_snapshot`, **`shared_read_only`, `follower_share`, `VisitStep`, `RowViewState[K, V]`, `visit_scan`, `visit_scan_reverse`, `row_*` *(3062 `e9ca4d65`)*,** `read_only_reader`, `staged_reader`, `get`, `fetch_many`, `scan`, `scan_reverse`, `scan_page`, `scan_next`, `find`, `find_page`, `find_next`, `count_at`, `changes`, `changes_page`, `open_span_changes`, `next_span_change` |
| 15 | `txn` | 10 | `store-txn-core.hirsel` *(split)*, `store-tx.hirsel`, `store-commit.hirsel`, `store-attempts.hirsel`, `store-mux.hirsel`, `store-fork.hirsel`, `store-cell.hirsel`, `store-log.hirsel`, `store-edge.hirsel`, `store-streams.hirsel`, `store-keyed-claims.hirsel` *(+ L168–L182 of `store-keyed-journal`)*, `store-keyed-lifecycle.hirsel`, `store-reexec.hirsel`, `store-tiered-tx-read.hirsel` *(`Tx` tiered adapters)*, `store-tx-range.hirsel` *(split from `store-read` L466–L560 + `store-index-registry` `apply_edits`)* | `status`, `types`, `format`, `pages`, `catalog`, `manifest`, `pager`, `journal`, `indexes`, `object_writer`, `read` | `TxState` *(scoped opaque)*, `TxRowsState[K, V]` *(scoped opaque)*, `AccountingState` *(opaque, mutable `ceiling`)*, `CallStreamState` *(opaque)*, `Parked` *(opaque)*, `PendingState` *(opaque)*, `Served`, `Completion`, `ClaimKey`, `ClaimSweep`, `ClaimRecovery`, `KeyedPolicy`, `Verdict`, `Difference`, `UnknownReason`, `ReplaySource`, `data_order`, `stage_transaction`, `settle_draft_local`, `commit_prepared_state`, `serve_call`, `conclude_call`, `put`, **`put_ordered` *(3076)*,** `delete`, `fetch`, `fetch_bytes`, `pop_first`, `pop_last`, `open_range_bounds`, `tx_count`, **`visit_range`, `visit_range_reverse` *(3062 `e9ca4d65`)*,** `rollback_writes_on_*`, `cell_*`, `log_*`, `edge_*`, `admit`, `admit_claim`, `conclude_claim`, `sweep_streams`, `sweep_claims`, `fork_branch`, `reexecute*` |
| 16 | `transfer` | 11 | `store-transfer-core.hirsel` *(split)*, `store-export.hirsel`, `store-object-export.hirsel` *(copy/orchestration half)*, `store-object-import.hirsel`, `store-prepared-check.hirsel`, `store-catchup.hirsel`, `store-rounds.hirsel`, `store-install.hirsel`, `store-activation.hirsel` | Layers 0–10 | `TransferState` *(opaque)*, `PreparationState` *(opaque)*, `ValidatedImportState`, `ImportedState`, `ImportWorkState`, `DeployError`, `PreparedTarget`, `ExportCopy`, `ExportStamp`, `Exported`, `Cutover`, `declare_export_state`, `begin_backup`, `begin_export`, `begin_export_copy`, `export_copy`, `release_export`, `open_export`, `write_prepared`, `validate_prepared`, `reserve_import`, `import_trees`, `keep_import`, `catch_up_export`, `stage_activation`, `install_activation`, `perform_cutover`, `restore` *(3079 T5 listed-packs-only)* |
| 17 | `maintenance` | 11 | `store-index-build.hirsel`, `store-verify.hirsel`, `store-expiry.hirsel`, `store-projector.hirsel` *(minus `Pin`)*, `store-operator-verify.hirsel` *(L73–L176 of `store-inspect`)* | Layers 0–10 | `ProjectorState` *(opaque)*, `DeclaredProjector`, `ProjectorDelta[K, V]`, `ProjectorMerge`, `Binding`, `Generation`, `Image`, `Pass`, `Projection`, `ProjectionDecl`, `IndexWalk`, `WalkFault`, `WalkFinding`, `WalkReport`, `WalkMode`, `VerifyOutcome`, `RepairOutcome`, `Swept`, `index_build_step`, `rebuild_index`, `rebuild_aggregate`, `begin_verify`, `verify_step`, `verify_release`, `repair_indexes`, `expire_rows`, `sweep_expired`, `projector`, `project_step`, `project_recorded`, `projection_row` |
| 18 | `store` | 12 | `store.hirsel` *(composition root `Db`/`DiskState`/`Backend`, open/close)*, `store-public-types.hirsel` *(Tier A nominal types + zero-copy conversions + Tier C aliases)*, `store-coordinators.hirsel` *(foreground commit/cut/durable/seal coordinators + thin `pub fn` forwarders)*, `store-attention.hirsel`, `sizes.hirsel` *(+ 3076 Directory timer budget in `writer_plan`)* | Layers 0–11 | All external `store.*` symbols, byte-identical `PROG-9` nominal types (§2), single foreground coordinators (§5), `open`, `open_authority`, `open_read_only`, `open_shared` *(3062)*, `memory`, `close`, `checkpoint`, `cut_install`, `durable*`, `seal`, `adopt`, `delete_branch`, `pin*`, `snapshot*`, `follow`, `attention*`, `accounts`, `store_limits`, sizing APIs |
| 19 | `main` | 13 | `main.hirsel` *(currently an empty successful entry point making no calls to `store`)* | `store` | `pub fn main` |

*Note on `store-keyed-journal.hirsel` (182 L):* L1–L112 (wire codec) → `format`; L114–L166 (`materialize_keyed_edits`, `keyed_lookup`, `keyed_draft_live`) → `pages` (`store-keyed-pages.hirsel`); L168–L182 (`resident_with_credit`, `available_resident`, `available_journal`, `draft_credit`) → `txn` (`store-keyed-claims.hirsel`).

---

## 4. Owned Subsystem State (`Db` and `DiskState` Decomposition, High Finding 7)

### 4.1 Composition Roots

`Db` drops from **39 raw fields** to **13 fields**:

```hirsel unbuilt
pub opaque type Db {
    // Immutable identity, capabilities & cross-cutting root
    store: types.StoreKey,
    clock: Clock,
    limits: types.LimitsSpec,
    attention_seq: u64,
    // Owned subsystem state (each pub opaque in its module)
    holders: pages.HolderTable,
    schema: catalog.SchemaState,
    indexes: indexes.IndexState,
    pager: pager.PagerState,
    accounting: txn.AccountingState,
    call_streams: txn.CallStreamState,
    projectors: maintenance.ProjectorState,
    // Shared commit-ticket sequence & optional durable backend
    frame_seq: u64,
    disk: Option[DiskState]
}
```

`DiskState` drops from **34 raw fields** to **8 fields**, separating Directory (`log@1`) and Object (`objects@1`/`sim@1`) cleanly behind `Backend`:

```hirsel unbuilt
type Backend = DirBackend(journal.DirJournalState) | ObjectBackend(object_writer.ObjectWriterState)

type DiskState {
    // Immutable open context
    open: u64,
    options: types.Options,
    tasks: Tasks,
    // Cross-backend CallRecord retention coordinate pair
    retention: journal.RecordRetention,
    // Owned subsystem states
    packs: manifest.PackDiskState,
    tiered: pager.DiskPagerState,
    transfer: transfer.TransferState,
    // Exclusive persistence backend
    backend: Backend
}
```

### 4.2 Distinct Lifecycles Preserved by This Representation

- **`Memory` (`Db.disk == None`):** No `DiskState`, no journal worker, no `Backend`. Commits use `AccountingState.changes` as ticket sequences (`store-commit.hirsel:254`). `CallStreamState` and `PagerState` hold their own copied policy fields (`stream_retention_ms`, `streams_per_caller_max`, `streams_max`, `writer_pin_bytes`) from `Options` so memory opens need no `DiskState`.
- **`LogAuthority` → `DirBackend(journal.DirJournalState)`:** Owns non-optional `lock: HostFile` and `d: Dir`, `creation: Bytes`, `file_seq`/`file_end`, `poison: Option[IoError]`, Directory worker + `coalesce_timer_task` + `end_timer_task` *(3076)*, and `DirJournalMsg` channel.
- **`ObjectAuthority` (`authority_kind == 1`) & `SimAuthority` (`authority_kind == 2`) → `ObjectBackend(object_writer.ObjectWriterState)`:** Carries `authority_kind: u8` and stream mode (`Standalone` vs `Attached(HostStreamId)` vs `DirectService` when `attached == true && stream == None`). Owns `ObjectJournalMsg` worker, `journal_idle_task` (idle head poll timer for standalone OR `slot_relay` task for attached), `weak` waiters (`SimAuthority`), `head_entry_seq`, `draft_offset`, `export_lifetime`, `archive_on`, `pending_archive_on`, `failed: Option[Error]`, and ATTACH flags.
- **`JournalService` (host stream):** Independent multi-Place service task owning its own `S/reg`, `RegisterLane`, `HoldTracker`, `StreamGcState`, and `slots`. Its lifecycle is completely independent of any one `Db`.
- **`ReadOnly`, `SharedCapture` *(3062)*, and `Follower` / `FollowerService`:** Independent lifetimes without `Db` or `DiskState`.

### 4.3 Corrected Field-by-Field Assignment: `Db` (All 39 Fields)

Every invariant below corrects the errors flagged in Review 3085 Finding 7:

| # | Current `Db` Field | Type | New Owner Module & Struct | Corrected Invariant Maintained by Owning Module |
|---|---|---|---|---|
| 1 | `store` | `StoreId` | **`store` (stays on `Db`)** | Immutable 128-bit store identity fixed at genesis/open. |
| 2 | `clock` | `Clock` | **`store` (stays on `Db`)** | Injected host clock capability. |
| 3 | `limits` | `StoreLimits` | **`store` (stays on `Db`)** | Initialized from `named_limits(o.limits)` on memory open (`store.hirsel:609`) and overwritten with the selected MANIFEST's recorded limits on durable open (`store.hirsel:3918`, `store-object-writer.hirsel:216`). Immutable after open finishes. |
| 4 | `disk` | `Option[DiskState]` | **`store` (stays on `Db`)** | `None` iff `memory@1`; `Some` iff `log@1`/`objects@1`/`sim@1`. |
| 5 | `attention_seq` | `u64` | **`store` (stays on `Db`)** | Monotonic sequence bumped via `bump_attention(mut db)` on any operator-visible signal change across `indexes` (`count_drift`), `object_writer` (reclaim), or `transfer` (activation). |
| 6 | `frame_seq` | `u64` | **`store` (stays on `Db`)** | Minted on durable commit/fork installation BEFORE `handoff` (`store-commit.hirsel:228`); `0` on `memory@1` (memory tickets use `AccountingState.changes`, `store-commit.hirsel:254`). On 3075 empty-commit skip, not incremented. `backend.journaled <= frame_seq`. |
| 7 | `pager` | `Option[Sender[TieredPagerMessage]]` | `pager.PagerState` | `Some` iff Tiered pager is active; paired with `pager_writer` and `park_notices`. |
| 8 | `pager_writer` | `Option[Sender[TieredPagerMessage]]` | `pager.PagerState` | One-slot writer demand channel into the same pager worker. |
| 9 | `park_notices` | `Option[Receiver[()]]` | `pager.PagerState` | Coalescing wake receiver handed out at most once via `park_notices(mut db)`. |
| 10 | `parked` | `ParkQueue` | `pager.PagerState` | Entries strictly ascending by `id < next_id`; at most one in-flight writer demand; cleared on close/poison. |
| 11 | `writer_pin_bytes` | `u64` | `pager.PagerState` | Effective policy copied from `Options.writer_pin_bytes` at open (works on memory opens too). |
| 12 | `indexes` | `IndexRegistry` | `indexes.IndexState` | Maps `(table, index, schema)` to extractors and `LogRetention`; installed through owner methods on both `seal` (`store-declare.hirsel:675`) and `install_activation` (`store-activation.hirsel:223`). |
| 13 | `drift` | `List[IndexDrift]` | `indexes.IndexState` | At most one entry per `(table, index)`; demotes drifted index before subsequent writes (`§I.3`). |
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
| 24 | `file_seq` | `u64` | Split: `journal.DirJournalState.file_seq` & `object_writer.ObjectWriterState.head_entry_seq` | On `DirBackend`: active `log@1` segment sequence (monotonic across rotations). On `ObjectBackend`: initialized to `head.entry.seq` (`store-object-writer.hirsel:312`), updated to `seq` on every object cut (`:623`), and returned by `archive(mut db, on)` (`:655`). |
| 25 | `file_end` | `u64` | Split: `journal.DirJournalState.file_end` & `object_writer.ObjectWriterState.draft_offset` | On `DirBackend`: exact byte offset in active `log@1` segment (`>= 72`, reset to `72` on rotate). On `ObjectBackend`: draft framing offset starting at `72`, advanced by `list.len(frame)` on commits/forks (`store-commit.hirsel:223`, `store-fork.hirsel:66`) before `object_journal_add` rebinds frames into GROUP entries (`store-object-journal.hirsel:37`), and reset to `72` on object cut (`store-object-writer.hirsel:624`). |
| 26 | `resident` | `u64` | `txn.AccountingState` | Conservative logical Memory byte count (exact when `settled == changes`, I4); checked against `available_resident`. |
| 27 | `tiered` | `u64` | `txn.AccountingState` | Logical Tiered byte count updated by prior deltas and settled on cuts. |
| 28 | `page_resident` | `u64` | `txn.AccountingState` | **Derived charge** (`resident_location_charge(packs.pages.locations)`), refreshed atomically by the root coordinator on open and every cut; reported beside `resident`, never admitted against `ceiling`. |
| 29 | `changes` | `u64` | `txn.AccountingState` | Monotonic count of non-skipped commits and cut-made mutations; also supplies `memory@1` commit ticket sequences (`store-commit.hirsel:254`). |
| 30 | `settled` | `u64` | `txn.AccountingState` | `changes` value at which `resident` was last exact (`settled <= changes`). |
| 31 | `ceiling` | `u64` | `txn.AccountingState` | **Mutable** effective resident admission ceiling: initialized to `options.resident_limit`, lowered by `reserve_import` (`store.hirsel:3257`), and restored by `forget_import` (`store-install.hirsel:122`). Exposed via `reserve_ceiling` / `restore_ceiling`. |
| 32 | `checkpointed` | `u64` | `txn.AccountingState` | Latest durable cut generation installed (`0` on memory before cut, `>= 1` on disk/object). |
| 33 | `keyed_credit` | `KeyedCredit` | `txn.CallStreamState` | Exact resident/padding/journal credit reserved for open keyed claims. |
| 34 | `admits` | `Map[Bytes, OpenAdmit]` | `txn.CallStreamState` | Active unreplied `Admitted(p)` call-stream entries committed during this open (`§1.8`). |
| 35 | `stream_retention_ms` | `u64` | `txn.CallStreamState` | Effective policy copied from `Options` on open (including memory opens). |
| 36 | `streams_per_caller_max` | `u64` | `txn.CallStreamState` | Effective per-caller call-stream cap copied from `Options`. |
| 37 | `streams_max` | `u64` | `txn.CallStreamState` | Effective global call-stream cap copied from `Options`. |
| 38 | `stream_sweep_after` | `Option[StreamSweepCursor]` | `txn.CallStreamState` | Incremental cursor for bounded call-stream expiry sweeps. |
| 39 | `projectors` | `List[DeclaredProjector]` | `maintenance.ProjectorState` | Registered local/follower projectors (`§1.14`); installed via owner operation on both `seal` and `install_activation`. |

### 4.4 Corrected Field-by-Field Assignment: `DiskState` (All 34 Fields)

| # | Current `DiskState` Field | Type | New Owner Module & Struct | Corrected Invariant Maintained by Owning Module |
|---|---|---|---|---|
| 1 | `open` | `u64` | **`store` (stays on `DiskState`)** | Open incarnation/epoch stamped into every `Ticket` and `Snapshot` (`seq` on Directory, `head.pair.epoch` on Object). |
| 2 | `options` | `Options` | **`store` (stays on `DiskState`)** | Immutable validated `Options` snapshot fixed at open (also source for `forget_import` restoring `AccountingState.ceiling`). |
| 3 | `tasks` | `Tasks` | **`store` (stays on `DiskState`)** | Task capability shared across `journal`, `pager`, `object_writer` (same capability previously duplicated on `ObjectWriterState.tasks`). |
| 4 | `record_cut` | `u64` | `journal.RecordRetention` *(on `DiskState`)* | First coordinate of retention floor: first live `log@1` segment or `objects@1` checkpoint entry seq. |
| 5 | `recorded_after` | `u64` | `journal.RecordRetention` *(on `DiskState`)* | Second distinct coordinate of retention floor: main-branch version at `record_cut`. Both coordinates installed together on every successful cut for either backend. |
| 6 | `pages` | `PageDisk` | `manifest.PackDiskState` | **Authoritative** pack allocator/location/list/owner/compaction state; updated only after MANIFEST or Object checkpoint publication succeeds (paired with `AccountingState.page_resident` refresh). |
| 7 | `scrubs` | `List[Arc[ScrubLease]]` | `manifest.PackDiskState` | Active scrub leases pinning referenced packs against AP21 reclamation. |
| 8 | `pager_task` | `Option[Task[()]]` | `pager.DiskPagerState` | Owned Tiered pager worker task; cancelled and joined on `close`. |
| 9 | `tiered_dirty` | `TieredDirty` | `pager.DiskPagerState` | Dirty Tiered collections/overlays awaiting cut emission; reset atomically on cut publish. |
| 10 | `restamp` | `Option[u32]` | `pager.DiskPagerState` | Collection id queued for `Memory -> Tiered` re-stamping on schema cut. |
| 11 | `unload` | `Option[u32]` | `pager.DiskPagerState` | Collection id queued for `Tiered -> Memory` unload on schema cut (mutually exclusive with `restamp`). |
| 12 | `lease` | `Option[Arc[ExportLease]]` | `transfer.TransferState` | Active online-export lease pinning segments/packs against removal. |
| 13 | `steps` | `List[LeaseStep]` | `transfer.TransferState` | Ordered schema commits on `main` since `lease` last advanced (`STEPS`). |
| 14 | `imported` | `Option[KeptImport]` | `transfer.TransferState` | Active kept-import pin and delta-round cursor; cleared by `forget_import` along with `AccountingState.ceiling` restore. |
| 15 | `reserving` | `Option[Inline[u8, 32]]` | `transfer.TransferState` | Digest of currently reserved `ValidatedImport`. |
| 16 | `target` | `Option[TargetLimits]` | `transfer.TransferState` | Target limits enforced on cuts while cutover is staged. |
| 17 | `lock` | `Option[HostFile]` | `journal.DirJournalState` *(non-optional `HostFile`)* | Exclusive `LOCK` held from before recovery through worker shutdown on `close` (`store-journal-status.hirsel:156`). |
| 18 | `d` | `Option[Dir]` | `journal.DirJournalState` *(non-optional `Dir`)* | Directory capability for `log@1` segments, packs, and `MANIFEST`. |
| 19 | `creation` | `Bytes` | `journal.DirJournalState` | Pre-allocated 4 KiB-aligned scratch block for `O_DIRECT` segment creation (never allocated on `ObjectBackend`). |
| 20 | `acks` | `Receiver[Ack]` | Split: `DirJournalState` (`Receiver[DirAck]`) & `ObjectWriterState` (`Receiver[ObjectAck]`) | Strictly ordered durability/error acks; missing worker ack on closed channel poisons (`Dir`) or latches terminal failure (`Object`). |
| 21 | `journaled` | `u64` | Owned per backend (`QueueAccounting`) | Highest `frame_seq` durably persisted (`journaled <= Db.frame_seq`). |
| 22 | `inflight` | `u64` | Owned per backend (`QueueAccounting`) | Unacknowledged bytes in flight (`<= FRAME_LIMIT`); charged **once** after successful `channel.send`, subtracted **once** via `bytes_out - acked_bytes` on ack. |
| 23 | `uncovered` | `u64` | Owned per backend (`QueueAccounting`) | **Distinct from `since_cut`!** On Directory reopen (`store.hirsel:3936`), initialized to recovered journal bytes (not `0`); cleared to `0` **only after** `MANIFEST` or Object `head` publication succeeds (`store.hirsel:4108`, `store-object-writer.hirsel:610`). Checked against `available_journal`. |
| 24 | `since_cut` | `u64` | Owned per backend (`QueueAccounting`) | **Distinct from `uncovered`!** Starts at `0` after reopen; on Directory cut, reset to `0` **before** draining (`store.hirsel:4018`) so refused cuts do not tight-loop; on Object cut, reset to `0` after publication (`store-object-writer.hirsel:611`). Triggers `maybe_checkpoint` when `> options.checkpoint_bytes`. |
| 25 | `acked_bytes` | `u64` | Owned per backend (`QueueAccounting`) | Cumulative acknowledged byte watermark; prevents `inflight` drift. |
| 26 | `written` | `Receiver[WriteNotice]` | Owned per backend (`DirJournalState` & `ObjectWriterState`) | Receives `Written` barrier confirmations before `recorded`/`reexec` reads active journal. |
| 27 | `written_send` | `Sender[WriteNotice]` | Owned per backend (`DirJournalState` & `ObjectWriterState`) | Cloned sender for worker `Barrier` replies. |
| 28 | `synced` | `Option[Receiver[()]]` | Owned per backend (`DirJournalState` & `ObjectWriterState`) | Coalesced group-sync notification handle sent **after** `Journaled`/`ObjectJournaled` ack (and on terminal failure). |
| 29 | `poison` | `Option[IoError]` | Owned per backend: `DirJournalState.poison` & `ObjectWriterState.poison` | Sticky first I/O failure (`Closed` on dead outbound queue or worker channel EOF, or Directory I/O error). On `ObjectBackend`, coexists with `ObjectWriterState.failed: Option[Error]` for typed non-I/O failures (`Fenced`, `Latched`, `Remote`, `Undecided`), preserving covered-ticket-before-error! |
| 30 | `journal` | `Sender[JournalMsg]` | Split: `DirJournalState` (`Sender[DirJournalMsg]`) & `ObjectWriterState` (`Sender[ObjectJournalMsg]`) | Bounded outbound queue (`2 * group_commits + 3` on Directory per 3076; `2 * group_commits` on Object). |
| 31 | `pending` | `Option[Bytes]` | Owned per backend (`QueueAccounting`) | At most one staged commit/fork frame awaiting immediate `handoff` after commit install. |
| 32 | `journal_task` | `Task[Result[(), Error]]` | Split: `DirJournalState` (+ `coalesce_timer_task`, `end_timer_task` per 3076) & `ObjectWriterState` | Owned background journal worker task; joined on `close` before `LOCK` release (Directory). |
| 33 | `journal_idle_task` | `Option[Task[()]]` | `object_writer.ObjectWriterState` *(never on `DirBackend`)* | Idle head-poll timer (standalone `objects@1`) or `slot_relay` task (attached host-stream Place); cancelled and joined on `close`. |
| 34 | `object` | `Option[ObjectWriterState]` | `object_writer.ObjectWriterState` *(the `ObjectBackend` arm of `Backend`)* | Complete inventory (`store-object-writer.hirsel:1-24` + backend fields): `authority_kind`, `workers`, `health`, `reclaims`, `unhealthy`, **`export_lifetime`**, `watch`, `terminal`, `locator`, `levels`, `weak`, `pair`, `recovered`, `entries`, `head_entry_seq`, `draft_offset`, `failed`, `stream: Option[HostStreamId]`, `attached: bool`, `has_sealed: bool`, `drop_attach_on_cut: bool`, **`archive_on: u64`**, **`pending_archive_on: Option[bool]`**. *(Explicitly distinguishes `attached` vs live `stream` vs `has_sealed`: an attached head with `live == None` has `attached == true, stream == None` and still requires a forced open-time cut!)* |

---

## 5. Foreground Coordinator, Paused-Worker Settlement, and Recovery Fence Contracts (High Findings 4, 5, 6)

### 5.1 Single Foreground Commit & Checkpoint Coordinator (`store-coordinators.hirsel`, High Finding 4)

Opaque subsystem states do not coordinate cross-subsystem transactions by themselves. Module **`store` (Layer 12)** is the single foreground coordinator for commits, checkpoints, and durability waits:

1. **Commit sequence (`store_body_at` → `append_transaction_at` → `settle_admit` → `handoff` → `maybe_checkpoint`):**
   - **Step 1 (Draft):** Run `txn.staged_until_decided(...)`. On `DriftStop`, record drift in `indexes.IndexState`, bump `attention_seq`, return `Failed(Drift)`.
   - **Step 2 (3075 empty-commit skip branch):** If `is_empty_commit` holds (0 staged data ops after folding callbacks/trims/cells, 0 stream ops, no keyed-credit delta, no `CALL`, `settlement == None`, and no `store.info(mut tx)`):
     - *Directory (`LOCK`-fenced):* zero I/O, no frame encoded, `Done.info = None`, no `frame_seq`/`changes`/`at` increment, **skip `maybe_checkpoint`**. Ticket carries current `(frame_seq, branch, head)` (or `changes` on Memory).
     - *Bucket / host-stream (head-fenced):* even if every prior ticket is already covered, execute a fresh head read / ETag compare via `object_writer.object_fresh_probe`. If head changed or claim lost → return `Fenced` and latch identically to a failed head write. Otherwise return `Done(info: None, ...)` with **no cut and no increment**.
   - **Step 3 (Admission & optional pre-commit accounting cut):** Check `journal_limit` (`uncovered + len(frame) <= available_journal`). If `over_bound && retaining`: compute exact union account; if `accounting.settled != accounting.changes && draft.tiered == None`, run **pre-commit `cut_install(NoHolderChange)`**, then **reseal `draft.frame`** against post-cut `(file_seq, file_end)` (`format.seal_frame(raw, DiskContext(store, 2, seq), offset)`).
   - **Step 4 (Headroom wait):** Wait via `wait_for_frame_room` until `backend.inflight + len(frame) <= FRAME_LIMIT` (polling acks through the common queue contract).
   - **Step 5 (Atomic foreground installation):** Update `CallStreamState` (`keyed_credit`, `admits`), reserve/admit `DiskPagerState.tiered_dirty`, advance backend offset (`Dir.file_end` or `Object.draft_offset`) and mint `Db.frame_seq += 1`, stash `backend.pending = frame`, install `HolderTable` roots + `record_bytes`, update `AccountingState` (`resident`, `tiered`, `changes`, `settled`), publish `Latest[Snapshot]`.
   - **Step 6 (FIFO `handoff`):** Take `backend.pending` and send `Frame(raw, db.frame_seq)` to the backend worker. **Charge `inflight`, `uncovered`, and `since_cut` strictly AFTER `channel.send` succeeds** (`store.hirsel:1142-1148`). On closed channel, poison (`Dir`) or record failure (`Object`) without charging.
   - **Step 7 (Post-handoff checkpoint):** Call `maybe_checkpoint(mut db)` (skipped on `Refused(Journal)` or 3075 empty-commit skip). If `since_cut > checkpoint_bytes` or `object_cut_due`, execute a **second `cut_install(NoHolderChange)`**, swallowing `Full(_)` as deferred.

2. **Directory checkpoint cut sequence (`cut_install` → `DirBackend`):**
   1. Resolve orphan import pin if `NoHolderChange`; check `healthy(db)`.
   2. Reset `DirJournalState.since_cut = 0` **before** draining (`store.hirsel:4018`), leaving `uncovered` untouched.
   3. Drain through final ticket (`durable_groups(Journal(db.frame_seq, 0, 0))`).
   4. Reserve holder slot (`prepare_holder_slot`), prune expired `PackDiskState.scrubs` and `compaction.held` leases, build `CutParticipants`, stage Tiered cut (`DiskPagerState`), update `HolderTable.next_node`, raise materialized pin watermark, compute `page_retry_base` and lineage rows, check `available_resident`.
   5. Inside `prepare_page_cut` retry loop: `plan_page_cut`; if `dir.file_seq != next_seq`, call `journal.dir_rotate` (**a retry after segment creation reuses the already-rotated segment**, `store.hirsel:4070`); write Tiered & page packs; compute `cut_retirement`; build candidate `MANIFEST`.
   6. Prepare Tiered install, validate candidate (`validate_cut_pages`), **sync directory, write & sync `MANIFEST.tmp`, rename to `MANIFEST`, sync directory again** (`manifest.publish_page_manifest_bytes`).
   7. Install authoritative state: `PackDiskState.pages`, `RecordRetention(seq, recorded_after)`, clear `uncovered = 0`, reset `DiskPagerState.tiered_dirty`, update `AccountingState` (`checkpointed`, `resident`, `tiered`, `page_resident`, `settled`), install `HolderTable` delta and `TransferState` lease step.
   8. Unlink retired files (`journal.dir_remove_retired`). **A post-publication removal error poisons the writer (`cut_io_error`, `store.hirsel:4131`) even though the renamed `MANIFEST` is already authoritative.**

### 5.2 Stream Settlement Under `ObjectPause` (High Finding 5)

During an Object cut (`object_writer.object_cut_state`), the foreground drains the latest ticket, reserves holder capacity, and sends `ObjectPause(handover, resuming)` to the object journal worker. The worker transfers exclusive ownership of `ObjectJournalSession` over `handover` and **blocks on `channel.receive(resuming)`**.

**Non-reentrant settlement invariants (statically preventing worker deadlock):**
1. **`SettleSession` has zero access to `Sender[ObjectJournalMsg]`:** `host_stream.stream_settle`, `host_stream.stream_settle_sealed`, and `host_stream.stream_poll_head` operate strictly on a narrow `SettleSession` carrying the paused worker's direct I/O resources (`client`, `register`, `authority`, `buffers`, `stream_root`, `last_batch_seq`, `commit_batches`, `archive_on`, `op_ms`, `group_bytes`, `group_commits`). They cannot send `Frame`, `ObjectFresh`, or `ObjectExportRead` to the paused worker.
2. **Unconditional session return:** `object_cut_state` ALWAYS sends the exclusive session back on `resuming` on **every** exit path (success, `SettleFenced`, `SettleNotApplied`, `Undecided`, or any `Error`), latching `ObjectWriterState.failed` whenever `session.register.terminal` is `Some`.
3. **Immutable-before-CAS and `HistoryApplied`-only install:** All packs, archive segment, locator, manifest, and checkpoint entry objects upload before `head` CAS (`Match(register.tag)`). **Only `HistoryApplied(tag)`** installs the new `head`, `tag`, `attach`, `locator`, `RecordRetention`, and reports the new cutoff/detach to the stream service. Every non-applied outcome restores `prior_attach` on `session.register.attach` and propagates the latched error.
4. **Archival & CLAIM ordering:** When `archive_on != 0`, sealed tails settle in exact order **before** the live tail (`stream_settle_sealed` → `stream_settle`), and both before sealing the archive segment. Direct-service `CLAIM` (`attach.live == None`: optional `stream_settle_sealed_core` + `history_advance_claim` entry) remains distinct from slot `CLAIM` (`attach.live == Some(_)`: `slot_claim_frame` + `slot_send` + `slot_wait`).
5. **`StreamAnswer` FIFO and ticket coverage contract:** `StreamAnswer(SlotAck)` rides the same FIFO `ObjectJournalMsg` channel as `Frame` and control messages. On `SlotFenced(epoch, ok_through, missing)`: if `!missing && ok_through > 0`, emit `ObjectJournaled(ok_through, ...)` + `synced` notice **before** returning `Error(Fenced(epoch))` (covered prefix succeeds before `Fenced`); if `missing`, return `Error(Undecided)`. On `SlotLatched(cause)`, return `Error(Latched(cause))`, and `durable_latch_error` maps every outstanding ticket of an attached `Db` to `Undecided` (even under `Superseded`). Never await an ack that only the paused worker or blocked service loop can deliver.

### 5.3 Three Distinct Directory Recovery Entry Points (High Finding 6)

`journal` (Layer 5) and `capture` (Layer 6) MUST expose and preserve three distinct recovery contracts:

1. **Directory writer open (`journal.disk_session`):** Checks `MANIFEST` header if present, **acquires exclusive `LOCK` before recovery** (`disk_lock` with `lock_wait_ms`), checks `EXPORT`, removes orphan `j-<seq>.end` files and **fsyncs the directory before creating any successor segment** (`store.hirsel:3639`). Replay (`journal_scan`) bounds middle segments by the successor header's `prev_end` and the final segment by `END` (`durable_end`); **only** specified physical torn-tail faults (`journal_torn(fault.why)`) in the final segment at `offset >= durable_end` terminate cleanly — all logical decode/apply errors and all pre-boundary or middle-segment faults propagate as `Corrupt`. Directory `close` holds `LOCK` through worker shutdown (`task.join`) and drains remaining acks before `host.close(lock)` (`store-journal-status.hirsel:127-162`).
2. **Read-only live capture (`capture.capture_directory` / `capture.open_shared`):** Retains selected segment file descriptors, acquires the shared terminal segment fence (`flock`), **probes writer `LOCK` non-blockingly** (never blocking on `LOCK`, which would invert the writer's `LOCK -> terminal segment` lock order!), polls every 1 ms if `LOCK` is free while the terminal fence is held (dead writer in-flight direct write), calls `host.sync_dir(d)` (O1), and compares exact `MANIFEST` bytes against the initial read before replaying (restarting on change).
3. **Offline locked recovery (`journal.disk_recover_locked`, required by lane 3079 offline backup):** Accepts proof of an already-held writer `LOCK` and runs the writer's exact recovery scan without re-opening `LOCK` or spawning a journal worker.

---

## 6. Claim-Machine API & Service Trackers (`host_stream`, Critical Finding 2 & High Finding 8)

### 6.1 Prerequisite Bug-Fix Lane (Lane 0): Existing Pre-`H` Abandon Gap

**Assessment per Sam's store rule:** At `store-stream-claim.hirsel:585-606`, after acquiring a hold row (`side.token == Some(token)`) and verifying the final `head` bytes (`stream_claim_final_head`), candidate-`H` construction (`sealed` list allocation `:587`, `history_claim_head` `:603`, `history_authority_end` `:604`, `authority` copy `:605`, `history_encode_register_head` `:606`) uses bare `?` before `history_write_head` (`:607`). Any allocation/limit/corrupt failure there returns directly without calling `stream_claim_abandon`. This is a **straightforward bug fix** against an already-ratified invariant (`STORE-SPEC §12.11.6` rule (e): a definite pre-`H` failure must attempt `Abandon(token)`, while a failed abandon itself leaves the row for service rule (a) cleanup). It needs no new protocol design and is therefore scheduled as **Lane 0** (its own first prerequisite lane, before L1) so the live bug on `main` is fixed immediately and proved by a new law case in `bucket_stream_claim_laws.hirsel`.

### 6.2 `ClaimMachine[T]` — Owned Per-Phase Payloads & Pre-I/O `HSent` Recording

Every phase of `ClaimMachine[T]` owns the exact state needed by the transitions leaving it. `RegisterLane.tag` belongs exclusively to `S/reg`; the Place `head` CAS tag (`head_tag: std.object.ETag`) comes strictly from the step-5 final `head` GET and lives in `ReadyToSendH`:

```hirsel unbuilt
pub type ClaimConfig: send {
    auth: types.ClaimAuth,           // 3048: expected (epoch, holder) + attempt bytes
    holder: types.StoreKey,          // independently fresh random physical holder (NEVER == auth.attempt!)
    authority_kind: u8,
    place_prefix: String,
    stream_root: String,
    resident_limit: u64,
    limits: Option[types.LimitsSpec],
    requested_levels: List[String],
    deadline: u64,
    max_rows: u64,
    claim_ms: u64,
    group_bytes: u64,
    group_commits: u64
}

pub type ClaimPhase[T] =
    Init
  | DirectSettled(
        head: history.HistoryHead,
        attach: format.Attach,
        replay: journal.JournalReplay,
        archive_on: u64
    )
  | SettledLoaded(
        head: history.HistoryHead,
        attach: format.Attach,
        tail: format.StreamRef,
        replay: journal.JournalReplay,
        archive_on: u64
    )
  | PreScanned(
        head: history.HistoryHead,
        attach: format.Attach,
        tail: format.StreamRef,
        cursor: TailScan,
        replay: journal.JournalReplay,
        archive_on: u64
    )
  | NoHoldFenced(
        head: history.HistoryHead,
        attach: format.Attach,
        tail: format.StreamRef,
        cursor: TailScan,
        replay: journal.JournalReplay,
        archive_on: u64
    )
  | Held(
        head: history.HistoryHead,
        attach: format.Attach,
        tail: format.StreamRef,
        token: types.StoreKey,
        rev_hold: u64,
        cursor: TailScan,
        replay: journal.JournalReplay,
        archive_on: u64
    )
  | ReadyToSendH(
        head: history.HistoryHead,
        attach_next: format.Attach,
        claimed: history.HistoryHead,
        authority: Bytes,
        head_tag: std.object.ETag,   // strictly from step-5 final head GET (never lane.tag!)
        hold: Option[types.StoreKey],// retained until H disposition is confirmed
        replay: journal.JournalReplay,
        admitted: T,
        archive_on: u64
    )
  | HSent(                           // recorded BEFORE history_write_head I/O!
        head: history.HistoryHead,
        attach_next: format.Attach,
        claimed: history.HistoryHead,
        authority: Bytes,
        hold: Option[types.StoreKey],
        replay: journal.JournalReplay,
        admitted: T,
        archive_on: u64
    )
  | HeadPublished(
        register: history.HistoryRegister,
        authority: Bytes,
        replay: journal.JournalReplay,
        admitted: T,
        archive_on: u64
    )
  | Claimed
  | FailedPreH(types.StoreError)
  | HAmbiguous(hold: Option[types.StoreKey], err: types.StoreError)
  | FailedPostH(types.StoreError)

pub opaque type ClaimMachine[T] {
    phase: ClaimPhase[T],
    config: ClaimConfig,
    lane: RegisterLane,              // S/reg state + S/reg ETag only
    buffers: TailBuffers,
    workspace: history.HistoryBuffers,
    granted: Option[(u64, u64)],     // acquired once, retained across retries
    witness: Bytes                   // step-1 Place head witness bytes
}

pub type ClaimedPlace[T] {
    register: history.HistoryRegister,
    replay: journal.JournalReplay,
    admitted: T,
    authority: Bytes
}

// Four distinct post-H / terminal outcomes (never collapsed into one error/restart):
pub type ClaimStep[T] =
    Continue
  | ConflictRestart                  // confirmed pre-H move or H conflict with same pair; hold abandoned; caller fetches fresh head & retries
  | ConflictLost(history.HistoryPair)// confirmed H/final-head conflict with changed pair; hold abandoned
  | AmbiguousH(types.StoreError)     // H returned HistoryRefused / HistoryUndecided; hold NOT abandoned (rule e)
  | ClaimPublishFailed(types.StoreError) // H applied, step-7 CLAIM or archival settle failed; hold NOT abandoned
  | Done(ClaimedPlace[T])
```

**Structural guarantees enforced by `ClaimMachine[T]`:**

1. **3048 `ClaimAuth` gate before ANY object mutation (including `TERM` and `Hold`):** In `Init`, before `stream_no_hold_scan` (which can write `TERM`), `Hold`, or `H`, `ClaimMachine` verifies `history_pair_matches(head.pair, config.auth.expect)` (else `FailedPreH(Superseded(head.pair))`) and verifies `config.holder != config.auth.attempt` (preserving byte-inequality resolution in `history_write_head`, `store-register.hirsel:14`).
2. **All fallible candidate construction completes BEFORE leaving `Held`:** In `Held` (and `NoHoldFenced` / `DirectSettled`), step-4 delta scan, step-5 `prepare_writer(store, replay)`, step-5 final `head` GET byte-equal to `witness` yielding `head_tag`, `sealed` list build, `attach_next`, `history_claim_head`, `authority` slice copy, and `history_encode_register_head` (including 3048 `ORIGIN` frame) into `workspace.body` ALL run before entering `ReadyToSendH`. Any failure or move here attempts `Abandon(token)` (best-effort) before transitioning to `Init` (`ConflictRestart`, retaining `granted`) or `FailedPreH(err)`.
3. **Record `HSent` BEFORE issuing `PUT head`:** Entering the `H` send step immediately replaces `machine.phase` with `HSent(...)` **before** calling `history_write_head(client, Match(head_tag), body, witness, deadline)`. Zero fallible work occurs between `ReadyToSendH` and `history_write_head`. If cancelled mid-`PUT` (`CONC-7`), `phase` is already `HSent`, so `claim_abort` NEVER abandons `hold`.
4. **Exact 4-way `H` disposition:**
   - `HistoryApplied(applied_tag)` → drop `hold` without abandoning; transition to `HeadPublished(HistoryRegister(claimed, applied_tag, None, Some(attach_next)), authority, replay, admitted, archive_on)` → `Continue`.
   - `HistoryConflict(_)` → `H` definitely did NOT apply; attempt `Abandon(token)` if `hold == Some(token)`; decode `workspace.witness`: same pair → reset to `Init` (retaining `granted`!) and return `ConflictRestart`; changed pair → `FailedPreH(Lost)` and return `ConflictLost(moved.head.pair)`.
   - `HistoryRefused(p)` / `HistoryUndecided` → **DO NOT ABANDON** `hold` (rule (e)); transition to `HAmbiguous(hold, err)` and return `AmbiguousH(err)`.
5. **Step 7 (`HeadPublished` → `Claimed` or `ClaimPublishFailed`):** Runs slot `CLAIM` (`attach_next.live == Some(_)`) or direct archival sealed settle + `history_advance_claim` (`attach_next.live == None`). Any failure transitions to `FailedPostH(err)` and returns `ClaimPublishFailed(err)` — NEVER abandoning `hold`.
6. **Safe `claim_abort(mut machine, mut client)`:** Attempts `Abandon(token)` **if and only if** `machine.phase` is `Held(token, ..)` or `ReadyToSendH(hold: Some(token), ..)`. In `HSent`, `HAmbiguous`, `HeadPublished`, `FailedPostH`, and `Claimed`, `claim_abort` is a strict no-op regarding `S/reg` (structurally enforcing rule (e)).

### 6.3 Complete Service Trackers (`RegisterLane`, `HoldTracker`, `StreamService`, High Finding 8)

3071 omitted `pending`/`stalled` from `RegisterLane` and omitted all live tracking collections from `HoldTracker`. The complete v2 definitions own every record needed to decide the next ack, stall verdict, settle, orphan poll, or timeout (`store-stream-service.hirsel:44`, `store-stream-task.hirsel:45-129, 313, 524`, `store-stream-poll.hirsel:173, 476`):

```hirsel unbuilt
pub opaque type RegisterLane {
    dir: String,
    sid: status.HostStreamId,
    authority_kind: u8,
    max_rows: u64,
    inc: u64,
    holder: types.StoreKey,
    seq: u64,
    reg: format.StreamRegister,
    tag: std.object.ETag,            // S/reg ETag only
    body: Bytes,
    compare: Bytes,
    pending: List[PendingSlice],     // slices of created batch awaiting ack decision
    stalled: List[PendingSlice],     // (store, epoch) pairs currently stalled by a reg row
    latched: Option[status.StreamLatch]
}

pub opaque type HoldTracker {
    hold_ns: u64,
    stall_poll_ns: u64,
    idle_poll_ns: u64,
    observed_rows: List[ObservedRow],   // (store, epoch, token, first_seen_ns) for rule (a)
    tickets: List[StallTicket],         // (slot, store, epoch, batch_seq, through) frontier per stalled slice
    active_settles: List[ActiveSettle], // (id, slot, store, epoch, pinned_tokens) for M1
    stall_polls: List[StallPoll],       // (slot, store, epoch, next_poll_ns) for hold_ms / 4 polls
    next_settle_id: u64,
    last_reg_ns: u64
}
```

`StreamService` owns `lane: RegisterLane`, `holds: HoldTracker`, `waiting: List[(u64, u64)]` (paired in exact 1:1 order with `lane.pending`: `(slot, through)` for each slice of the in-flight batch), `prefixes: Map[types.StoreKey, String]`, `slots: List[ServiceSlot]` (each carrying `inbox`, `place: Option[SlotPlace]`, `held`, and `unsent: Option[SlotAck]`), `gc: StreamGcState`, `janitor_asks: List[JanitorAsk]`, `timer`, and `poll_at`.

**Explicit service-loop guarantees:**
1. **Batch-slice pairing:** `stream_service_batch` populates `waiting` in exact 1:1 order with `lane.pending` slices. On `stream_cycle` / `stream_decide_pending` verdicts: `acked == true` emits `SlotAcked(through, batch_seq)` on `slot`; `acked == false` inserts/merges `StallTicket(slot, store, epoch, batch_seq, max(through))` into `HoldTracker.tickets`.
2. **Directive acknowledgement & stale-reply rejection:**
   - `ReportSettled(slot, settle_id, was_applied, _, new_from_seq, _)`: matches and removes `ActiveSettle` by exact `settle_id` (stale/duplicate `settle_id` ignored). Only when `was_applied == true` does it advance `slots[slot].place.settled_from = max(settled_from, new_from_seq)` and issue `Unhold(active.tokens)` (M1 fixed token set).
   - `ReportFenced(slot, old_epoch, moved_epoch, cutoff)`: verifies `slot < len(slots)` and `slots[slot].place` matches `epoch == old_epoch` (rejecting stale reports from detached/reused slots!), marks `place.fenced = true`, clears `HoldTracker` + `lane.stalled` for `(store, old_epoch)`, computes `(ok_through, missing)` across `tickets` against `settled_from` and `cutoff`, emits `SlotFenced(old_epoch, ok_through, missing)`, and writes `ClearEpoch(store, moved_epoch)`.
   - `ReportDetached(slot, epoch)`: verifies `place.epoch == epoch` before clearing `place`, `unsent`, tracking records, and `tickets`.
3. **Bounded-outbox failure & next-wake continuation:** `stream_answer` uses non-blocking `conn.try_send(place.answers, answer)` (`§12.11.12`). On `Error(Full(kept))`, it stores `slot.unsent = Some(kept)` (retaining the newest answer); on `Error(Closed(_))`, it clears `unsent`. Every `stream_service_wake` begins by calling `stream_flush_acks` **before** reports, polls, or new batches, and `hold_next_deadline` arms the single service alarm on the earliest of `last_reg_ns + idle_poll_ns`, any `stall_polls[i].next_poll_ns`, and any un-settled `observed_rows[i].first_seen_ns + hold_ns` — guaranteeing bounded outbox saturation never stalls the service or loses terminal notifications.

---

## 7. Split of `store.hirsel` (4,846 Lines → Focused Files at Exact Declaration Boundaries)

Every split below follows exact top-level declaration boundaries verified against `origin/main` (`store.hirsel:1-4846`), fixing 3071's `Tx` (`L513`) and `empty_roots` (`L30`) line-range errors. Module `"store"` itself is split across `store.hirsel`, `store-public-types.hirsel`, and `store-coordinators.hirsel` so every file stays well under 1,000 lines while keeping all Tier A `PROG-9` nominal declarations in module `"store"`.

| Current `store.hirsel` Lines | Est. Lines | Exact Declarations | Target File & Module |
|---|---|---|---|
| `L2–L27`, `L94–L97`, `L348–L410`, `L573–L596`, `L809–L866`, `L1567–L1597`, `L2464–L2476`, `L4146–L4186` | ~280 | Internal mirrors (`StoreKey`, `CallerId`, `BranchRef`, `StoreError`, `LimitsSpec`, `ScanLimits`, `ResumeState`), non-pinned leaf types (`Limit`, `Why`, `Fault`, `StoreCapacity`, `StorageKind`, `CallId`, `CallInfo`, `Mark`, `Ticket`, `Read`, `StoreFailure`, `Outcome`, `Undone`, `Done`, `Bound`, `Page`, `Resumed`, `Receipt`, `Stop`, `Plan`, `CallReserve`, `ServeError`, `Reserve`, `Served`, `RequestDigest`, `Stored`, `Looked`, `Seen`, `Change`, `CallRecord`, `StopKind`, `Recorded`), `splitmix64`, `call_info`, `same`, `make_id`, `capacity`, `copied` | `store-types.hirsel` → **`types`** (L0) |
| `L816–L822`, `L1683–L1763`, `L1828–L1843`, `L2499–L2798`, `L4216–L4442` | ~700 | `DISK_FORMAT`, `BINDING_BYTES`, `DiskContext`, `DiskFrame`, `DiskHeader`, `DiskFrameHead`, `le`/`read_le`/`blob`, `seal_frame`, `disk_read_frame*`, `encode_data_op`, `encode_log_trim`, `encode_stream_op*`, `encode_fork`, `JournalCommit`/`JournalFork`/`JournalEvent`, `decode_settlement`, `journal_decode*`, `journal_torn` | `store-format.hirsel` → **`format`** (L1) |
| `L28–L36`, `L99–L323`, `L411–L512` | ~360 | `Roots`, `empty_roots`, `share_roots`, `Head`, `Tip`, `State`, `ProposedMain`, `HolderDelta`, `HolderTable`, `state_with_delta`, `install_holder_delta`, `DataOp`, `StagedValue`, `DataRows` *(+ 3076 `sorted: bool`)*, `DataOps`, `Staged`, `op_slot` *(strictly stops at `L512`, before `Tx` at `L513`!)* | `store-roots.hirsel` → **`pages`** (L2) |
| `L1764–L1827`, `L1844–L1875`, `L2000–L2412`, `L2486–L2498` | ~520 | `RowSchemas`, `ReadersHandle[V]`, `TableHandle[K, V]`, `IndexHandle[J, K, V, P]`, `RowDecoder`, `row_schemas`, `row_value`, `encode_key`, `typed_bound`, `table_key`, `catalog_index_key`, `catalog_entry_in`, `stage_create_table`, `stage_table`, `catalog_handle`, `catalog_index_handle`, `stage_drop_table_key`, `migrate_build`, `stage_migrate`, `catalog_insert`, `catalog_recharge` | `store-catalog.hirsel` → **`catalog`** (L3) |
| `L2799–L2854`, `L3386–L3694`, `L3793–L3816`, `L3948–L3983`, `L4443–L4660` | ~720 | Directory I/O (`disk_io`, `disk_path`, `disk_write_synced`, `disk_remove_optional`, `SegmentSpan`, `DiskRemovals`, `DiskStartup`, `DiskSession`, `disk_lock`, `disk_start`, `disk_session`, `disk_recover_locked`, `JournalChannels`, `dir_rotate`, `JournalInput`, `journal_scan`, `JournalReplay`, `journal_following`) | `store-journal-io.hirsel` → **`journal`** (L5) |
| `L513–L560` *(internals)*, `L772–L808`, `L867–L1107`, `L1166–L1493`, `L1505–L1566`, `L1612–L1682`, `L1876–L1999`, `L2429–L2450`, `L4187–L4215`, `L4661–L4667` | ~920 | `TxState`, `Publication`, `PendingState`, `Ran`/`Stage`/`Finished`, `stage_call`, `draft_body`, `finish_stage`, `serve_inner`, `conclude_inner`, `fetch_logical`, `fetch_bytes`, `fetch`, `put`, `put_ordered` *(3076)*, `delete`, `key_span`, `published_value`, `reads_key`, `recorded` | `store-txn-core.hirsel` → **`txn`** (L10) |
| `L2855–L3385` | ~531 | `PreparedTarget`, `PreparedMeta`, `CatalogChange`, `Inventory`, `PreparedRow`, `write_prepared`, `ValidatedImportState`, `validate_prepared`, `ImportedState`, `ImportWorkState`, `reserve_import`, `import_trees`, `Cutover`, `TargetLimits`, `planned_catalog`, `planned_ranges`, `pack_leased` | `store-transfer-core.hirsel` → **`transfer`** (L11) |
| `L40–L93`, `L324–L347`, `L597–L771`, `L1108–L1165`, `L3695–L3792`, `L3817–L3947`, `L3984–L4145` | ~750–950 across 3 root files | `store.hirsel` (`Db`, `DiskState`, `Backend`, `memory`, `open`, `open_authority`, `open_read_only`, `close`), `store-public-types.hirsel` (Tier A pinned `PROG-9` nominal types + zero-copy conversions + Tier C aliases), `store-coordinators.hirsel` (commit/cut/durable/seal coordinators + thin `pub fn` forwarders) | `store.hirsel`, `store-public-types.hirsel`, `store-coordinators.hirsel` → **`store`** (L12) |
| `L1598–L1611`, `L2413–L2428`, `L2451–L2463`, `L4668–L4846` | ~220 | Inline test fixtures (`test_db`, `typed_put`, `bucket`, `OldAccount`/`NewAccount`/`upgrade`, `reserve_tx`, `law_call_reserve`) | Move to `[tests.sources]` (`store_law_schema.hirsel`); keep only `law_call_reserve` allow-listed until `runner` L33 migrates |

*Line-length ratchet rule:* Measure actual line counts of all root files after L14 lands before removing `store.hirsel` from `tools/file_length_ratchet.json`.

---

## 8. Coordination With All Eight Concurrent Lanes

| Lane | Status & Scope | Exact Coordination Against the v2 Order |
|---|---|---|
| **3008** *(landed `f5eb79cb`)* & **3048** *(active)* | 3008 landed stream exits/archival. 3048 lands universal `ClaimAuth`, 49-byte `ORIGIN` head frame (`MUST = 1`), Format 11 bump + 3023 `retirement_root`/`RetiredList` removal from `0x0031` (`MANIFEST_FIXED_BYTES` 316 → 280), `HeadView`, Runner `Deposed`. | **Land 3048 before L1 starts.** Keep `ORIGIN` in `H` and copy its bytes byte-for-byte on ordinary advances. Keep Format 11's `0x0031` `retirement_root` removal distinct from the new `ORIGIN` head frame. Rebase Lane 0 (if after 3048), L1, L4, L7 (`capture` + `history`), L8 (`host_stream`), L9 (`object_writer`), and the Runner `PROG-9` identity checks onto the exact tag and byte layout 3048 lands. (`m1b2/run-store-claim-auth-origin-3048.md:17,29`, `store-register.hirsel:44`.) |
| **3075** *(active)* | STORE-SPEC v2.147 empty-commit skip + F1–F7 review fixes (head read on Bucket/stream, `Done.info: Option[CallInfo]`, no cut on skip, `settlement == None` + `keyed.after == db.keyed_credit` in `is_empty_commit`). | **Land 3075 before splitting the commit path (L12) and preferably before L1.** On Directory, `LOCK` suffices for zero-I/O skip. On head-fenced Bucket and host-stream stores, even an already-covered ticket does not authorize returning a stale observation: preserve the fresh head check, `Fenced` latch behavior, `Done.info: Option[CallInfo]`, and skipped-checkpoint rule across L6, L8, L9, L12, and root `store-coordinators.hirsel`. (`m1b2/run-store-empty-commit-build-3075.md:12,20`.) |
| **3062** *(design landed `658cb65b` + `e9ca4d65`)* | STORE-SPEC v2.148 `SharedCapture` (`Shared[SharedBacking]`, `CONC-15`, fresh `Unique` `capture_id`, home `MEM-24` accounting — replacing 3071's `Arc` assumption!) and scoped leaf visitors (`Visit`, `RowView[K, V]`, `visit_scan*`, `visit_range*`, `row_*`). | **Integrated directly into the v2 map:** `TieredRefusal.Shared \| Scan` in **L1 (`types`, Layer 0)**; `SharedCaptureState`, `SharedBacking`, `open_shared`, `share_capture`, `freeze_roots_to_shared`, `shared_visit_leaves` in **L7 (`capture`, Layer 6)**; `shared_read_only`, `follower_share` (reading `object_writer.follower_tip_data` + calling `capture.freeze_roots_to_shared`), `VisitStep`, `RowViewState`, `visit_scan*`, `row_*` in **L11 (`read`, Layer 9)**; `visit_range*` in **L12 (`txn`, Layer 10)**; nominal `store.SharedCapture`, `store.RowView`, `store.Visit` + thin re-exports in **`store` (Layer 12)**. Implementation rebases after Step 7 (`capture`), Step 9 (`object_writer` follower tip), and Steps 11–12 (`read`/`txn`). (`perf-store-read-design-3062/DESIGN.md §5-§6`.) |
| **3076** *(design landed `06ad1735`, Astra review `3100`)* | STORE-SPEC v2.149 L94–L95: Directory adaptive group-commit coalescing + decoupled `END` cadence (`group_delay_us`, `end_groups`, `end_interval_ms`, two 4 KiB timer tasks, `writer_plan`), and checked ordered bulk ingest (`put_ordered`, `Error.OutOfOrder`, `DataRows.sorted`, `std.page.{max_key, bulk_from_source, append_sorted_source, apply_sorted_source}`). | **Integrated directly into the v2 map:** `Options`/`Overrides` fields & `StoreError.OutOfOrder` in **L1 (`types`, Layer 0)** + pinned `store.Error.OutOfOrder`; zero-second-edit-list byte seam `encode_data_ops_resolved` in **L1 (`format`, Layer 1)**; `DataRows.sorted`, binary-search `op_slot`, and `apply_collection_batch` calling `std.page.*_source` in **L2 (`pages`, Layer 2)**; `DirJournalMsg.{CoalesceTick, EndTick, Close}` + `coalesce_timer_task`/`end_timer_task` + 4-trigger `END` policy in **L6 (`journal`, Layer 5)** (land #8 build after L6 and Astra 3100 review, keeping BL5 `end == end_named` and notice ordering intact); `put_ordered` + `data_order` `InOrder` fast path in **L12 (`txn`, Layer 10)**; `writer_plan` +4.8 KiB timer pricing in `sizes.hirsel` (`store`, Layer 12). Apply #10 after L1+L2+L12 (or atomically with them), producing canonical bytes with zero second edit list. |
| **3079** *(active)* | Store capture slice 3 offline backup (locked recovery, Check before stamp, fallback to writer on orphan pin / missing terminal) + T5 listed-packs-only `restore` + damaged-backup mutilation corpus. | **Prefer landing 3079 before moving capture/export (L6, L7, L13).** Its locked-recovery entry belongs on `journal.disk_recover_locked` (L6), offline capture/check on `capture` (Step 7), and T5 listed-packs-only `restore` on `transfer` (L13). Preserve its damaged-backup mutilation oracle intact when reassigning tests. (`m1b2/run-store-capture-s3-backup-3079.md:13`.) |
| **3080** *(active)* | CLOUD #6 Bucket request counters below `std.object` retry wrapper + `hirsel store stats` output. | Request counting inside `core/std/object.hirsel` is completely independent of `store` module moves. Coordinate `stats`/`inspect` counter display and its Bucket fixture with **L7 (`capture`)** and **L14 (`maintenance` / `store`)**. Never move the counter above the retry boundary or add it to a persisted store field. (`m1b2/run-store-bucket-counters-3080.md:11`.) |
| **3081** *(active)* | Fleet Phase 2 platform seams + S1 `Backing.Bucket` (`platform.local` selectable standalone & streamed via `Config.journal`). | Node/incarnation/assignment work is independent. `platform.local` is a public external consumer of L8/L9 authority and `JournalSlot` construction and is sensitive to 3048 `ClaimAuth`. Preserve both standalone and streamed platform cases through L8/L9 landings. (`m1b2/run-platform-fleet-seams-3081.md:11,26`.) |
| **3082** *(active)* | M3 S6/S9 NVMe `objects_cache` on `runner.BucketBacking` + Bucket pack caching. | Coordinate `runner.BucketBacking` constructor with 3081. Serialize pack-read/cache plumbing in `store-history-packs.hirsel` with **L7 (`history`)** and **L9 (`object_writer`)** (or fold landed 3082 cleanly into L7/L9). Preserve content-hash verification before admission, Bucket refetch on mismatch/corruption, bounded MEM/disk charging, and working reopen after local cache deletion. The cache adds **no** authoritative `Backend` arm. (`m1b2/run-m3-s6s9-bucket-cache-3082.md:11`.) |

---

## 9. Test Ownership, Visibility, and Law-Obligation Rules (Medium Finding 9 & Missing Laws)

### 9.1 White-Box vs. Black-Box Test Placement (`PROG-4`)

Under `PROG-4` (`docs/spec/programs.md:130,145`), a test file assigned to module `M` in `[tests.sources]` sees `M`'s private members **only** — it cannot see private fields or constructors of `"store"` (`Db.resident`), `"manifest"` (`page_manifest_empty`, mutable `PageManifest` fields), or sibling sim fixtures (`BucketSim`).

Therefore:
1. **Never blindly reassign a mixed law file** (such as `store_format10_laws.hirsel` or `bucket_stream_claim_laws.hirsel`) out of `"store"`. Keep cross-module / end-to-end laws and `BucketSim`/`Db` fixture laws in `"store"`, exercising lower modules through their public surfaces (or narrow read-only inspection helpers).
2. **Split pure single-module white-box assertions** into dedicated module-owned test files in `[tests.sources]` (e.g., `format_wire_laws.hirsel` → `format`, `claim_machine_laws.hirsel` → `host_stream`) when testing private invariants directly.
3. **Update `[tests.runs]` keys whenever a `@test` moves modules:** `PROG-4` refuses any `[tests.runs."store@1/<mod>/<test>"]` entry whose module path no longer matches the test's module in `[tests.sources]`.
4. **Preserve independent oracles:** Frozen Format 10/11 corpus byte arrays (`store_format10_laws.hirsel`, `store_corpus_format10.hirsel` rebased to 3048's Format 11) MUST stay literal byte arrays independent of production encoders; numerical admission assertions (`L90-Q4`, `L70`, `L85`, `L92`/`L93`) MUST remain exact.
5. **External downstream & compile-refusal verification:** Exercise `store.*` from a downstream package (`runner` / `platform` / external smoke) through `store.*` alone (catching broken external constructors, broken `OWN-20` scoped borrow anchors, or leaked internal module methods), and preserve compile-refusal fixtures (`PROG-2` opaque constructors, 3062 V4 `RowView` escape & `Tx` mutation refusals).

### 9.2 Four New Law Obligations Added to the Cutover Lanes

Every gap identified in Review 3085 ("What the existing laws cannot establish") is assigned to an explicit lane and existing harness below:

| Obligation | Existing Harness Extended | Oracle & Bug Class Caught | Enforced In |
|---|---|---|---|
| **O1: Cross-build `PROG-9` persisted identity** | `core/programs/runner/runner-describe.hirsel` (or `runner` query law harness) + `core/programs/store/store_law_schema.hirsel` | Literal pre-cutover `PROG-9` identity byte vectors (§2.4) for an unchanged application query (`store.Tx`), `query_as`/action (`runner.Caller`), schema (`store.Declare`, `store.Error`), slot selector (`store.Table`), and generic instantiations (`store.StoreId`, `store.Tx`+`store.Table`); resolves an old persisted key through Runner; records `code.revision` expectations separately for versioned indexes (`store-declare.hirsel:336`). | **L1, L3, L11, L12, L14** |
| **O2: Pre-`H` failure, ambiguous `H`, and post-`H` `CLAIM` failure** | `core/programs/store/bucket_stream_claim_laws.hirsel` | (a) Inject candidate-build allocation failure after step-5 final head check (definite pre-`H` refusal): assert `H` writes == 0, `Abandon` attempted, `reg` writes == 3, row cleared by rule (b), next claimant recovers. (b) Inject mid-step cancellation between external `claim_step` calls before `HSent` vs after `HSent`. (c) Distinguish ambiguous `H` (`HistoryUndecided`/`HistoryRefused` → row stays `Held` under rule (e), no abandon) vs applied `H` + failed step-7 `CLAIM` (row stays un-abandoned, single slot grant preserved, next claimant recovers cleanly). | **Lane 0** (part a) & **L8** (parts a–c) |
| **O3: Worker progress across `ObjectPause` settlement** | `core/programs/store/bucket_stream_exit_laws.hirsel` & `core/programs/store/object_journal_laws.hirsel` | Hold `ObjectPause` handover, drive settle to each outcome (`SettleApplied`, `SettleFenced`, `SettleNotApplied`, terminal error), verify session always returns on `resuming` and worker resumes or terminates with the specified result; verify covered prefix acked before `Fenced` and outstanding tickets mapped to `Undecided` on latch; compile-time guarantee via `SettleSession` lacking `Sender[ObjectJournalMsg]`. | **L8 & L9** |
| **O4: External visibility, borrow anchors, and compile-refusal fixtures** | `core/programs/runner/` downstream check + `core/programs/store/store_format10_laws.hirsel` + `store_tx_range_laws.hirsel` + `core/front/` refusal fixtures | Downstream package compiles and runs using only `store.*`; `range(tx, ...)` `in tx` and `row_bytes(row)` `in row` borrow anchors hold; opaque `PROG-2` refusal fixtures stay refused; frozen corpus bytes stay independent of production encoders. | **L1, L3, L11, L12, L14** |

---

## 10. Executable Cutover Sequence (Lane 0 + 14 Sequential Lanes)

**Sequential landing rule (correcting 3071 `MODULES.md:529`):** L5 (`pager`), L6 (`journal`), and L10 (`indexes`) are **NOT** parallel landing lanes — L6 consumes L5 replay work, constructors and root call sites overlap, and every lane edits shared `hirsel.toml` and `hirsel.laws`. A lane may stage a pure extraction draft once its inputs exist, but **each lane lands sequentially on the previous landing**, updating `hirsel.toml`, regenerating `hirsel.laws` (`hirsel laws write core/programs/store --libraries core`), running its proving laws (`--libraries core`), and passing `python3 tools/buck2/fmt.py --check`.

Each subsection below is a self-contained cutover spec executable by a single Sol lane.

---

### Lane 0 (Prerequisite Bug Fix): Pre-`H` Candidate-Build Abandon Guard

- **Goal:** Fix the existing live bug at `store-stream-claim.hirsel:585-606` where fallible candidate-`H` construction (`sealed` allocation, `history_claim_head`, `history_authority_end`, `authority` copy, `history_encode_register_head`) uses bare `?` after a hold row is acquired, bypassing `stream_claim_abandon`.
- **Files touched:** `core/programs/store/store-stream-claim.hirsel`, `core/programs/store/bucket_stream_claim_laws.hirsel`, `core/programs/store/hirsel.laws`.
- **Exact change:** Move candidate-`H` construction (`:585-606`) inside a pre-`H` fallible helper/scope whose `Error` branch calls `stream_claim_abandon(mut read_source.client, mut side, deadline)` before returning `Error(e)`. Keep `history_write_head` (`:607`) outside that abandon-on-error scope so `HistoryRefused` and `HistoryUndecided` continue leaving the row `Held` per rule (e). Coordinate with 3048's `ORIGIN` encoding if 3048 lands first.
- **Proving laws (O2a):** Extend `bucket_stream_claim_laws.hirsel` with a case injecting a candidate-build allocation failure after step-5 final `head` check (`bound bytes` or injected sealed-list limit) and asserting: `p1/head` PUT count == 0, `reg` PUT count == 3 (hold, abandon, rule-b unhold), no lingering `Held` row, and next claimant C succeeds cleanly.

---

### Lane 1 (L1): `status`, `types`, `format` + `PROG-9` Identity Lock

- **Goal:** Establish Layers 0–1 cleanly with zero cycles, pin all Tier A `PROG-9` nominal types at `store@1/store/*`, and freeze pre-cutover identity byte vectors.
- **Files created/moved:**
  - `status` (L0): keep `store-health-types.hirsel`; move `store-stream-types.hirsel` (delete duplicate `pub type StreamLatch = status.StreamLatch` at `:17`; keep root alias in `store-public-types.hirsel`).
  - `types` (L0): create `store-types.hirsel` (split from `store.hirsel` per §7), move `store-tiered-refusals.hirsel` (include 3062 `Shared | Scan` if not yet added), move pure limit values/checks from `store-limits.hirsel` (leaving `store_limits(db)` in `store`), move `EndpointRefusal` from `store-object-authority.hirsel:2`, `Position` from `store-levels.hirsel:92`, `PinOwner` from `store-projector.hirsel:849`, and `Options`/`Overrides`/`Kind`/`Shortage` from `sizes.hirsel` (keep `Options` fields aligned with 3076 when 3076 builds). Include internal mirrors `StoreKey`, `CallerId`, `BranchRef`, `LimitsSpec`, `ScanLimits`, `StoreError`.
  - `format` (L1): create `store-format.hirsel` (split from `store.hirsel` per §7), move `store-stream-frames.hirsel` (+ `stream_xindex_encode`/`decode` moved from `store-stream-gc.hirsel:120`), `store-tiered-frames.hirsel`, `store-stream-key.hirsel`, L1–L112 of `store-keyed-journal.hirsel`, `replay_within_limits` from `store-limits.hirsel:98`, and pure byte-level `store-commit-wire.hirsel` (`encode_data_ops_resolved` taking a scoped row emitter; leave `data_order` and prior lookup in `"store"` until L12).
  - `store` (L12): create `store-public-types.hirsel` holding Tier A pinned nominal `Error`, `Caller`, `StoreId`, `Branch`, `StoreLimits`, `Limits` (and zero-copy converters) plus Tier C aliases.
- **Rebase note:** Rebase onto 3048's Format 11 `ORIGIN` frame and `DISK_FORMAT = 11` before starting.
- **Proving laws & obligations:** `store_format10_laws` (or Format-11 successor, kept in `"store"` or split per §9.1 with frozen bytes independent of production encoders), `store_corpus_laws`, `store_size_laws`, `store_journal_laws`, `bucket_stream_laws`, **+ O1 (cross-build `PROG-9` identity byte-vector test in `runner` + `store` checking all 5 vectors in §2.4)**, **+ O4 (downstream compile check)**.

---

### Lane 2 (L2): `pages` (Capture Deferred to Step 7!)

- **Goal:** Land Layer 2 (`pages`) and `pages.HolderTable` on `Db`, moving pure tree/pin/holder operations without touching `capture` (deferred to L7).
- **Files created/moved:**
  - `pages` (L2): create `store-roots.hirsel` (`store.hirsel` `L28-L36`, `L99-L323`, `L411-L512` — **stop before `Tx` at `L513`** — plus `Pin` moved from `store-projector.hirsel:849`), create `store-keyed-pages.hirsel` (L114–L166 of `store-keyed-journal.hirsel`), move `store-pages.hirsel`, `store-layout.hirsel`, `store-tail.hirsel`.
- **Owned state & invariants:** Introduce `holders: pages.HolderTable` on `Db` owning all 8 fields (`heads`, `branch_names`, `pins`, `record_bytes`, `next_pin`, `next_branch`, `next_node`, `bases`). Single owner operations refresh `branch_names`, `bases`, and `record_bytes` (including keyed-root record bytes on commit) atomically. Prepare `DataRows.sorted` / `apply_collection_batch` hook for 3076 Perf #10.
- **Proving laws:** `store_pages_paged_laws`, `store_layout_laws`, `store_prior_laws`, `store_fork_laws`.

---

### Lane 3 (L3): `catalog`

- **Goal:** Land Layer 3 (`catalog`) and `catalog.SchemaState` on `Db`, keeping live `Db`/follower/export orchestration outside `catalog` and keeping Tier A catalog handle types pinned at `store@1/store/*`.
- **Files created/moved:**
  - `catalog` (L3): create `store-catalog.hirsel` (split per §7), move `store-handle-check.hirsel`, and pure schema/declaration/migration logic from `store-declare.hirsel` and `store-migration.hirsel` (`declare_from_base`, `complete_declaration`, `stage_create_table`, `stage_table`, `stage_migrate`).
  - Keep `seal`, `declare`, `declare_migration`, `finish_migration` in `store` (L12); keep `declare_follower` for L9 (`object_writer`) and `declare_export` for L13 (`transfer`).
  - Pin nominal `Declare`, `TableDecl[K, V]`, `Migration`, `StagedMigration`, `Table[K, V]`, `Index[J, K, V, P]`, `Cell[V]`, `Log[V]`, `Edge[A, B]`, `Readers[V]` in `store-public-types.hirsel` wrapping `catalog.*State`/`*Handle`.
- **Owned state & invariants:** Introduce `schema: catalog.SchemaState` (`fence`, `next_table`) on `Db` with explicit `propose_fence` / `restore_fence` (for activation rollback) and atomic `next_table` commit with `ReplaceMain`.
- **Proving laws & obligations:** `store_schema_laws`, `store_declare_laws`, `store_lookup_laws`, `store_law_schema`, **+ O1 (`store.Declare`, `store.Table`, `store.Cell`, `store.Log` `PROG-9` identity + versioned index `code.revision` check)**, **+ O4 (opaque handle `PROG-2` refusal check)**.

---

### Lane 4 (L4): `manifest`

- **Goal:** Land Layer 3 (`manifest`) and `manifest.PackDiskState` on `DiskState`, breaking all upward edges to `journal`, `pager`, and `history`.
- **Files moved:** All 13 manifest/pack/location/cut/retirement/frees/lineage/scrub files (`store-manifest.hirsel`, `store-manifest-wire.hirsel`, `store-list-read.hirsel`, `store-page-recovery.hirsel`, `store-locations.hirsel`, `store-packs.hirsel`, `store-pack-index.hirsel`, `store-cut-participants.hirsel`, `store-cut-retirement.hirsel`, `store-tiered-frees.hirsel`, `store-tiered-lineage.hirsel`, `store-scrub.hirsel`, `store-scrub-pace.hirsel`).
- **Cycle breaks:** Move `disk_cut_state` (`store-cut-participants.hirsel:168`) to root `store-coordinators.hirsel`. Replace `Option[View[HistoryLocator]]` and `TieredReadInventory` in `store-page-recovery.hirsel:73` with neutral `PackObjectRef` callback/map and `LoadedPackInventory`. Rebase cleanly on 3048's Format 11 `retirement_root`/`RetiredList` removal.
- **Owned state & invariants:** Introduce `packs: manifest.PackDiskState` (`pages`, `scrubs`) on `DiskState`; root coordinator refreshes `page_resident` from `packs` on open and cut.
- **Proving laws:** `store_manifest_page_laws`, `store_manifest_page_laws_2`, `store_locations_disk_laws`, `store_packs_disk_laws`, `store_scrub_laws`, `tiered_lineage_laws` (updating `[tests.runs]` paths for any moved white-box test).

---

### Lane 5 (L5): `pager`

- **Goal:** Land Layer 4 (`pager`), `pager.PagerState` on `Db`, and `pager.DiskPagerState` on `DiskState`, keeping `JournalReplay` assembly and `Tx`-bound adapters out of `pager`.
- **Files moved:** The 25 `store-tiered*.hirsel` files (`store-tiered.hirsel`, `packs`, `ranges`, `cache`, `pager`, `pager-batch`, `attempt`, `plan`, `point-read`, `count`, `scan`, `span`, `initial-changes`, `changes`, `pins`, `overlay`, `replay`, `fold`, `emission`, `publish`, `probe`, `probe-index`, `promote`, `compact`, `residency`). Leave `store-tiered-tx-read.hirsel` in `"store"` until L12 (`txn`), exposing `Tx`-independent paged read/count helpers from `pager`.
- **Cycle breaks:** `store-tiered-replay.hirsel` returns neutral `TieredManifestRestore` (leaving `JournalReplay` construction for L6 `journal`). `pager` wraps `manifest.LoadedPackInventory` into `TieredReadInventory`.
- **Owned state & invariants:** Introduce `pager: pager.PagerState` (`pager`, `pager_writer`, `park_notices`, `parked`, `writer_pin_bytes`) on `Db` and `tiered: pager.DiskPagerState` (`pager_task`, `tiered_dirty`, `restamp`, `unload`) on `DiskState`.
- **Proving laws:** `tiered_laws`, `tiered_read_laws`, `tiered_fold_laws`, `tiered_compact_laws`, `tiered_promote_laws`, `tiered_residency_laws`, `tiered_queue_laws`, `tiered_pager_batch_laws`, `tiered_attempt_laws`, `tiered_tx_read_laws`, `tiered_crash_arm_laws`.

---

### Lane 6 (L6): `journal`

- **Goal:** Land Layer 5 (`journal`), `journal.DirJournalState`, `journal.RecordRetention`, `DirJournalMsg`, and the common foreground queue/ack contract, routing Directory sends/cuts/close through `DirJournalState` while leaving Object traffic on its existing worker until L9.
- **Files created/moved:** Create `store-journal-io.hirsel` (split per §7), move `store-journal-worker.hirsel`, `store-journal-recover.hirsel` (+ `JournalReplay` assembly from `TieredManifestRestore`), and Directory/common queue helpers from `store-journal-status.hirsel`.
- **Owned state & invariants:**
  - Introduce `retention: journal.RecordRetention` (`record_cut`, `recorded_after`) on `DiskState`.
  - Introduce `journal.DirJournalState` (owning non-optional `lock: HostFile`, `d: Dir`, `creation`, `file_seq`, `file_end`, `DirJournalMsg` channel, `DirAck` receiver, `QueueAccounting` keeping `uncovered` distinct from `since_cut`, `written*`, `synced`, `poison`, `pending`, `journal_task`, and slot ready for 3076's `coalesce_timer_task`/`end_timer_task`).
  - Preserve **writer recovery fence** (`LOCK` before recovery, `prev_end` vs `END` torn-tail rule, orphan-END removal fsync, `close` holding `LOCK` through worker `task.join`) AND expose **`disk_recover_locked`** for 3079 offline backup (§5.3).
  - Unblocks 3076 Perf #8 (after Astra review 3100).
- **Proving laws:** `store_journal_laws`..`store_journal_laws_7`, `store_disk_laws`, `store_disk_laws_2`, `store_crash_laws`, `store_ace_crash_laws`, `store_replay_laws`, `store_sim_disk_laws`.

---

### Lane 7 (L7): `history` + Deferred `capture` *(may land as two commits: 7a `history`, 7b `capture`)*

- **Goal:** Land Layer 6 (`history` and `capture`) on their completed L1–L6 dependencies, keeping `verify_release`/`repair_indexes` out of `capture` and placing 3062's `SharedCaptureState` in `capture`.
- **Files created/moved:**
  - **7a (`history`, Layer 6):** Move `store-history.hirsel`, `store-history-read.hirsel`, `store-history-witness.hirsel`, `store-history-packs.hirsel`, `store-register.hirsel`, `store-object-authority.hirsel` (minus `EndpointRefusal`), `store-object-cut.hirsel`, plus create `store-history-open.hirsel` (`HistoryOpenBase`, `history_load_admitted`, `history_check_open` split from `store-object-open.hirsel`). Preserve 3048 `ClaimAuth`/`ORIGIN`/`HeadView` and fold/coordinate 3082 pack cache hash-verify + refetch.
  - **7b (`capture`, Layer 6):** Move `store-capture.hirsel`, `store-capture-check.hirsel`, `store-capture-check-journal.hirsel`, `store-capture-journal.hirsel`, `store-capture-pages.hirsel`, `store-capture-catalog.hirsel`, `store-capture-describe.hirsel`, and L1–L71 (`catalog_entries`) of `store-inspect.hirsel` (leaving `verify_release`/`repair_indexes` in `"store"` until L14 `maintenance`). Preserve §3.3b live capture fence (retained fds, shared terminal fence, non-blocking `LOCK` probe, 1 ms poll, O1 `sync_dir`, exact `MANIFEST` byte check). Place 3062 `SharedCaptureState`/`SharedBacking`/`open_shared`/`share_capture`/`freeze_roots_to_shared`/`shared_visit_leaves` here (unblocking 3062 #2 build). Return neutral `CapturedState` to callers.
- **Proving laws:** `authority_laws`, `bucket_laws`, `bucket_order_laws`, `bucket_q5_laws`, `object_cut_laws`, `location_laws`, `store_capture_laws` (including 3079 offline backup laws if landed), `store_crash_laws`.

---

### Lane 8 (L8): `host_stream` (`ClaimMachine[T]`, `RegisterLane`, `HoldTracker`, `SettleSession`)

- **Goal:** Land Layer 7 (`host_stream`) with the complete v2 `ClaimMachine[T]`, complete `RegisterLane`/`HoldTracker`, non-reentrant `SettleSession`, and plain `StreamFollowMaterial`, updating the existing object worker, claim callers, and slot relay in the same landing before L9 switches `DiskState` to `Backend`.
- **Files moved:** All 10 stream files (`store-stream-claim.hirsel`, `store-stream-service.hirsel`, `store-stream-slot.hirsel`, `store-stream-task.hirsel`, `store-stream-poll.hirsel`, `store-stream-settle.hirsel`, `store-stream-gc.hirsel` minus `stream_xindex_encode`, `store-stream-janitor.hirsel`, `store-stream-follow.hirsel`, `store-stream-archive.hirsel`).
- **Key structural changes:**
  - Implement `ClaimMachine[T]` with owned per-phase payloads, 3048 `ClaimAuth` gate before any write/`TERM`, retained `granted`, step-5 `head_tag`, pre-I/O `HSent` recording, 4 distinct outcomes (`ConflictRestart`, `ConflictLost`, `AmbiguousH`, `ClaimPublishFailed`), and safe `claim_abort` (§6.2).
  - Implement complete `RegisterLane` (with `pending`, `stalled`) and `HoldTracker` (with `observed_rows`, `tickets`, `active_settles`, `stall_polls`, `next_settle_id`, `last_reg_ns`), stale-reply rejection, and bounded-outbox `unsent` retry (§6.3).
  - Refactor `stream_settle`, `stream_settle_sealed`, and `stream_poll_head` to take non-reentrant `SettleSession` (zero access to `Sender[ObjectJournalMsg]`, §5.2).
  - Return plain `StreamFollowMaterial` from `stream_follow_capture` (no `FollowCapture` dependency).
- **Proving laws & obligations:** `bucket_stream_laws`, `bucket_stream_service_laws`, `bucket_stream_claim_laws`, `bucket_stream_unhold_laws`, `bucket_stream_election_laws`, `bucket_stream_gc_laws`, `bucket_stream_janitor_laws`, `bucket_stream_direct_follow_laws`, `bucket_stream_exit_laws`, **+ O2 (pre-`H` failure, step-boundary cancel before/after `HSent`, ambiguous `H`, post-`H` `CLAIM` failure)**, **+ O3 (worker progress across `ObjectPause` settle outcomes)**.

---

### Lane 9 (L9): `object_writer` + `DiskState.Backend` Sum Cutover

- **Goal:** Land Layer 8 (`object_writer`), move worker-side export fetch into `object_writer`, and switch `DiskState` atomically to `Backend = DirBackend(journal.DirJournalState) | ObjectBackend(object_writer.ObjectWriterState)`.
- **Files moved:** `store-object-open.hirsel` (writer startup glue), `store-object-journal.hirsel` (+ `ObjectExportLease`, `ObjectExportFetch`, `object_export_read` moved from `store-object-export.hirsel`), `store-object-writer.hirsel`, `store-reclaim.hirsel`, `store-sim-levels.hirsel`, `store-levels.hirsel` (minus `Position` and root `durable_at`), `store-health.hirsel`, `store-followers.hirsel` (exposing `follower_tip_data` + `declare_follower_state`, no `read.Snapshot`), `store-follower-service.hirsel`, `store-object-follower.hirsel`.
- **Owned state & invariants:** Complete `ObjectWriterState` (§4.4 row 34, including `authority_kind`, `head_entry_seq`, `draft_offset`, `export_lifetime`, `archive_on`, `pending_archive_on`, `attached`, `stream`, `has_sealed`, `drop_attach_on_cut`, `failed`, `weak`, `journal_idle_task`) and `ObjectJournalMsg`. Switch constructors (`open`, `open_authority`), dispatch (`handoff`, `durable`, `cut_install`, `fresh_snapshot`, `archive`), and `close` onto `Backend` in one atomic cutover. Pin nominal `store.Follower` in `store-public-types.hirsel`. Preserve 3081 `platform.local` standalone + streamed cases and 3082 pack cache.
- **Proving laws & obligations:** `object_journal_laws`, `store_reclaim_laws`, `follower_laws`, `follower_crash_laws`, `replication_laws`, `assembly_laws`, **+ O3 (full `ObjectPause` handover + `StreamAnswer` covered-prefix-before-`Fenced` + latch-to-`Undecided` verification)**, plus `platform` standalone/streamed compile+smoke if 3081 landed.

---

### Lane 10 (L10): `indexes`

- **Goal:** Land Layer 4 (`indexes`) and `indexes.IndexState` on `Db`, keeping `Tx` edit application out of `indexes`.
- **Files moved:** `store-index-registry.hirsel` (pure registry/extractor planning/drift; leave `Tx` `apply_edits` in `"store"` until L12) and `store-aggregate.hirsel`.
- **Owned state & invariants:** Introduce `indexes: indexes.IndexState` (`indexes`, `drift`) on `Db` with explicit owner installation methods called by root `seal` and `install_activation`. Pin nominal `store.Aggregate[G]` in `store-public-types.hirsel`.
- **Proving laws:** `store_lookup_laws`, `store_index_crash_laws`, `store_put_laws_2`.

---

### Lane 11 (L11): `read`

- **Goal:** Land Layer 9 (`read`) over neutral capture/follower data, integrating 3062's read-path symbols (`shared_read_only`, `follower_share`, `VisitStep`, `RowViewState`, `visit_scan*`, `row_*`) while leaving `Tx` range/count adapters for L12.
- **Files moved:** `store-read.hirsel` (minus `Tx` `range`/`range_reverse`/`range_prefix`/`count`/`TxRows`, left in `"store"` until L12), `store-read-pages.hirsel`, `store-changes.hirsel`.
- **Key contracts:** Construct `SnapshotState`, `ReaderState`, and `ReadOnlyState` from `capture.CapturedState`, `object_writer.FollowerTipData`, and `capture.SharedCaptureState`. Pin nominal `store.Snapshot`, `store.Reader`, `store.ReadOnly`, `store.SharedCapture` *(3062)*, `store.RowView[K, V]` *(3062)*, and `store.Visit` *(3062)* in `store-public-types.hirsel` (keeping `row_key_parts`/`row_bytes`/`row_current_payload` `in row` borrow anchors intact!).
- **Proving laws & obligations:** `store_read_laws`, `store_cursor_laws`, `store_changes_laws`, `store_changes_page_law`, `store_diff_laws`, **+ O1 (`store.ReadOnly` identity check)**, **+ O4 (borrow anchor & external visibility check)**.

---

### Lane 12 (L12): `txn`

- **Goal:** Land Layer 10 (`txn`), `txn.AccountingState` and `txn.CallStreamState` on `Db`, moving all `Tx` operations (including `Tx` range/count, `Tx` tiered reads, `Tx` index edit application, `data_order`/prior resolution, 3062 `visit_range*`, and 3076 `put_ordered`) while keeping the foreground commit/cut/3075-skip coordinator in root `store-coordinators.hirsel`.
- **Files created/moved:** Create `store-txn-core.hirsel` (split per §7) and `store-tx-range.hirsel` (`Tx` range/count from `store-read.hirsel:466-560` + `apply_edits` from `store-index-registry.hirsel`), move `store-tx.hirsel`, `store-commit.hirsel` (+ `data_order`/prior lookup from `store-commit-wire`), `store-attempts.hirsel`, `store-mux.hirsel`, `store-fork.hirsel`, `store-cell.hirsel`, `store-log.hirsel`, `store-edge.hirsel`, `store-streams.hirsel`, `store-keyed-claims.hirsel` (+ L168–L182 of `store-keyed-journal`), `store-keyed-lifecycle.hirsel`, `store-reexec.hirsel`, and `store-tiered-tx-read.hirsel`.
- **Owned state & invariants:**
  - Introduce `accounting: txn.AccountingState` (7 fields, with **mutable `ceiling`** via `reserve_ceiling`/`restore_ceiling`!) and `call_streams: txn.CallStreamState` (6 fields, carrying policy copies for memory opens) on `Db`.
  - Pin nominal `store.Tx`, `store.TxRows[K, V]`, `store.Pending` in `store-public-types.hirsel`, keeping `pub fn range(...) in tx` and `pub fn fetch_bytes(...) in tx` root-scoped borrow anchors intact!
  - Preserve 3075's empty-commit skip (`Done.info: Option[CallInfo]`, head-fence probe on Bucket/stream, no cut/increment) across every caller (`run`, `run_on`, `run_with_reply`, `store_body_at`).
- **Proving laws & obligations:** `store_put_laws`, `store_put_laws_2`, `store_tx_range_laws`, `cell_log_laws`, `store_claim_crash_laws`, `store_claim_quota_laws`, `store_reexec_laws`, `store_swarm_laws`, `store_trust_laws`, **+ O1 (`store.Tx` Runner query/action `PROG-9` identity + Runner L61/L91 smoke)**, **+ O4 (`in tx` borrow anchor + downstream `runner` build)**.

---

### Lane 13 (L13): `transfer`

- **Goal:** Land Layer 11 (`transfer`) and `transfer.TransferState` on `DiskState` against the settled cut and `object_writer` export-fetch contracts.
- **Files created/moved:** Create `store-transfer-core.hirsel` (split per §7), move `store-export.hirsel`, `store-object-export.hirsel` (copy/orchestration half; worker fetch already moved to `object_writer` in L9), `store-object-import.hirsel`, `store-prepared-check.hirsel`, `store-catchup.hirsel`, `store-rounds.hirsel`, `store-install.hirsel`, `store-activation.hirsel`, plus `declare_export`/`finish_export_declaration` orchestration.
- **Owned state & invariants:** Introduce `transfer: transfer.TransferState` (`lease`, `steps`, `imported`, `reserving`, `target`) on `DiskState`. Wire `reserve_import` → `AccountingState.reserve_ceiling`, `forget_import` → `AccountingState.restore_ceiling(options.resident_limit)` + `ObjectWriterState.clear_export_lifetime`, and `install_activation` → `SchemaState.propose_fence`/`restore_fence` + root `IndexState`/`ProjectorState` install. Preserve 3079's T5 listed-packs-only `restore` and damaged-backup mutilation oracle. Pin nominal `Preparation`, `ValidatedImport`, `Imported`, `ImportWork` in `store-public-types.hirsel`.
- **Proving laws:** `store_backup_laws`, `store_catchup_laws`, `store_crash_install_laws`, `import_read_laws`.

---

### Lane 14 (L14): `maintenance` + Root `store` Completion

- **Goal:** Land Layer 11 (`maintenance`), `maintenance.ProjectorState` on `Db`, finalize the split root module `"store"`, and verify end-state hygiene.
- **Files created/moved:**
  - `maintenance` (L11): move `store-index-build.hirsel`, `store-verify.hirsel`, `store-expiry.hirsel`, `store-projector.hirsel` (minus `Pin` already in L2), and create `store-operator-verify.hirsel` (`verify_release` & `repair_indexes` from `store-inspect.hirsel:73-176`).
  - `store` (L12): finalize `store.hirsel`, `store-public-types.hirsel` (including pinned `Delta[K, V]`, `Merge`, `Fold`), `store-coordinators.hirsel`, `store-attention.hirsel`, and `sizes.hirsel` (coordinate 3080 `stats`/`inspect` counter display and 3076 `writer_plan`).
  - Measure actual line counts of every file in `core/programs/store/`; only update/remove `store.hirsel` in `tools/file_length_ratchet.json` once measured under the limit.
- **Proving laws & obligations:** `store_projector_laws`, `store_index_crash_laws`, `store_trust_laws`, **+ O1 (full cross-build `PROG-9` identity verification across all 5 vectors + Runner key resolution)**, **+ O4 (full downstream `runner`/`platform`/`vector`/`search` compile & `fmt.py --check`)**.

---

## 11. Prior Art From Peer Storage Engines

All citations verified directly against local clones in `/tmp/ref-*`:

| Peer Engine (Clone & Commit) | Boundary Architecture & Citations | Verdict | Wins, Weaknesses & How Hirsel Uses It |
|---|---|---|---|
| **redb** (`/tmp/ref-redb`, `2fc790d`) | `Database` (`src/db.rs:798-803`) is a 3-field composition root (`mem`, `transaction_tracker`, `closed`). Acyclic stack: `StorageBackend` → `page_store` (`TransactionalMemory`) → `tree_store` (`Btree` + `TableTree`) → `transactions` (`WriteTransaction`) → `db` façade. | **ADOPTED** | **Win:** Tiny composition root with single-owner subsystem handles. **Weakness:** `WriteTransaction` (`src/transactions.rs:935-957`) holds 18 raw fields. **Hirsel:** Adopts thin composition roots (`Db`/`DiskState`) while keeping `TxState`, `AccountingState`, and `CallStreamState` cleanly separated and pinning root nominal `PROG-9` types at Layer 12. |
| **fjall** (`/tmp/ref-fjall`, `8700604`) | Splits `DatabaseInner` (`src/db.rs:35-61`) from `SupervisorInner` (`src/supervisor.rs:16-36`), with dedicated `journal/`, `flush/`, `compaction/`, `keyspace/`, `poison.rs` (`src/poison.rs:8-41`). | **ADAPTED** | **Win:** Dedicated `journal/manager.rs` and `PoisonSignal` isolate WAL retention and poison state. **Weakness:** Mutual `Arc` cycles between `Supervisor` and `Keyspace` (`src/db.rs:82-93`). **Hirsel:** Enforces a strictly acyclic 14-layer DAG so cyclic ownership cannot form. |
| **SQLite** (`/tmp/ref-sqlite`, `795e28a`) | Strict opaque-header stack: VFS → WAL (`src/wal.h:53-75`) → Pager (`src/pager.h:12-43`) → B-Tree (`src/btree.h`) → VDBE (`src/vdbe.h`). | **ADOPTED (improved)** | **Win:** WAL/format changes stay isolated from B-Tree and execution. **Weakness:** `struct Pager` (`src/pager.c:619-654+`) grew to ~80 fields. **Hirsel:** Splits Pager into `manifest` (packs/locations), `pager` (Tiered cache/eviction), and `journal` (WAL segments) enforced at compile time via `pub opaque type` (`PROG-2`). |
| **TigerBeetle** (`/tmp/ref-tigerbeetle`, `a2a3c706`) | Separates VSR WAL (`src/vsr/journal.zig`), block Grid (`src/vsr/grid.zig:30-85`), LSM Forest/Manifest (`src/lsm/forest.zig:30-90`), and deterministic `StateMachineType(Storage)` (`src/state_machine.zig:223-258`) via capability injection. | **ADOPTED** | **Win:** Explicit capability injection enables deterministic simulation without passing storage-root god objects or re-entering workers. **Hirsel:** Uses Hirsel `scoped fn` ports in `ClaimMachine[T]` and non-reentrant `SettleSession` (zero access to worker queues). |
| **FoundationDB** (`/tmp/ref-foundationdb`, `58f80b1`) | Separates durability log (`TLogServer`), commit proxy (`CommitProxyServer`), sequencer, and storage engine (`StorageServer`). | **ADAPTED** | **Win:** Orthogonal evolution of commit coordination, log durability, and storage layout. **Hirsel:** Preserves the same separation in-process via acyclic modules and a single foreground coordinator (`store-coordinators.hirsel`). |
