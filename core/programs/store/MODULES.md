# Hirsel Store Module Architecture (`store@1`)

**Status:** Design only (no code changed). Normative architecture document for `core/programs/store/` (checked against `origin/main` `f5eb79cb`).  
**Source findings:** Complexitysmash findings #1–#4 (`COMPLEXITYSMASH.md`).  
**Wholehog principle:** This document specifies the clean end state directly — no compatibility shims, no stepping-stone dual APIs, no cyclic module graphs. Every cutover lane lands its final module seam and owned subsystem state in one pass.

---

## 1. Executive Summary

Today `core/programs/store/hirsel.toml` assigns 123 of its 125 source files (49,000+ lines) to the single module `"store"` (only `store-health-types.hirsel` = `"status"` and `main.hirsel` = `"main"` sit outside it). As a result:

1. Every subsystem can read and mutate every other subsystem's private types and fields.
2. `Db` (`store.hirsel:40-92`, 39 fields) and `DiskState` (`store.hirsel:3695-3736`, 34 fields) are god objects that accrete state for every feature, and `DiskState` interleaves Directory (`log@1`) and Object (`objects@1`/`sim@1`) optional fields instead of separating them behind a sum type.
3. `store.hirsel` (4,846 lines) mixes transaction execution (`:1075-1395`), persisted binary encoding (`:1683-1763`, `:2499-2854`, `:4146-4442`), schema catalog mechanics (`:1764-2412`), directory startup/cuts (`:3386-4145`), and export/import preparation (`:2855-3385`).
4. The Bucket host-stream claim lifecycle (§12.11) is scattered across `store-stream-claim.hirsel`, `store-stream-service.hirsel`, `store-stream-slot.hirsel`, `store-stream-task.hirsel`, `store-stream-poll.hirsel`, and `store-stream-settle.hirsel`, threading 11–17 overlapping arguments through every phase and mutating `StreamLane` and `ObjectJournalSession` internals directly.

The end state partitions `core/programs/store` into **19 strictly acyclic modules** (18 library modules including the existing `status` and root façade `store`, plus `main`), shrinks `Db` from 39 fields to **13** (6 root/shared fields + 7 opaque subsystem states) and `DiskState` from 34 fields to **8** (3 root fields + 4 opaque subsystem states + 1 `Backend` sum), splits `store.hirsel` from 4,846 lines to ~550 lines, and encapsulates the entire §12.11 claim and hold lifecycle behind a narrow `ClaimMachine` + `RegisterLane` + `HoldTracker` API in `host_stream`.

External packages (`runner`, `platform`, `search`, `vector`, user programs) see **zero breaking changes**: root module `store` preserves all 372 external `store.*` names and `PROG-9` `StaticFn` identities via transparent `pub type` aliases and thin `pub fn` forwarders.

---

## 2. Module Map and Acyclic Dependency Graph

### 2.1 Hirsel Language Constraints Verified

Every module name in this map has been verified against Hirsel's `LANG-7` and `PROG-2` rules:

- **Zero `LANG-7` root-member collisions:** No module name matches any of the 339 lowercase `pub` functions or constants in module `store` (specifically avoiding `index`, `follow`, `followers`, `projector`, `reader`, `table`, `branch`, `pin`, `fork`, `health`, `levels`, `limits`, `memory`, `open`, `close`, `run`, `serve`, `verify`).
- **Zero accidental core/std module extensions:** No module name matches any `core` module (`core`, `list`, `map`, `string`, `bytes`, `cell`, `log`, `arc`, `rc`, `channel`, `conn`, `task`, `latest`, `code`, `host`, `order`, `region`, `span`, `text`, `simd`) or `std` module (`codec`, `stream`, `file`, `dir`, `object`, `page`, etc.).
- **Opaque invariants (`PROG-2`):** Every owned subsystem state struct is `pub opaque type` in its owning module, so `store` and sibling modules cannot construct, destructure, or read its fields directly.

### 2.2 Strict Acyclic Layering (19 Modules)

```text
Layer 0:   status   types
             \       /
Layer 1:      format                      (persisted wire & frame codec)
                 |
Layer 2:       pages                      (in-memory persistent page trees & appliers)
              /     \
Layer 3:  catalog   manifest              (schema catalog; packs, locations, manifest, scrub)
           |   \    /    |
Layer 4:   |  indexes  pager    journal   (sync index registry; tiered pager; log@1 WAL)
           |     |      /  \    /  |
Layer 5:   |     |  capture history       (verified dir capture; stateless objects@1 protocol)
           |     |    |       |
Layer 6:   |     |    |  host_stream      (§12.11 stream service + ClaimMachine API)
           |     |    |       |
Layer 7:   |     |    | object_writer     (objects@1/sim@1 writer, reclaim, levels, followers)
           |     |    \     /
Layer 8:   |     |     read               (Snapshot, Reader, cursors, scoped leaf visitor, changes)
           |      \    /  |
Layer 9:   \-------txn----/               (Tx execution, commit, attempts, mux, fork, call streams)
                  /   \
Layer 10:  transfer   maintenance         (export/import/catchup/activation; backfill/verify/expiry/projectors)
                \       /
Layer 11:         store                   (thin Db/DiskState composition root, open/close/cuts, public façade)
                    |
Layer 12:         main                    (workload entry)
```

**No cycles exist anywhere in Layers 0 → 12.** Every allowed call edge strictly descends in layer number (and never crosses laterally within the same layer except `types -> status` within Layer 0 and `pager -> manifest` in Layer 4 → 3).

### 2.3 Six Structural Cuts That Eliminate Current Bidirectional Cycles

Static symbol-graph analysis of `core/programs/store/` at `39c2f235` identified every existing mutual dependency; the end-state removes all six structurally:

1. **Extract leaf types, constants, and error/allocation helpers into `types` and `status`:** Move `META_LIMIT`, `PAGE_LIMIT`, `BLOB_LIMIT`, `KEY_LIMIT`, `MIB`, `disk_fault`, `disk_allocation`, `capacity`, `raised_capacity`, `round8`, `same`, `named`, `Pin`/`PinOwner`, `Options`, `StoreLimits`, and `Kind` out of `store-manifest.hirsel`, `sizes.hirsel`, `store-index-registry.hirsel`, and `store.hirsel`. Move `sizes.hirsel` to Layer 11 (`store`) where it reads constants from every subsystem without pulling `manifest` upward.
2. **Consolidate all persisted binary codecs into `format` (Layer 1):** Move frame sealing/headers (`history_append_header`, `history_seal`, `history_frame`, `history_number`), `ATTACH`/`REG`/`BATCH`/`TERM`/`XINDEX` wire frames, `ORIGIN` head frames (lane 3048), `COMMIT`/`FORK`/`CALL`/settlement/keyed-edit wire codecs (`store-stream-key.hirsel` lines 1–55, `store-keyed-journal.hirsel` lines 1–112, `store-commit-wire.hirsel`), `TieredFrame` wire codec, and `Catalog` binary encoding into `format`. This breaks `format <-> history`, `format <-> txn`, and `format <-> pager`.
3. **Separate pure page-tree substrate (`pages`) from on-disk packs (`manifest`), cold-page paging (`pager`), and high-level snapshot diffs (`read`):** Move pure page-tree mutation appliers (`apply_cell`, `apply_log_append`, `trim_log_page`, `materialize_keyed_edits`, `apply_collection_batch`) into `pages`; move manifest tree codecs (`store-tiered-frees.hirsel`, `store-tiered-lineage.hirsel`) into `manifest`; move `store-changes.hirsel` (which diffs both Memory and Tiered collections) up to `read`. This yields the strict linear chain `types -> format -> pages -> manifest -> pager -> read`.
4. **Split `JournalMsg` into `journal.DirJournalMsg` and `object_writer.ObjectJournalMsg`:** Today `JournalMsg` carries Directory variants (`Frame`, `Rotate`, `Barrier`) alongside Object/Stream/Export variants (`ObjectClose`, `ObjectFresh`, `ObjectExportRead`, `ObjectPause`, `StreamAnswer(SlotAck)`), forcing `journal <-> host_stream`, `journal <-> object_writer`, and `journal <-> transfer`. Splitting the message enum and replacing `DiskState`'s overlapping optional fields with `Backend = DirBackend(journal.DirJournalState) | ObjectBackend(object_writer.ObjectWriterState)` eliminates all three cycles.
5. **Split stateless `history` below `host_stream`, and invert `host_stream -> object_writer` via capability ports:** `store-history*.hirsel`, `store-register.hirsel`, `store-object-authority.hirsel`, and `store-object-cut.hirsel` form stateless `history` (Layer 5). `host_stream` (Layer 6) owns the §12.11 protocol and `ClaimMachine`, taking narrow scoped capability ports (`ClaimPorts`, `SettlePort`, janitor `open_and_close` callback) instead of importing `ObjectJournalSession`, `Authority`, or `FollowCapture`. `object_writer` (Layer 7) supplies those ports and wraps `Authority`/`FollowCapture`.
6. **Split synchronous index maintenance (`indexes`, Layer 4) from stepped background jobs (`maintenance`, Layer 10):** `indexes` holds `IndexRegistry`, `IndexDrift`, extractor execution, and `Aggregate` accounting (called synchronously by `txn`). Background drivers that run transactions, snapshots, or followers (`store-index-build.hirsel`, `store-verify.hirsel`, `store-expiry.hirsel`, `store-projector.hirsel`) live in `maintenance` (Layer 10) above `read` and `txn`. Similarly, `Catalog` row types and `Table`/`Index`/`Cell`/`Log`/`Edge`/`Readers` handles live in `catalog` (Layer 3), below `indexes`, `capture`, `read`, and `txn`.

### 2.4 Complete Module Directory (All 125 Existing Files + Split Files)

Every single file in `core/programs/store/hirsel.toml` is assigned to exactly one module below. Files marked *(split from `store.hirsel`)* are the new files created by Section 4.

| # | Module | Layer | Files Assigned (Total: 125 existing + 7 split = 132 files) | Allowed Callees (Direct) | Narrow Public Surface |
|---|---|---|---|---|---|
| 1 | `status` | 0 | `store-health-types.hirsel`, `store-stream-types.hirsel` | *(none)* | `Health`, `StreamLatch`, `HostStreamId`, `StreamField`, `StreamRefusal`, `stream_refusal_text`, `stream_latch_text` |
| 2 | `types` | 0 | `store-limits.hirsel`, `store-tiered-refusals.hirsel`, `store-types.hirsel` *(split)* | `status` | `StoreId`, `StorageKind`, `Caller`, `Limit`, `Why`, `Fault`, `StoreCapacity`, `Error`, `Branch`, `CallId`, `CallInfo`, `Mark`, `Ticket`, `Read`, `StoreFailure`, `Outcome`, `Undone`, `Done`, `Bound`, `Limits`, `Resume`, `Page`, `Resumed`, `Looked`, `Seen`, `Change`, `CallRecord`, `StopKind`, `Recorded`, `Plan`, `CallReserve`, `ServeError`, `Reserve`, `Receipt`, `Stop`, `RequestDigest`, `RowSchemas`, `IndexKind`, `Pin`, `PinOwner`, `StoreLimits`, `Options`, `Overrides`, `Kind`, `Shortage`, `WriterPlan`, `TieredRefusal`, `ClaimAuth` *(3048)*, error/allocation helpers (`disk_fault`, `disk_allocation`, `capacity`, `raised_capacity`, `same`, `named`, `round8`, `check_limits`, `journal_floor`) |
| 3 | `format` | 1 | `store-format.hirsel` *(split)*, `store-commit-wire.hirsel`, `store-stream-frames.hirsel`, `store-tiered-frames.hirsel`, `store-stream-key.hirsel`, `store-keyed-journal.hirsel` *(wire half)* | `status`, `types` | `DISK_FORMAT`, `BINDING_BYTES`, `DiskContext`, `DiskFrame`, `DiskHeader`, `DiskFrameHead`, `seal_frame`, `disk_read_frame*`, `disk_header_bytes`, `disk_read_header`, `encode_staged`, `encode_fork`, `encode_call*`, `encode_keyed_edits`, `decode_keyed_edits`, `journal_decode*`, `JournalEvent`, `JournalCommit`, `JournalFork`, `history_append_header`, `history_seal`, `history_frame`, `Attach`, `SealedTail`, `StreamRef`, `StreamRegister`, `RegRow`, `XIndex`, `BatchSlice`, `BatchRow`, `stream_*_encode`/`decode`, `OriginFrame` *(3048)*, `TieredFrame`, `read_tiered_frame`, `StreamId`, `StreamKey`, `caller_key`, `stream_key_bytes`, `parse_stream_key`, `Catalog` wire codec |
| 4 | `pages` | 2 | `store-pages.hirsel`, `store-layout.hirsel`, `store-tail.hirsel`, `store-roots.hirsel` *(split)*, `store-keyed-pages.hirsel` *(applier half of `store-keyed-journal`)* | `types`, `format` | `PageTree`, `PageRoots`, `Roots`, `Head`, `Tip`, `State`, `HolderDelta`, `ProposedMain`, `HolderTable` *(opaque)*, `DataOp`, `StagedValue`, `DataRows`, `DataOps`, `Staged`, `TailBases`, `CollectionCursor`, `CollectionDescriptor`, `PageRange`, `put_page_row`, `remove_page_row`, `bulk_collection`, `drop_collection`, `canonical_roots`, `apply_cell`, `apply_log_append`, `trim_log_page`, `materialize_keyed_edits`, `apply_collection_batch`, `state_with_delta`, `install_holder_delta`, `read_tail` |
| 5 | `catalog` | 3 | `store-catalog.hirsel` *(split)*, `store-handle-check.hirsel`, `store-declare.hirsel`, `store-migration.hirsel` | `types`, `format`, `pages` | `Catalog`, `CatalogRecord`, `ValueSchema`, `IndexRecord`, `IndexState`, `Readers[V]` *(opaque)*, `Table[K, V]` *(opaque)*, `Index[J, K, V, P]` *(opaque)*, `Cell[V]` *(opaque)*, `Log[V]` *(opaque)*, `Edge[A, B]` *(opaque)*, `SchemaState` *(opaque)*, `SchemaPlan`, `Declare`, `Mode`, `TableDecl`, `Migration`, `StagedMigration`, `CollectionKind`, `CollectionInfo`, `catalog_handle`, `catalog_index_handle`, `readers`, `row_value`, `stage_create_table`, `stage_table`, `stage_drop_table_key`, `stage_migrate`, `declare`, `seal` *(taking cut callback)*, handle validity checks |
| 6 | `manifest` | 3 | `store-manifest.hirsel`, `store-manifest-wire.hirsel`, `store-list-read.hirsel`, `store-page-recovery.hirsel`, `store-locations.hirsel`, `store-packs.hirsel`, `store-pack-index.hirsel`, `store-cut-participants.hirsel`, `store-cut-retirement.hirsel`, `store-tiered-frees.hirsel`, `store-tiered-lineage.hirsel`, `store-scrub.hirsel`, `store-scrub-pace.hirsel` | `types`, `format`, `pages` | `PackDiskState` *(opaque)*, `PageDisk`, `PageManifest`, `ValidatedPageManifest`, `LoadedPageStore`, `PageLocations`, `PagedLists`, `CutParticipants`, `PagedCutLayout`, `PackRun`, `DiskIndex`, `Lineage`, `DeferredFree`, `Scrub`, `ScrubConfig`, `ScrubRound`, `disk_read_page_manifest`, `disk_publish_manifest`, `plan_page_cut`, `cut_retirement`, `attach_page_graph`, `validate_directory_graph`, `begin_scrub`, `scrub_step`, `scrub_pacer_*`, `record_scrub_round` |
| 7 | `pager` | 4 | `store-tiered.hirsel`, `store-tiered-packs.hirsel`, `store-tiered-ranges.hirsel`, `store-tiered-cache.hirsel`, `store-tiered-pager.hirsel`, `store-tiered-pager-batch.hirsel`, `store-tiered-attempt.hirsel`, `store-tiered-plan.hirsel`, `store-tiered-point-read.hirsel`, `store-tiered-count.hirsel`, `store-tiered-scan.hirsel`, `store-tiered-span.hirsel`, `store-tiered-tx-read.hirsel`, `store-tiered-initial-changes.hirsel`, `store-tiered-changes.hirsel`, `store-tiered-pins.hirsel`, `store-tiered-overlay.hirsel`, `store-tiered-replay.hirsel`, `store-tiered-fold.hirsel`, `store-tiered-emission.hirsel`, `store-tiered-publish.hirsel`, `store-tiered-probe.hirsel`, `store-tiered-probe-index.hirsel`, `store-tiered-promote.hirsel`, `store-tiered-compact.hirsel`, `store-tiered-residency.hirsel` | `types`, `format`, `pages`, `manifest` | `PagerState` *(opaque)*, `DiskPagerState` *(opaque)*, `TieredPagerMessage`, `TieredPagerLink`, `TieredPins`, `TieredTouchSet`, `TieredTarget`, `TieredDataDelta`, `TieredDirty`, `TieredCutStage`, `TieredReadInventory`, `TieredCompaction`, `start_pager`, `stop_pager`, `park_notices`, `park_enqueue`/`poll`/`cancel`, `read_tiered_point*`, `tiered_scan*`, `tiered_tx_*`, `tiered_changes`, `tiered_fold_cut`, `tiered_publish_cut`, `tiered_compact_cut`, `plan_residency_move` |
| 8 | `journal` | 4 | `store-journal-worker.hirsel`, `store-journal-status.hirsel` *(dir half)*, `store-journal-recover.hirsel`, `store-journal-io.hirsel` *(split)* | `types`, `format`, `pages`, `manifest`, `pager` | `DirJournalState` *(opaque)*, `DirJournalMsg`, `DirAck`, `WriteNotice`, `RecordRetention`, `JournalInput`, `JournalReplay`, `DiskSession`, `DiskStartup`, `disk_session`, `start_dir_journal`, `enqueue_pending`, `handoff`, `poll_acks`, `durable_wait`, `rotate_segment`, `disk_cut_with_limit`, `journal_scan`, `journal_apply`, `journal_apply_commit`, `journal_apply_fork`, `journal_following`, `read_recorded_dir`, `close_dir_journal` |
| 9 | `indexes` | 4 | `store-index-registry.hirsel`, `store-aggregate.hirsel` | `types`, `format`, `pages`, `catalog` | `IndexState` *(opaque)*, `IndexRegistry`, `IndexRegistration`, `IndexDrift`, `IndexReport`, `IndexValue`, `IndexStop`, `Aggregate[G, M]`, `Totals`, `empty_index_state`, `register_declared`, `apply_put_plan`, `apply_delete_plan`, `record_drift`, `demote_drifted`, `aggregate`, `totals` |
| 10 | `capture` | 5 | `store-capture.hirsel`, `store-capture-check.hirsel`, `store-capture-check-journal.hirsel`, `store-capture-journal.hirsel`, `store-capture-pages.hirsel`, `store-capture-catalog.hirsel`, `store-capture-describe.hirsel`, `store-inspect.hirsel` | `types`, `format`, `pages`, `catalog`, `manifest`, `pager`, `journal` | `VerifiedCapture` *(opaque, `Arc`-backed for 3062 #2)*, `CheckTotals`, `DescribeEntry`, `DescribeLine`, `EntryForm`, `CatalogEntry`, `VerifyOutcome`, `capture_directory`, `check_capture`, `describe_capture`, `capture_need`, `planned_capture`, `catalog_entries`, `verify_release` |
| 11 | `history` | 5 | `store-history.hirsel`, `store-history-read.hirsel`, `store-history-witness.hirsel`, `store-history-packs.hirsel`, `store-register.hirsel`, `store-object-authority.hirsel`, `store-object-cut.hirsel` | `status`, `types`, `format`, `pages`, `manifest`, `journal` | `HistoryHead`, `HistoryPair`, `HistoryRef`, `HistoryCapture`, `HistoryConfig`, `HistoryLocator`, `HistoryRegister`, `HistoryBuffers`, `HistoryOpenBase`, `HistoryWrite`, `HistoryClaimResult`, `HistoryActivation`, `ObjectPackRead`, `EndpointRefusal`, `HeadView` *(3048)*, `history_fetch`, `history_write_head`, `history_check_open`, `history_check_endpoint`, `history_check_conditions`, `history_register_publish`, `history_replace_claim`, `history_publish_genesis`, `history_publish_checkpoint`, `history_load_admitted`, `history_write_page_cut`, `history_validate_manifest` |
| 12 | `host_stream` | 6 | `store-stream-claim.hirsel`, `store-stream-service.hirsel`, `store-stream-slot.hirsel`, `store-stream-task.hirsel`, `store-stream-poll.hirsel`, `store-stream-settle.hirsel`, `store-stream-gc.hirsel`, `store-stream-janitor.hirsel`, `store-stream-follow.hirsel`, `store-stream-archive.hirsel` | `status`, `types`, `format`, `pages`, `manifest`, `journal`, `history` | **Claim-Machine API (§5):** `ClaimMachine` *(opaque)*, `ClaimPhase`, `ClaimConfig`, `ClaimStep`, `ClaimedPlace[T]`, `claim_new`, `claim_step`, `claim_run`, `claim_abort`; `RegisterLane` *(opaque)*; `HoldTracker` *(opaque)*; `JournalService` *(opaque)*, `JournalSlot` *(opaque)*, `SlotLink`, `SlotAck`, `SlotReport`, `StreamPlan`, `StreamLimits`, `Batching`, `StreamRoot`, `SettlePort`, `SettleOutcome`, `StreamFollowResult`, `stream_service_start`, `stream_slot_open`, `stream_settle`, `stream_settle_sealed`, `stream_poll_head`, `stream_follow_round`, `stream_janitor_step` |
| 13 | `object_writer` | 7 | `store-object-open.hirsel`, `store-object-journal.hirsel`, `store-object-writer.hirsel`, `store-reclaim.hirsel`, `store-sim-levels.hirsel`, `store-levels.hirsel`, `store-health.hirsel`, `store-followers.hirsel`, `store-follower-service.hirsel`, `store-object-follower.hirsel` | `status`, `types`, `format`, `pages`, `manifest`, `pager`, `journal`, `history`, `host_stream` | `ObjectWriterState` *(opaque)*, `ObjectJournalMsg`, `ObjectJournalSession`, `Authority`, `Refusal`, `Claim`, `Level`, `Position`, `SimLevels`, `LevelWorld`, `LevelSync`, `Follower`, `Followers`, `FollowerService`, `FollowerOwner`, `FollowerSlot`, `FollowerPlan`, `open_object_writer`, `object_cut_state`, `object_stream_close`, `object_cut_close`, `object_fresh_probe`, `durable_at`, `position`, `levels`, `health`, `on_unhealthy`, `claim_of`, `reclaim_preview`, `reclaim_purge`, `streamed`, `stream_reader`, `follower`, `fresh`, `at_least`, `follower_service` |
| 14 | `read` | 8 | `store-read.hirsel`, `store-read-pages.hirsel`, `store-changes.hirsel` | `types`, `format`, `pages`, `catalog`, `manifest`, `pager`, `capture`, `object_writer` | `Snapshot` *(opaque)*, `Reader` *(opaque)*, `ReadOnly` *(opaque)*, `AnonymousCursor`, `Changes`, `ChangePage`, `SpanRows`, `reader`, `follower_reader`, `read_only_reader`, `staged_reader`, `get`, `fetch_many`, `scan`, `scan_reverse`, `scan_page`, `scan_next`, `find`, `find_page`, `find_next`, `count`, `count_at`, `changes`, `changes_page`, `open_span_changes`, `next_span_change`, `visit_resident_leaves` *(3062 #3)* |
| 15 | `txn` | 9 | `store-txn-core.hirsel` *(split)*, `store-tx.hirsel`, `store-commit.hirsel`, `store-attempts.hirsel`, `store-mux.hirsel`, `store-fork.hirsel`, `store-cell.hirsel`, `store-log.hirsel`, `store-edge.hirsel`, `store-streams.hirsel`, `store-keyed-claims.hirsel`, `store-keyed-lifecycle.hirsel`, `store-reexec.hirsel` | `status`, `types`, `format`, `pages`, `catalog`, `manifest`, `pager`, `journal`, `indexes`, `object_writer`, `read` | `Tx` *(scoped opaque)*, `AccountingState` *(opaque)*, `CallStreamState` *(opaque)*, `Parked` *(opaque)*, `Pending` *(opaque)*, `Served`, `Completion`, `ClaimKey`, `ClaimSweep`, `ClaimRecovery`, `KeyedPolicy`, `Verdict`, `Difference`, `UnknownReason`, `ReplaySource`, `run_tx`, `serve_call`, `conclude_call`, `put`, `delete`, `fetch`, `fetch_bytes`, `pop_first`, `pop_last`, `rollback_writes_on_*`, `cell_*`, `log_*`, `edge_*`, `admit`, `admit_claim`, `conclude_claim`, `sweep_streams`, `sweep_claims`, `fork_branch`, `reexecute`, `reexecute_with` |
| 16 | `transfer` | 10 | `store-transfer-core.hirsel` *(split)*, `store-export.hirsel`, `store-object-export.hirsel`, `store-object-import.hirsel`, `store-prepared-check.hirsel`, `store-catchup.hirsel`, `store-rounds.hirsel`, `store-install.hirsel`, `store-activation.hirsel` | `status`, `types`, `format`, `pages`, `catalog`, `manifest`, `pager`, `journal`, `indexes`, `history`, `object_writer`, `read`, `txn` | `TransferState` *(opaque)*, `DeployError`, `Preparation`, `PreparedTarget`, `ValidatedImport`, `ImportWork`, `Imported`, `ExportCopy`, `ExportStamp`, `Exported`, `Cutover`, `begin_backup`, `begin_export`, `begin_export_copy`, `export_copy`, `release_export`, `open_export`, `write_prepared`, `validate_prepared`, `reserve_import`, `import_trees`, `keep_import`, `catch_up_export`, `stage_activation`, `install_activation`, `perform_cutover`, `restore` |
| 17 | `maintenance` | 10 | `store-index-build.hirsel`, `store-verify.hirsel`, `store-expiry.hirsel`, `store-projector.hirsel` | `status`, `types`, `format`, `pages`, `catalog`, `manifest`, `pager`, `journal`, `indexes`, `object_writer`, `read`, `txn` | `ProjectorState` *(opaque)*, `DeclaredProjector`, `Binding`, `Generation`, `Image`, `Pass`, `Merge`, `Projection`, `ProjectionDecl`, `IndexWalk`, `WalkFault`, `WalkFinding`, `WalkReport`, `WalkMode`, `Swept`, `index_build_step`, `rebuild_index`, `rebuild_aggregate`, `begin_verify`, `verify_step`, `repair_indexes`, `expire_rows`, `sweep_expired`, `projector`, `project_step`, `project_recorded`, `projection_row` |
| 18 | `store` | 11 | `store.hirsel` *(~550 L composition root)*, `store-attention.hirsel`, `sizes.hirsel` | all Layers 0–10 | Thin `Db` & `DiskState` composition roots, `open`, `open_authority`, `open_read_only`, `memory`, `close`, `checkpoint`, `cut_install`, `adopt`, `delete_branch`, `pin`, `unpin`, `pinned`, `snapshot`, `snapshot_of`, `follow`, `attention`, `attention_mark`, `accounts`, sizing APIs (`sized`, `planned*`, `writer_plan`, `service_bytes`), and thin `pub` re-exports/forwarders for every external `store.*` symbol |
| 19 | `main` | 12 | `main.hirsel` | `store` | `pub fn main` (benchmark/workload entry) |

*Note on `store-keyed-journal.hirsel` (182 L):* Lines 1–112 (wire codec) move to `format`; lines 114–166 (`materialize_keyed_edits`, `keyed_lookup`, `keyed_draft_live`) move to `pages` as `store-keyed-pages.hirsel`; lines 168–182 (`resident_with_credit`, `available_resident`, `available_journal`, `draft_credit`) move into `store-keyed-claims.hirsel` in `txn`.

*Note on `[tests.sources]` (92 law files):* Black-box law files that test the public `store.*` API stay assigned to `"store"` in `[tests.sources]`. White-box law files that test internal invariants of a specific module (e.g., `store_locations_disk_laws.hirsel` → `manifest`, `store_format10_laws.hirsel` → `format`, `bucket_stream_claim_laws.hirsel` → `host_stream`, `tiered_compact_laws.hirsel` → `pager`) are assigned directly to their owning module in `[tests.sources]` under `PROG-4` ("Test sources join the package's modules for `hirsel check` and `hirsel test`... so it may call that module's private functions"), keeping production surfaces narrow.

---

## 3. Owned Subsystem State (`Db` and `DiskState` Decomposition)

### 3.1 Resulting Composition Roots

`Db` drops from **39 raw fields** to **13 fields** (6 root/shared fields + 7 opaque subsystem owners):

```hirsel
pub opaque type Db {
    // Immutable identity, capabilities & cross-cutting root
    store: types.StoreId,
    clock: Clock,
    limits: types.StoreLimits,
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

`DiskState` drops from **34 raw fields** to **8 fields**, replacing overlapping `d: Option[Dir]` + `lock: Option[HostFile]` + `object: Option[ObjectWriterState]` + shared `journal` channel with a type-safe `Backend` sum:

```hirsel
type Backend = DirBackend(journal.DirJournalState) | ObjectBackend(object_writer.ObjectWriterState)

type DiskState {
    // Immutable open context
    open: u64,
    options: types.Options,
    tasks: Tasks,
    // Cross-backend CallRecord floor
    retention: journal.RecordRetention,
    // Owned subsystem states
    packs: manifest.PackDiskState,
    tiered: pager.DiskPagerState,
    transfer: transfer.TransferState,
    // Exclusive persistence backend (Directory log@1 vs Bucket objects@1/sim@1)
    backend: Backend
}
```

### 3.2 Complete Field-by-Field Assignment: `Db` (All 39 Fields)

| # | Current `Db` Field | Type | New Owner Module & Struct | Invariant Maintained by Owning Module |
|---|---|---|---|---|
| 1 | `store` | `StoreId` | **`store` (stays on `Db`)** | Immutable 128-bit store identity fixed at genesis/open. *(Cannot move: §3.4.)* |
| 2 | `clock` | `Clock` | **`store` (stays on `Db`)** | Injected host clock capability. *(Cannot move: §3.4.)* |
| 3 | `limits` | `StoreLimits` | **`store` (stays on `Db`)** | Immutable per-store `(frame, value, meta)` ceilings matching MANIFEST. *(Cannot move: §3.4.)* |
| 4 | `disk` | `Option[DiskState]` | **`store` (stays on `Db`)** | `None` iff `memory@1`; `Some` iff `log@1`/`objects@1`/`sim@1`. *(Cannot move: §3.4.)* |
| 5 | `attention_seq` | `u64` | **`store` (stays on `Db`)** | Monotonic sequence for `attention_mark`, bumped on any operator-visible signal change across subsystems. *(Cannot move: §3.4.)* |
| 6 | `frame_seq` | `u64` | **`store` (stays on `Db`)** | Strictly monotonic sequence of non-skipped durable frames minted by this open (`0` on `memory@1`); `backend.journaled <= frame_seq`. *(Cannot move cleanly: §3.4.)* |
| 7 | `pager` | `Option[Sender[TieredPagerMessage]]` | `pager.PagerState` | `Some` iff store has started a Tiered pager; paired with `pager_writer` and `park_notices`. |
| 8 | `pager_writer` | `Option[Sender[TieredPagerMessage]]` | `pager.PagerState` | One-slot writer demand channel into the same pager worker; `Some` iff `pager` is `Some`. |
| 9 | `park_notices` | `Option[Receiver[()]]` | `pager.PagerState` | Coalescing wake receiver handed out at most once via `park_notices(mut db)`. |
| 10 | `parked` | `ParkQueue` | `pager.PagerState` | Entries sorted strictly ascending by `id < next_id`; at most one in-flight writer demand across all entries; cleared atomically on close/poison. |
| 11 | `writer_pin_bytes` | `u64` | `pager.PagerState` | Fixed writer partition pin ceiling (§13.2) derived from `options.writer_pin_bytes`; bounds every attempt's pinned cold frames. |
| 12 | `indexes` | `IndexRegistry` | `indexes.IndexState` | Maps `(table, index, schema)` to active extractors and `LogRetention`; `next_ordinal` strictly increases; matches `Public`/`Backfilling` entries in `heads[0].roots.catalog`. |
| 13 | `drift` | `List[IndexDrift]` | `indexes.IndexState` | At most one entry per `(table, index)`; demotes drifted index before any subsequent write uses it (§I.3). |
| 14 | `heads` | `List[Head]` | `pages.HolderTable` | Non-empty; slot `0` is always `main` (`id == 0`), slots `1..` strictly ascending by `id`; `at` monotonic per branch. |
| 15 | `branch_names` | `Map[String, u32]` | `pages.HolderTable` | Exact bijection between non-main `heads[1..]` names and their branch `id`s. |
| 16 | `pins` | `List[Pin]` | `pages.HolderTable` | All active named, projector, and import pins; every pin name unique within its owner class. |
| 17 | `record_bytes` | `u64` | `pages.HolderTable` | Exact sum of `branch_root_bytes(h)` + `pin_root_bytes(p)` over `heads` and `pins`; always `<= limits.meta`. |
| 18 | `next_pin` | `u64` | `pages.HolderTable` | Strictly monotonic pin ordinal (`>= len(pins)`). |
| 19 | `next_branch` | `u32` | `pages.HolderTable` | Strictly greater than `h.id` for every `h` in `heads` and every past branch in history. |
| 20 | `next_node` | `u64` | `pages.HolderTable` | Strictly `>=` every page-node stamp reachable from any root in `heads` or `pins`. |
| 21 | `bases` | `TailBases` | `pages.HolderTable` | Cached map of projector-pin roots; rebuilt atomically inside `HolderTable` on every `pins` add/remove/replace. |
| 22 | `fence` | `u32` | `catalog.SchemaState` | Catalog/activation fence level persisted in MANIFEST frame `0x0070` (`0` or `1`, monotonic). |
| 23 | `next_table` | `u32` | `catalog.SchemaState` | Strictly greater than every allocated collection/index id in `heads[0].roots.catalog`; advanced atomically with `ReplaceMain`. |
| 24 | `file_seq` | `u64` | `journal.DirJournalState` *(and `object_writer` reads `head.entry.seq`)* | Active segment sequence on `log@1` (strictly monotonic across rotations; eliminated as a duplicated mirror on `objects@1` and `memory@1`). |
| 25 | `file_end` | `u64` | `journal.DirJournalState` | Exact byte offset of active `log@1` segment tail (`>= 72`, 4 KiB-aligned on `O_DIRECT` flush; eliminated as dummy `72` on `objects@1` and `0` on `memory@1`). |
| 26 | `resident` | `u64` | `txn.AccountingState` | Conservative logical Memory byte count across all heads/pins (exact whenever `settled == changes`, I4); never exceeds available ceiling on admitting writes. |
| 27 | `tiered` | `u64` | `txn.AccountingState` | Logical Tiered byte count updated by format-5+ prior deltas and settled on cuts. |
| 28 | `page_resident` | `u64` | `txn.AccountingState` | Resident page-location charge (`resident_location_charge(pages.locations)`) reported beside `resident`, never admitted against `ceiling`. |
| 29 | `changes` | `u64` | `txn.AccountingState` | Monotonic count of non-skipped commits and cut-made mutations across all branches. |
| 30 | `settled` | `u64` | `txn.AccountingState` | `changes` value at which `resident` was last computed exactly (`settled <= changes`). |
| 31 | `ceiling` | `u64` | `txn.AccountingState` | Immutable resident admission ceiling (`options.resident_limit`). |
| 32 | `checkpointed` | `u64` | `txn.AccountingState` | Latest durable cut generation installed on this store (`0` on memory before first cut, `>= 1` on disk). |
| 33 | `keyed_credit` | `KeyedCredit` | `txn.CallStreamState` | Exact resident/padding/journal credit reserved for open keyed claims so claim completion cannot deadlock on `journal_limit` or `ceiling`. |
| 34 | `admits` | `Map[Bytes, OpenAdmit]` | `txn.CallStreamState` | Active unreplied `Admitted(p)` call-stream entries committed during this open (§1.8); cleared on reply/release. |
| 35 | `stream_retention_ms` | `u64` | `txn.CallStreamState` | Call-stream reply retention window fixed from `options.stream_retention_ms`. |
| 36 | `streams_per_caller_max` | `u64` | `txn.CallStreamState` | Per-caller active call-stream ceiling. |
| 37 | `streams_max` | `u64` | `txn.CallStreamState` | Global active call-stream ceiling. |
| 38 | `stream_sweep_after` | `Option[StreamSweepCursor]` | `txn.CallStreamState` | Incremental cursor for bounded call-stream expiry sweeps. |
| 39 | `projectors` | `List[DeclaredProjector]` | `maintenance.ProjectorState` | Registered local/follower projectors (§1.14); unique names matching declared projector pins. |

### 3.3 Complete Field-by-Field Assignment: `DiskState` (All 34 Fields)

| # | Current `DiskState` Field | Type | New Owner Module & Struct | Invariant Maintained by Owning Module |
|---|---|---|---|---|
| 1 | `open` | `u64` | **`store` (stays on `DiskState`)** | Open incarnation/epoch stamped into every `Ticket` and `Snapshot` (`this_open` check). *(Cannot move: §3.4.)* |
| 2 | `options` | `Options` | **`store` (stays on `DiskState`)** | Immutable validated `Options` snapshot fixed at open. *(Cannot move: §3.4.)* |
| 3 | `tasks` | `Tasks` | **`store` (stays on `DiskState`)** | Task capability used to spawn/supervise background workers across `journal`, `pager`, `object_writer`. *(Cannot move: §3.4.)* |
| 4 | `record_cut` | `u64` | `journal.RecordRetention` *(on `DiskState`)* | First live journal segment (`log@1`) or checkpoint entry seq (`objects@1`) holding retained `CallRecord`s; updated atomically on every cut. *(Shared across backends: §3.4.)* |
| 5 | `recorded_after` | `u64` | `journal.RecordRetention` *(on `DiskState`)* | Main-branch version at `record_cut`; versions `<= recorded_after` are below the live journal. *(Shared across backends: §3.4.)* |
| 6 | `pages` | `PageDisk` | `manifest.PackDiskState` | Consistent pack allocator (`next_pack`), `locations`, `lists`, `owners`, `carried`, `deferred`, `forked`, and `compaction`; updated atomically only after MANIFEST/checkpoint publish succeeds. |
| 7 | `scrubs` | `List[Arc[ScrubLease]]` | `manifest.PackDiskState` | Active scrub leases pinning referenced pack sequences against AP21 reclamation. |
| 8 | `pager_task` | `Option[Task[()]]` | `pager.DiskPagerState` | Owned Tiered pager worker handle; `Some` iff `Db.pager` is active; cancelled and joined on `close`. |
| 9 | `tiered_dirty` | `TieredDirty` | `pager.DiskPagerState` | Exact set of dirty Tiered collections and overlays awaiting emission/fold on the next cut; reset atomically on cut publish. |
| 10 | `restamp` | `Option[u32]` | `pager.DiskPagerState` | Collection id currently queued for `Memory -> Tiered` re-stamping on its schema cut (`None` outside `set_residency`). |
| 11 | `unload` | `Option[u32]` | `pager.DiskPagerState` | Collection id currently queued for `Tiered -> Memory` unload on its schema cut (mutually exclusive with `restamp`). |
| 12 | `lease` | `Option[Arc[ExportLease]]` | `transfer.TransferState` | At most one active online-export lease pinning segments and packs against removal. |
| 13 | `steps` | `List[LeaseStep]` | `transfer.TransferState` | Ordered schema commits on `main` since `lease` last advanced (§3.5a Catch-up `STEPS`); empty when `lease` is `None`. |
| 14 | `imported` | `Option[KeptImport]` | `transfer.TransferState` | Active kept-import pin and delta-round cursor (§3.5a); cleared on cutover or abort. |
| 15 | `reserving` | `Option[Inline[u8, 32]]` | `transfer.TransferState` | Digest of the one currently reserved `ValidatedImport`; prevents concurrent conflicting reservations. |
| 16 | `target` | `Option[TargetLimits]` | `transfer.TransferState` | Target limits enforced on cuts while a prepared cutover is staged; cleared after cutover. |
| 17 | `lock` | `Option[HostFile]` | `journal.DirJournalState` *(non-optional `HostFile` inside `DirBackend`!)* | Exclusive `LOCK` file handle held for the entire `log@1` open; dropped only on `close`. |
| 18 | `d` | `Option[Dir]` | `journal.DirJournalState` *(non-optional `Dir` inside `DirBackend`!)* | Directory capability for `log@1` segments, packs, and `MANIFEST`. |
| 19 | `creation` | `Bytes` | `journal.DirJournalState` | Pre-allocated 4 KiB-aligned scratch block for `O_DIRECT` segment header creation (`disk_new_segment`); never allocated on `ObjectBackend`. |
| 20 | `acks` | `Receiver[Ack]` | Split: `journal.DirJournalState` (`Receiver[DirAck]`) & `object_writer.ObjectWriterState` (`Receiver[ObjectAck]`) | Strictly ordered durability/error acknowledgements from the owning backend's worker task. |
| 21 | `journaled` | `u64` | Owned per backend (`DirJournalState` & `ObjectWriterState`, read via narrow `backend_journaled()`) | Highest `frame_seq` durably persisted (`journaled <= Db.frame_seq`); monotonic. |
| 22 | `inflight` | `u64` | Owned per backend (`DirJournalState` & `ObjectWriterState`) | Unacknowledged bytes in flight to the worker (`<= FRAME_LIMIT` backpressure bound). |
| 23 | `uncovered` | `u64` | Owned per backend (`DirJournalState` & `ObjectWriterState`) | Journal bytes written since the last durable cut (`<= options.journal_limit`, AP15); reset to `0` on cut. |
| 24 | `since_cut` | `u64` | Owned per backend (`DirJournalState` & `ObjectWriterState`) | Bytes written since last cut; triggers `maybe_checkpoint` once `> options.checkpoint_bytes`. |
| 25 | `acked_bytes` | `u64` | Owned per backend (`DirJournalState` & `ObjectWriterState`) | Cumulative acknowledged byte watermark used to compute `inflight` deltas without drift. |
| 26 | `written` | `Receiver[WriteNotice]` | `journal.DirJournalState` *(and `ObjectWriterState`)* | Receives `Written` barrier confirmations before `recorded`/`reexec` reads the active journal segment. |
| 27 | `written_send` | `Sender[WriteNotice]` | `journal.DirJournalState` *(and `ObjectWriterState`)* | Cloned sender passed into worker for `Barrier` replies. |
| 28 | `synced` | `Option[Receiver[()]]` | Owned per backend (`DirJournalState` & `ObjectWriterState`) | Coalesced group-sync notification handle handed out once by `sync_notices(mut db)`. |
| 29 | `poison` | `Option[IoError]` | `journal.DirJournalState` | Sticky I/O failure on `log@1`; once `Some`, every mutating call returns `Poisoned(e)` and no further writes issue. |
| 30 | `journal` | `Sender[JournalMsg]` | Split: `DirJournalState` (`Sender[DirJournalMsg]`) & `ObjectWriterState` (`Sender[ObjectJournalMsg]`) | Outbound channel to the owning backend's worker; bounded by `group_commits * 2`. |
| 31 | `pending` | `Option[Bytes]` | Owned per backend (`DirJournalState` & `ObjectWriterState`) | At most one staged commit/fork frame awaiting immediate `handoff` after in-memory commit install. |
| 32 | `journal_task` | `Task[Result[(), Error]]` | Split: `DirJournalState` & `ObjectWriterState` | Owned background journal worker task; joined on `close`. |
| 33 | `journal_idle_task` | `Option[Task[()]]` | `object_writer.ObjectWriterState` *(never used on `DirBackend`!)* | Idle head-poll timer (standalone `objects@1`) or `slot_relay` task (attached host-stream Place); cancelled and joined on `close`. |
| 34 | `object` | `Option[ObjectWriterState]` | `object_writer.ObjectWriterState` *(the `ObjectBackend` arm of `Backend`!)* | Owns `workers`, `health`, `reclaims`, `unhealthy`, `watch`, `terminal`, `locator`, `levels`, `weak`, `pair`, `recovered`, `entries`, `failed`, `stream`, `attached`, `has_sealed`, `drop_attach_on_cut`. |

### 3.4 Fields That Cannot Move Cleanly to a Single Subsystem (and Why)

Eleven fields intentionally stay on `Db` or `DiskState` (never hidden by artificial indirection):

1. **`Db.store: StoreId`**, **`Db.clock: Clock`**, **`Db.limits: StoreLimits`**: Immutable identity, capability, and format-ceiling roots read by 6–25 files across `pages`, `catalog`, `manifest`, `journal`, `object_writer`, `read`, `txn`, `transfer`, and `maintenance`. Assigning them to any single leaf subsystem would force unrelated subsystems to depend on that leaf.
2. **`Db.disk: Option[DiskState]`**: The structural seam separating `memory@1` (`None`) from durable backends (`Some`). Must live on `Db`.
3. **`Db.attention_seq: u64`**: Aggregates operator-attention changes across three independent subsystems (`indexes.drift`, `object_writer.reclaims`, `transfer` activation). Only the composition root `store` sees all three.
4. **`Db.frame_seq: u64`**: Minted by `txn` on every non-skipped commit/fork (and read as `0` on `memory@1` and on v2.147 zero-mutation commits to form `Ticket(Journal(db.frame_seq, branch, version))`), while correlated against `backend.journaled` in `durable`/`durable_now`/`durable_at`. Moving `frame_seq` inside `DiskState` would require branching on `db.disk` at every ticket creation and would scatter the `journaled <= frame_seq` invariant; keeping it on `Db` as the single monotonic handoff counter with explicit `next_frame_seq` mutation on handoff is minimal and clear.
5. **`DiskState.open: u64`**, **`DiskState.options: Options`**, **`DiskState.tasks: Tasks`**: Shared open-level incarnation, configuration, and task-spawning capability used by `journal`, `manifest`, `pager`, `object_writer`, and `transfer`.
6. **`DiskState.retention: journal.RecordRetention` (`record_cut`, `recorded_after`)**: Updated atomically on every checkpoint cut by *both* `DirBackend` (`store.hirsel:4106`) and `ObjectBackend` (`store-object-writer.hirsel:557`), and read by `store.retained_main_after`, `store-reexec.hirsel`, and `store-export.hirsel`. Grouping the pair into a 2-field `RecordRetention` value on `DiskState` keeps one owner for the invariant (`record_cut` and `recorded_after` always advance together) without duplicating it across `DirBackend` and `ObjectBackend`.

---

## 4. Split of `store.hirsel` (4,846 Lines → 8 Focused Files)

`store.hirsel` currently mixes seven distinct responsibilities across 4,846 lines. The split separates **transaction execution**, **persisted-format codec**, and **schema catalog** (Finding #3) plus the remaining leaf/journal/transfer chunks so that **a persisted-format change edits `format` (and `manifest` for manifest layout) and never touches `txn` or `catalog`**:

| Current `store.hirsel` Lines | Lines | Concern | Target File & Module |
|---|---|---|---|
| `L1–L39`, `L348–L410`, `L573–L596`, `L1494–L1504`, `L1567–L1597`, `L2464–L2476`, `L4146–L4186` | ~290 | Public value/error/ticket/outcome/record leaf types, `splitmix64`, `call_info`, `same`, `make_id`, `RequestDigest`, `capacity`, `copied` | `store-types.hirsel` → **`types`** |
| `L816–L822`, `L1683–L1763`, `L1828–L1843`, `L2499–L2798`, `L4216–L4442` | ~700 | Persisted binary primitives (`le`, `read_le`, `blob`, `append_be`), `DISK_FORMAT`, `DiskContext`, `DiskFrame`, `DiskHeader`, xxh3 `seal_frame`/`disk_read_frame*`, `encode_data_op`, `encode_log_trim`, `encode_stream_op*`, `encode_fork`, `JournalCommit`/`JournalFork`/`JournalEvent`, `journal_ops`, `decode_settlement`, `journal_decode*`, `journal_torn` | `store-format.hirsel` → **`format`** |
| `L99–L323`, `L411–L561` | ~375 | `Head`, `Tip`, `State`, `ProposedMain`, `HolderDelta`, `HolderTable`, `state_with_delta`, `install_holder_delta`, `DataOp`, `StagedValue`, `DataRows`, `DataOps`, `Staged`, `op_slot` | `store-roots.hirsel` → **`pages`** |
| `L1764–L1827`, `L1844–L1875`, `L2000–L2412`, `L2486–L2498` | ~520 | `RowSchemas`, `Readers[V]`, `Table[K, V]`, `Index[J, K, V, P]`, `RowDecoder`, `row_schemas`, `row_value`, `encode_key`, `typed_bound`, `table_key`, `catalog_index_key`, `catalog_entry_in`, `define/bulk/remove_store_collection`, `system_table`, `stage_create_table`, `readers`, `table`, `stage_table`, `catalog_handle`, `catalog_index_handle`, `stage_drop_table_key`, `migrate_build`, `stage_migrate`, `catalog_insert`, `catalog_recharge` | `store-catalog.hirsel` → **`catalog`** |
| `L2799–L2854`, `L3386–L3694`, `L3793–L3816`, `L3940–L4139`, `L4443–L4660` | ~780 | Directory I/O helpers (`disk_io`, `disk_path`, `disk_write_synced`, `disk_remove_optional`, `disk_publish_manifest`), `SegmentSpan`, `DiskRemovals`, `DiskStartup`, `DiskSession`, `disk_lock`, `disk_start`, `disk_session`, `JournalChannels`, `disk_rotate`, `disk_cut_timed`, `disk_cut_with_limit`, `JournalInput`, `journal_scan`, `JournalReplay`, `journal_following` | `store-journal-io.hirsel` → **`journal`** |
| `L772–L815`, `L823–L1493`, `L1505–L1566`, `L1612–L1682`, `L1876–L1999`, `L2429–L2450`, `L4187–L4215`, `L4661–L4667` | ~960 | `Publication`, `Receipt`, `Stop`, `Plan`, `CallReserve`, `ServeError`, `Reserve`, `Pending`, `Served`, `serve`, `conclude`, `info`, `run`, `run_on`, `run_with_reply`, `store_body_*`, `append_transaction_at`, `handoff`, `maybe_checkpoint`, `Ran`/`Stage`/`Finished`/`Prepared`, `stage_call`, `draft_body`, `finish_stage`, `wait_for_frame_room`, `durable`, `sync_notices`, `durable_now`, `durable_groups`, `covered`, `fetch_logical`, `fetch_bytes`, `fetch`, `put`, `delete`, `key_span`, `published_value`, `reads_key`, `recorded` | `store-txn-core.hirsel` → **`txn`** |
| `L2855–L3385` | ~531 | `PreparedTarget`, `PreparedMeta`, `CatalogChange`, `Inventory`, `PreparedRow`, `catalog_changes`, `changed_ranges`, `range_inventory`, `write_prepared`, `ValidatedImport`, `validate_prepared`, `Imported`, `ImportWork`, `reserve_import`, `import_trees`, `Cutover`, `TargetLimits`, `planned_catalog`, `planned_ranges`, `pack_leased` | `store-transfer-core.hirsel` → **`transfer`** |
| `L40–L98`, `L324–L347`, `L562–L572`, `L597–L771`, `L3695–L3792`, `L3817–L3939`, `L4140–L4145` | ~550 | Thin `Db` and `DiskState` composition roots, `memory`, `open`, `open_authority`, `open_read_only`, `close`, `checkpoint`, `cut_install`, `adopt`, `delete_branch`, `snapshot`, `snapshot_of`, `follow`, `healthy`, `retained_main_after`, and thin `pub` API forwarders/type aliases | `store.hirsel` → **`store`** |
| `L1598–L1611`, `L2413–L2428`, `L2451–L2463`, `L4668–L4846` | ~220 | Inline test fixtures (`test_db`, `typed_put`, `bucket`, `OldAccount`/`NewAccount`/`upgrade`, `reserve_tx`, `law_call_reserve`, `reserve_settles_pin`) | Move test-only fixtures to `[tests.sources]` (`store_law_schema.hirsel`); keep only `law_call_reserve`/`test_db` allow-listed in `store.hirsel` until `runner` L33 migrates |

**Verification of line-length hygiene:** Every single file produced by the split is under 1,000 lines, allowing `store.hirsel` to be removed from `tools/file_length_ratchet.json` upon Lane 14 completion.

**Why format changes stop editing `txn`:**
- `txn` builds typed in-memory `Staged` / `DataRows` / `KeyedDraft` / `CallRecord` values and calls `format.encode_staged(...)` to obtain `Bytes`.
- `txn` never reads or writes `DISK_FORMAT`, frame tags (`0x0011`, `0x0013`, `0x0031`, `0x0040`, `0x00D0`, `0x00E0`), little-endian widths, or xxh3 bindings.
- `catalog` builds typed `Catalog` records and calls `format.encode_catalog` / `format.decode_catalog`.
- Therefore a Format 10 → Format 11 bump (such as lane 3048's `ORIGIN` frame + `retirement_root` removal) edits `format` and `manifest`, with **zero edits to `txn`**.

---

## 5. Claim-Machine API (`host_stream`, Finding #4)

### 5.1 What Changes

Today, claiming an attached Place (`§12.11.5`), electing a stream (`§12.11.7`), and servicing holds/stalls/unholds (`§12.11.4`, `§12.11.6`) pass overlapping mutable context (`ObjectPackRead`, `SlotLink`, `ClaimStream`, `StreamLane`, `HistoryBuffers`, `Options`, `Random`, `Clock`, `ObjectJournalSession`) across 6 files, with `stream_tail_batch` taking 17 parameters and `stream_claim_attempt` taking 11.

The end state puts three narrow opaque types inside `host_stream`:

1. **`RegisterLane`** (`pub opaque type`): owns the `S/reg` cached register body, `ETag`, incarnation/holder identity, batch sequence, pending slices, and terminal latch. No code outside `host_stream` (and no task/poll/claim code inside it) mutates `reg`, `tag`, `pending`, or `latched` directly.
2. **`ClaimMachine`** (`pub opaque type`): owns the §12.11.5 (and lane 3048 `ClaimAuth` + `ORIGIN`) Place claim state machine. `object_writer`, `store-stream-slot`, and `history` supply narrow scoped capabilities (`load_settled`, `grant_slot`, `prepare_writer`, `publish_claim`) rather than threading `ObjectPackRead`, `SlotLink`, and `HistoryBuffers` through each phase.
3. **`HoldTracker`** (`pub opaque type`): owns the service task's per-Place stall tickets, first-seen hold timestamps, active settles, and stall-poll schedules, returning pure `List[ServiceAction]` directives executed by `store-stream-task` and `store-stream-poll`.

### 5.2 `ClaimMachine` States and Transitions

```text
                       ┌──────────────────────────┐
                       │          Init            │◄───────────────────────────┐
                       │ (head witness decoded;   │                            │
                       │  ClaimAuth verified)     │                            │
                       └────┬───────────────┬─────┘                            │
             live == None   │               │ live == Some(tail)               │
         (direct service)   │               ▼                                  │
                            │     ┌───────────────────┐                        │
                            │     │   SettledLoaded   │  (404 head moved)      │
                            │     │ (ckpt + sealed +  ├────────────────────────┤
                            │     │  entries replayed)│                        │
                            │     └─────────┬─────────┘                        │
                            │               │ reg read                         │
                            │      ┌────────┴────────┐                         │
                            │      │ reg.inc >       │ reg.inc == tail.inc     │
                            │      │ tail.inc        │ (XINDEX + tail prescan  │
                            │      ▼                 │  + grant_slot once)     │
                            │ ┌──────────────┐       ▼                         │
                            │ │ NoHoldFenced │  ┌──────────┐                   │
                            │ │ (TERM placed │  │PreScanned│                   │
                            │ │  no reg Hold)│  └────┬─────┘                   │
                            │ └──────┬───────┘       │ Hold(token) CAS         │
                            │        │     Moved     ├──► NoHoldFenced         │
                            │        │   Undecided   ├──► Abandon* -> Failed   │
                            │        │     Written   ▼                         │
                            │        │          ┌──────────┐                   │
                            │        │          │   Held   │ (token active!)   │
                            │        │          │ (delta   │                   │
                            │        │          │  scan)   │                   │
                            │        │          └────┬─────┘                   │
                            │        │               │ any abort/move ->       │
                            │        │               │ GUARANTEED Abandon(tok) │
                            ▼        ▼               ▼                         │
                       ┌──────────────────────────────────┐                    │
                       │          DeltaVerified           │                    │
                       │ (prepare_writer + final head GET │  same-pair moved   │
                       │  byte-equal to step-1 witness)   ├────────────────────┤
                       └────────────────┬─────────────────┘  (abandons token   │
                                        │ H CAS (Match)       if Held)         │
                                        │   Conflict, same pair                │
                                        │   (abandons token) ──────────────────┘
                                        │   Conflict, new pair -> Failed(Lost)
                                        │   Remote/Undecided   -> Failed (keeps Held: rule e)
                                        ▼
                       ┌──────────────────────────────────┐
                       │          HeadPublished           │
                       │ (H Applied; ATTACH + ORIGIN set) │
                       └────────────────┬─────────────────┘
                                        │ publish_claim (CLAIM via slot or entry)
                                        ▼
                       ┌──────────────────────────────────┐
                       │             Claimed              │  (terminal Ok)
                       └──────────────────────────────────┘
```

**Structural invariants enforced by `ClaimMachine`:**

1. **Rule (e) leak freedom:** `token: Option[StoreId]` exists *only* inside `ClaimMachine`. Every transition leaving `Held` or `DeltaVerified` on `Restarting`, `Lost`, `Corrupt`, `Full`, or `Remote` before `H` is sent calls `abandon_active_hold` inside the state machine itself. Only an ambiguous `H` outcome (`HistoryRefused` / `HistoryUndecided`) leaves the row `Held` for service rule (a), matching §12.11.6 rule (e).
2. **`ClaimAuth` gate before any write (lane 3048):** In `Init`, before any `S/reg` `Hold` or `TERM` or `H` write executes, `ClaimMachine` verifies `head.pair == auth.expect` (returning `Failed(Superseded(actual))` on mismatch) and checks that `holder != auth.attempt` (preserving byte-inequality resolution in `history_write_head`).
3. **Single slot grant per open:** `granted: Option[(u64, u64)]` is acquired once across restarts so retries never leak slots or duplicate `SlotAttach` messages.
4. **Pure stepability:** `claim_step` advances exactly one phase per call, allowing deterministic simulation tests to pause, inject faults, or inspect phase invariants between every pair of phases without standing up a full `Db`.

### 5.3 Idiomatic Hirsel Signatures

All signatures below use valid Hirsel syntax (no traits; capabilities supplied via owned opaque handles and `scoped fn` callbacks):

```hirsel
pub type ClaimPhase: shareable + send =
    Init
    | SettledLoaded
    | PreScanned
    | NoHoldFenced
    | Held(token: types.StoreId, rev_hold: u64)
    | DeltaVerified
    | HeadPublished
    | Claimed
    | Failed

pub type ClaimConfig: send {
    holder: types.StoreId,
    authority_kind: u8,
    place_prefix: String,
    stream_root: String,
    resident_limit: u64,
    limits: Option[types.StoreLimits],
    requested_levels: List[String],
    deadline: u64,
    max_rows: u64
}

pub opaque type RegisterLane {
    dir: String,
    sid: status.HostStreamId,
    authority_kind: u8,
    max_rows: u64,
    inc: u64,
    holder: types.StoreId,
    seq: u64,
    reg: format.StreamRegister,
    tag: std.object.ETag,
    body: Bytes,
    compare: Bytes,
    latched: Option[status.StreamLatch]
}

pub opaque type ClaimMachine {
    phase: ClaimPhase,
    config: ClaimConfig,
    lane: RegisterLane,
    granted: Option[(u64, u64)],
    witness: Bytes,
    cursor_next: u64,
    cursor_last_x: Option[u64]
}

pub type ClaimedPlace[T] {
    register: history.HistoryRegister,
    replay: journal.JournalReplay,
    admitted: T,
    authority: Bytes
}

pub type ClaimStep[T] =
    Continue
    | Restart
    | Done(ClaimedPlace[T])
```

And the narrow operational entry points (each capability passed explicitly rather than bundling `Db`/`ObjectPackRead`/`SlotLink`/`ObjectJournalSession`):

```hirsel
pub fn claim_new(
    sid: status.HostStreamId,
    take config: ClaimConfig,
    take initial_head: Bytes
) -> Result[ClaimMachine, types.Error]

pub fn claim_step[T](
    mut machine: ClaimMachine,
    mut client: std.object.Client,
    r: Random,
    load_settled: scoped fn(history.HistoryHead, format.Attach, Bytes) -> Result[Option[journal.JournalReplay], types.Error] waits,
    grant_slot: scoped fn(types.StoreId, u64, String) -> Result[(u64, u64), types.Error] waits,
    prepare_writer: scoped fn(types.StoreId, journal.JournalReplay) -> Result[T, types.Error],
    publish_claim: scoped fn(history.HistoryRegister, Bytes, bool) -> Result[history.HistoryRegister, types.Error] waits
) -> Result[ClaimStep[T], types.Error] waits

pub fn claim_run[T](
    mut machine: ClaimMachine,
    mut client: std.object.Client,
    r: Random,
    load_settled: scoped fn(history.HistoryHead, format.Attach, Bytes) -> Result[Option[journal.JournalReplay], types.Error] waits,
    grant_slot: scoped fn(types.StoreId, u64, String) -> Result[(u64, u64), types.Error] waits,
    prepare_writer: scoped fn(types.StoreId, journal.JournalReplay) -> Result[T, types.Error],
    publish_claim: scoped fn(history.HistoryRegister, Bytes, bool) -> Result[history.HistoryRegister, types.Error] waits
) -> Result[ClaimedPlace[T], types.Error] waits

pub fn claim_abort(
    mut machine: ClaimMachine,
    mut client: std.object.Client
) waits
```

On the service side, `HoldTracker` replaces the 5 parallel vectors scattered across `StreamService` (`store-stream-task.hirsel:116-121`) and `store-stream-poll.hirsel`:

```hirsel
pub type ServiceAction: send =
    SendSlotAck(slot: u64, ack: SlotAck)
    | WriteRegister(intent: RegIntent)
    | PollOrphanHead(store: types.StoreId, epoch: u64, aged: bool)
    | EmitJanitorAsk(ask: JanitorAsk)
    | LatchService(cause: status.StreamLatch)

pub opaque type HoldTracker {
    hold_ns: u64,
    stall_poll_ns: u64,
    next_settle_id: u64
}

pub fn hold_on_reg(
    mut tracker: HoldTracker,
    reg: format.StreamRegister,
    now_ns: u64,
    serving_slot: scoped fn(types.StoreId, u64) -> Option[u64]
) -> Result[List[ServiceAction], types.Error]

pub fn hold_on_report(
    mut tracker: HoldTracker,
    report: SlotReport,
    now_ns: u64
) -> Result[List[ServiceAction], types.Error]

pub fn hold_next_deadline(tracker: HoldTracker, idle_target_ns: u64) -> u64
```

On the Place settle side (`store-stream-settle.hirsel`), `stream_settle` no longer accepts `ObjectJournalSession`; `object_writer` passes a narrow `SettleContext` (`client`, `register`, `authority`, `stream_root`, `last_acked`, `commit_batches`, `op_ms`, `group_bytes`) plus `append_entry` / `publish_head` callbacks, severing `host_stream -> object_writer`.

---

## 6. Coordination With Running and Queued Lanes

No cutover lane starts until **`store-bucket-stream-build3-3008`**, **`3048 store-claim-auth-origin`**, and the **`store-empty-commit`** build (v2.147) have landed on `origin/main`. Store performance implementation lanes rebase onto the first module split (Lane 1, or Lane 2/6/11 depending on target module).

| Active / Queued Lane | Scope | Exact Placement in This Module Map | Ordering Constraint |
|---|---|---|---|
| `store-bucket-stream-build3-3008` *(landed `f5eb79cb`)* | §12.11 stream service, claim, settle, GC, janitor, follower round, archival/exits | **`host_stream`** (`store-stream-*.hirsel`) + thin hooks in **`object_writer`** (`store-object-journal.hirsel`, `store-object-writer.hirsel`) | Landed at `f5eb79cb` (including `store-stream-archive.hirsel`); Lane 8 encapsulates its claim/poll/settle seams. |
| `perf-store-read-design-3062` *(running, design)* | Finding #2: multi-unit shared verified capture; Finding #3: scoped leaf visitor for resident scans | Finding #2 → **`capture`** (`VerifiedCapture` `Arc`-backed immutable backing) + **`read`** (per-unit `Reader` over shared capture). Finding #3 → **`read`** (`store-read.hirsel` scoped leaf visitor). | Implementation rebases onto **Lane 2** (for #2 in `capture`) and **Lane 11** (for #3 in `read`), or onto Lane 1 if started early. |
| `store-empty-commit-review-3059` + empty-commit build *(running/queued)* | STORE-SPEC v2.147 zero-mutation `run` skip (`observed_info`, `commit_clock_floor`, L91) | **`txn`** (`store-txn-core.hirsel`, `store-tx.hirsel`, `store-commit.hirsel`, `store-keyed-lifecycle.hirsel`) | Lands **before** Lane 1 starts; Lane 12 moves all 4 files cleanly into `txn`. |
| `perf-store-baseline-3061` *(running)* | Measurement only (S1–S3, S7–S8 baselines) | None (read-only measurement) | Independent. |
| `3048 store-claim-auth-origin` *(queued after 3008)* | Universal `ClaimAuth`, 49-byte `ORIGIN` head frame, Format 11 bump + 3023 `retirement_root`/`RetiredList` removal, `HeadView`, Runner `Deposed` | `ORIGIN` frame & Format 11 `0x0031` → **`format`**; `RetiredList` removal → **`manifest`** + **`capture`** (`store-capture-pages.hirsel`); `ClaimAuth` & `HeadView` → **`history`** + **`host_stream`** (`ClaimMachine.Init` gate); `Deposed` → `runner` | Lands **before** Lane 1 starts; proves why `format` + `manifest` + `host_stream` are the right boundaries (zero edits to `txn`). |
| Store write-path design *(queued)* | Perf #8: Directory journal coalescing + separate `END` cadence; Perf #10: checked ordered bulk ingest | Perf #8 → **`journal`** (`store-journal-worker.hirsel` only!). Perf #10 → **`format`** (`store-commit-wire.hirsel`) + **`pages`** (`store-pages.hirsel`) + **`txn`** (`store-commit.hirsel`). | Implementation rebases onto **Lane 1** (`format`), **Lane 2** (`pages`), and **Lane 6** (`journal`). |

---

## 7. Cutover Order (14 Sol-Sized Implementation Lanes)

Each lane is a mechanical module move plus its owned-state encapsulation (one clear spec, one landing commit, `hirsel laws write core/programs/store --libraries core`, `fmt.py --check`). **Lane 1 lands first because it unblocks the most downstream work**: extracting `types` + `format` breaks `store.hirsel`'s biggest inward dependency hub, unlocks Perf #10 (`format`), and allows Lanes 2–13 to peel off subsystems strictly bottom-up without ever creating a temporary cycle.

| Lane | Module(s) Landed | Files Touched & Owned-State Change | Proving Law Suites (`core/programs/store/`) | Unblocks |
|---|---|---|---|---|
| **L1** *(first)* | `status` (add `store-stream-types`), `types`, `format` | Extract `store-types.hirsel` and `store-format.hirsel` from `store.hirsel`; move `store-limits`, `store-tiered-refusals`, `store-commit-wire`, `store-stream-frames`, `store-tiered-frames`, `store-stream-key`, wire half of `store-keyed-journal`; update `hirsel.toml` + `hirsel.laws`. | `store_format10_laws`, `store_corpus_laws`, `store_corpus_format10`, `store_journal_laws`, `store_size_laws`, `bucket_stream_laws` | **Unblocks every other cutover lane (L2–L14)** and Perf #10 (`store-commit-wire`). |
| **L2** | `pages`, `capture` | Extract `store-roots.hirsel` from `store.hirsel` + `store-keyed-pages.hirsel`; introduce `pages.HolderTable` (owns `heads`, `branch_names`, `pins`, `record_bytes`, `next_pin`, `next_branch`, `next_node`, `bases` on `Db`); move `store-pages`, `store-layout`, `store-tail`, and all 8 `capture`/`inspect` files. | `store_pages_paged_laws`, `store_layout_laws`, `store_capture_laws`, `store_prior_laws`, `store_fork_laws` | **Unblocks 3062 #2** (multi-unit shared verified capture in `capture`), L3–L6, and Perf #10 (`pages`). |
| **L3** | `catalog` | Extract `store-catalog.hirsel` from `store.hirsel`; introduce `catalog.SchemaState` (owns `fence`, `next_table` on `Db`); move `store-handle-check`, `store-declare`, `store-migration`. | `store_schema_laws`, `store_declare_laws`, `store_lookup_laws`, `store_law_schema` | Unblocks L10 (`indexes`), L11 (`read`), L12 (`txn`), L13 (`transfer`). |
| **L4** | `manifest` | Move 13 files (`store-manifest*`, `store-list-read`, `store-page-recovery`, `store-locations`, `store-packs`, `store-pack-index`, `store-cut-*`, `store-tiered-frees`, `store-tiered-lineage`, `store-scrub*`); introduce `manifest.PackDiskState` (owns `pages`, `scrubs` on `DiskState`). | `store_manifest_page_laws`, `store_manifest_page_laws_2`, `store_locations_disk_laws`, `store_packs_disk_laws`, `store_scrub_laws`, `tiered_lineage_laws` | Unblocks L5 (`pager`), L6 (`journal`), L7 (`history`). |
| **L5** | `pager` | Move 26 `store-tiered*.hirsel` files (all except `frames`/`refusals`/`frees`/`lineage`); introduce `pager.PagerState` (owns `pager`, `pager_writer`, `park_notices`, `parked`, `writer_pin_bytes` on `Db`) and `pager.DiskPagerState` (owns `pager_task`, `tiered_dirty`, `restamp`, `unload` on `DiskState`). | `tiered_laws`, `tiered_read_laws`, `tiered_fold_laws`, `tiered_compact_laws`, `tiered_promote_laws`, `tiered_residency_laws`, `tiered_queue_laws`, `tiered_pager_batch_laws`, `tiered_attempt_laws`, `tiered_tx_read_laws`, `tiered_crash_arm_laws` | Unblocks L6 (`journal`), L11 (`read`), L12 (`txn`). |
| **L6** | `journal` | Extract `store-journal-io.hirsel` from `store.hirsel`; split `DirJournalMsg` out of `JournalMsg`; move `store-journal-worker`, `store-journal-recover`, dir half of `store-journal-status`; introduce `journal.DirJournalState` (owns `lock`, `d`, `creation`, `file_seq`, `file_end`, dir `acks`/`journaled`/`inflight`/`uncovered`/`since_cut`/`acked_bytes`/`written*`/`synced`/`poison`/`journal`/`pending`/`journal_task`) and `journal.RecordRetention` (`record_cut`, `recorded_after`). | `store_journal_laws`..`store_journal_laws_7`, `store_disk_laws`, `store_disk_laws_2`, `store_crash_laws`, `store_ace_crash_laws`, `store_replay_laws`, `store_sim_disk_laws` | **Unblocks Perf #8** (Directory journal coalescing & END cadence) and L7 (`history`). |
| **L7** | `history` | Move 7 stateless object-history files (`store-history*`, `store-register`, `store-object-authority`, `store-object-cut`); keep `ClaimAuth` + `HeadView` (3048) clean on `history`. | `authority_laws`, `bucket_laws`, `bucket_order_laws`, `bucket_q5_laws`, `object_cut_laws`, `location_laws` | Unblocks L8 (`host_stream`) and L9 (`object_writer`). |
| **L8** | `host_stream` | Move 10 `store-stream-*.hirsel` files (including `store-stream-archive.hirsel`); implement `ClaimMachine`, `RegisterLane`, `HoldTracker`, and `SettlePort` capability inversion (§5). | `bucket_stream_laws`, `bucket_stream_service_laws`, `bucket_stream_claim_laws`, `bucket_stream_unhold_laws`, `bucket_stream_election_laws`, `bucket_stream_gc_laws`, `bucket_stream_janitor_laws`, `bucket_stream_direct_follow_laws`, `bucket_stream_exit_laws` | Unblocks L9 (`object_writer`). |
| **L9** | `object_writer` | Move 10 object-writer/reclaim/levels/health/follower files; replace `DiskState`'s overlapping optional fields with `Backend = DirBackend(journal.DirJournalState) | ObjectBackend(object_writer.ObjectWriterState)`; move `ObjectJournalMsg` and `journal_idle_task` into `ObjectWriterState`. | `object_journal_laws`, `store_reclaim_laws`, `follower_laws`, `follower_crash_laws`, `replication_laws`, `assembly_laws` | Unblocks L11 (`read`), L12 (`txn`), L13 (`transfer`). |
| **L10** | `indexes` | Move `store-index-registry.hirsel`, `store-aggregate.hirsel`; introduce `indexes.IndexState` (owns `indexes`, `drift` on `Db`). | `store_lookup_laws`, `store_index_crash_laws`, `store_put_laws_2` | Unblocks L12 (`txn`) and L14 (`maintenance`). |
| **L11** | `read` | Move `store-read.hirsel`, `store-read-pages.hirsel`, `store-changes.hirsel`; narrow `Snapshot`/`Reader` construction to `read`. | `store_read_laws`, `store_cursor_laws`, `store_changes_laws`, `store_changes_page_law`, `store_diff_laws`, `store_tx_range_laws` | **Unblocks 3062 #3** (scoped leaf visitor in `read`) and L12 (`txn`). |
| **L12** | `txn` | Extract `store-txn-core.hirsel` from `store.hirsel`; move 12 txn/commit/attempts/mux/fork/cell/log/edge/call-stream/reexec files; introduce `txn.AccountingState` (7 fields) and `txn.CallStreamState` (6 fields) on `Db`. | `store_put_laws`, `store_put_laws_2`, `cell_log_laws`, `store_claim_crash_laws`, `store_claim_quota_laws`, `store_reexec_laws`, `store_swarm_laws`, `store_trust_laws` (+ `runner` L61/L91 smoke) | Unblocks L13 (`transfer`) and L14 (`maintenance`). |
| **L13** | `transfer` | Extract `store-transfer-core.hirsel` from `store.hirsel`; move 8 export/import/prepared/catchup/rounds/install/activation files; introduce `transfer.TransferState` (owns `lease`, `steps`, `imported`, `reserving`, `target` on `DiskState`). | `store_backup_laws`, `store_catchup_laws`, `store_crash_install_laws`, `import_read_laws` | Unblocks L14. |
| **L14** | `maintenance`, `store` cleanup | Move `store-index-build`, `store-verify`, `store-expiry`, `store-projector` into `maintenance` with `maintenance.ProjectorState` (owns `projectors` on `Db`); shrink `store.hirsel` to ~550 L composition root; remove `store.hirsel` from `tools/file_length_ratchet.json`. | `store_projector_laws`, `store_index_crash_laws`, `store_trust_laws`, full `store` + `runner` compile & `fmt.py --check` | Final end state complete. |

*Parallelism note:* Once L1–L4 land, **L5 (`pager`)**, **L6 (`journal`)**, and **L10 (`indexes`)** have disjoint file sets and can run concurrently if desired; after L9, **L10 (`indexes`)** and **L11 (`read`)** are also disjoint.

---

## 8. Prior Art From Peer Storage Engines

All citations below were verified directly against local clones in `/tmp/ref-*`.

| Peer Engine (Clone & Commit) | Boundary Architecture & Citations | Verdict | Wins, Weaknesses & How Hirsel Uses It |
|---|---|---|---|
| **redb** (`/tmp/ref-redb`, `2fc790d`) | `Database` (`src/db.rs:798-803`) is a 3-field composition root (`mem: Arc<TransactionalMemory>`, `transaction_tracker: Arc<TransactionTracker>`, `closed: bool`). Strict acyclic stack: `StorageBackend` (`src/db.rs:72`) → `page_store` (`TransactionalMemory` in `src/tree_store/page_store/page_manager.rs`, owning header, buddy allocator, regions, cache) → `tree_store` (`Btree` + catalog `TableTree` in `src/tree_store/table_tree.rs:73-76` over `PageResolver`) → `transactions` (`WriteTransaction` in `src/transactions.rs:935-957`) → `db` façade. | **ADOPTED** | **Win:** `Database` stays tiny because every subsystem owns its invariants behind one handle (`TransactionalMemory`, `TableTree`, `TransactionTracker`). **Weakness:** `WriteTransaction` (`src/transactions.rs:935-957`) still holds 18 fields directly including policy flags and savepoint locks. **Hirsel:** Adopts redb's thin composition-root pattern for `Db` and `DiskState`, and keeps `catalog` (`TableTree` analogue) cleanly separated from both `manifest`/`pager` (`page_store`) and `txn`. |
| **fjall** (`/tmp/ref-fjall`, `8700604`) | Splits `DatabaseInner` (`src/db.rs:35-61`: `meta_keyspace`, `config`, `supervisor`, `stop_signal`, `is_poisoned: PoisonSignal`, `worker_pool`, `lock_file`) from `SupervisorInner` (`src/supervisor.rs:16-36`: `keyspaces`, `write_buffer_size`, `flush_manager`, `seqno`, `snapshot_tracker`, `journal`, `journal_manager`), with dedicated modules `journal/`, `flush/`, `compaction/`, `keyspace/`, `poison.rs` (`src/poison.rs:8-41`). | **ADAPTED** | **Win:** Dedicated `journal/manager.rs` (`src/journal/manager.rs:44-47`) and `PoisonSignal` (`src/poison.rs:8-18`) keep WAL retention and poison state completely isolated from keyspace/transaction logic. **Weakness:** `Supervisor` and `Keyspace` hold mutual `Arc` cycles that must be manually broken in `DatabaseInner::drop` (`src/db.rs:82-93`, `// IMPORTANT: Break cyclic Arcs`). **Hirsel:** Adopts fjall's dedicated `journal` + `manifest` + poison ownership, but enforces a strictly acyclic module DAG so no cyclic `Arc`s can ever form. |
| **SQLite** (`/tmp/ref-sqlite`, `795e28a`) | Classic 4-tier strict opaque-header stack: VFS → WAL (`src/wal.h:53-75`, `typedef struct Wal Wal`) → Pager (`src/pager.h:12-43`, `typedef struct Pager Pager`) → B-Tree (`src/btree.h`, `src/btreeInt.h:345-363`, `struct Btree`) → VDBE execution & schema (`src/vdbe.h`, `src/vdbeInt.h`). Upper layers never inspect lower struct layouts. | **ADOPTED (improved)** | **Win:** Strict opaque boundaries (`Pager`, `Wal`, `Btree`, `Vdbe`) allow WAL vs rollback-journal changes without touching B-Tree or VDBE execution — exactly the separation Finding #3 asks for between `format`/`journal`, `pages`, `catalog`, and `txn`. **Weakness:** SQLite's `struct Pager` (`src/pager.c:619-654+`) became an internal god object (~80 fields mixing locks, cache, WAL handle, journal mode, and page sizes). **Hirsel:** Splits SQLite's monolithic Pager into three distinct modules — `journal` (WAL segments), `manifest` (packs & locations), and `pager` (Tiered cold-page cache & eviction) — and enforces opacity at compile time via `pub opaque type` (`PROG-2`). |
| **TigerBeetle** (`/tmp/ref-tigerbeetle`, `a2a3c706`) | Separates VSR WAL (`src/vsr/journal.zig`), block Grid + FreeSet + Scrubber (`src/vsr/grid.zig:30-85`), LSM Forest + Manifest (`src/lsm/forest.zig:30-90`, `src/lsm/manifest.zig`), and deterministic `StateMachineType(Storage)` (`src/state_machine.zig:223-258`) using explicit capability injection (`comptime Storage`, callback ports) for deterministic simulation. | **ADOPTED** | **Win:** Explicit capability injection lets the core state machines (journal, grid, state machine) step deterministically without owning transport or storage-root god structs. **Weakness:** Deep Zig `comptime` parameterization couples all generic instantiations at build time. **Hirsel:** Adopts TigerBeetle's capability-injection pattern via Hirsel's zero-trait `scoped fn` ports in `ClaimMachine` (§5), letting `host_stream` stay below `object_writer` and step deterministically in sim tests. |
| **FoundationDB** (`/tmp/ref-foundationdb`, `58f80b1`) | Separates durability log (`fdbserver/tlog/TLogServer.cpp`), commit coordination (`fdbserver/commitproxy/CommitProxyServer.cpp`, `ProxyCommitData.h`), version sequencing (`fdbserver/sequencer/`), and storage-engine layout (`fdbserver/storageserver/`). | **ADAPTED** | **Win:** Clean separation between transaction admission/commit coordination and log/storage persistence makes format and backend evolution orthogonal to transaction semantics. **Weakness:** Process-level RPC boundaries add serialization overhead for embedded single-writer stores. **Hirsel:** Keeps the same logical module separation (`txn` vs `journal`/`object_writer` vs `pages`/`manifest`/`pager`) in-process via zero-cost Hirsel module calls and `pub opaque` structs. |
