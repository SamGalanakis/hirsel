# Independent coverage review

**PASS — coverage of the fixed snapshot is supported.** No material missing distinct subsystem, representation cluster, or layer was identified. No new bounded review row is required. This verdict covers audit coverage and evidence availability only; it does not certify source correctness, approve implementation, or replace the separate materiality review.

Reviewed source: `/workspace/code/hirsel-audit-coverage-review`.

- HEAD before and after inspection: `3ee0621a603659ab0168f565b99012b642415419`.
- Tree before and after inspection: `a4aac830c45398a66591f2c44b707aaf3cef281b`.
- `git status --porcelain`: empty before and after inspection, and after writing this report.
- No source edits, tests, builds, network calls, live-data/config reads, commits, or delegation. Only this external report was written.

## Enumeration and exact ownership

Independently compared `git ls-files` with `ownership.json`: all **557 tracked paths** are accounted for by **470 owned paths** and **87 explicit context exclusions**. There are no missing or stale paths, no overlap between the owned/excluded sets, and no competing default owners.

The six shared files account for the difference between complete-file lists and the inventory's displayed file counts. Their defaults appear in `shared_file_defaults`; they are not missing from the dispatched ownership contract. Default assignments agree with `file_ownership`.

Independently extracted the current SQL table/index names and top-level Rust/TypeScript struct/enum/type/interface names from the six shared files. All **123 definitions** match the explicit split ledger and their recorded source-line anchors:

| Shared file | Definitions | Ownership result |
|---|---:|---|
| `crates/hirsel-host/src/storage/current.sql` | 41 tables/indexes | Every table and index has one explicit owner, including receipts, generated request columns' table, cancellation, plugin thread KV, and artifact staging/association tables. |
| `crates/hirsel-proto/src/thread.rs` | 9 | Identity/detail, activity/turn, and Related definitions split explicitly. |
| `crates/hirsel-proto/src/chat.rs` | 4 | Chat/message/tool-summary and Blob definitions split explicitly. |
| `crates/hirsel-proto/src/client.rs` | 5 | SendMode, AgentSlot, PushPlatform and connection envelopes split explicitly. |
| `crates/hirsel-proto/src/host.rs` | 1 | HostToClient belongs to C14. |
| `app/src/protocol.ts` | 63 | All local definitions belong to C14; imported Thread/artifact shapes retain their own file owners. |

Every split agrees with the corresponding cluster's `owned_definitions`; no exact definition is double-owned. All listed existing-test and major-consumer paths exist. Whole-file defaults do not override named definitions. Cross-layer findings do not make their consumer files co-owned.

## Completion evidence for every cluster

Read bounded coverage/skip sections and relevant finding/disposition sections from all 27 worker reports. The table below records substantive review evidence, not just path enumeration. Worker paths are `workers/<full cluster ID>.md`; accepted F-notes and coordinator rejection/skip notes are under `verified/` and indexed by `dispositions.md`.

| Cluster | Final status | Evidence supporting completed coverage |
|---|---|---|
| C01 Thread identity | skip | Worker lines 186–247 examine row decoding, hierarchy, revision/receipt writes, summary derivation, icon patches, shared DDL/proto and tests. Nested action identity candidate has an explicit coordinator disposition. |
| C02 Turn lifecycle | recommend | Worker lines 5–7, 47–58 trace durable requests through execution, lanes, observations and publication; explicitly discuss cancellation, recovery, task epochs and optional preaccept turn identity. F01 is retained; Host-preference proposal is separately rejected. |
| C03 Conversation | skip | Worker lines 46–86 and 768–796 cover message insertion, idempotency, mentions, joins, decoding, tool summaries, wire and native/web consumers. Both candidates have recorded dispositions; tracked tool-ID work is acknowledged. |
| C04 Blobs | recommend | Worker lines 246–261 cover upload admission, metadata, SQL joins/order, signed URLs, filesystem paths, pending web uploads and native projection. F06/F07 provide finding evidence. |
| C05 Delegation/scope | skip | Worker coverage table and lines 267–351 cover caller/history fencing, subtree permissions, independent cursors, reports, receipts, execution revocation, bridge replay/telemetry, catalog and tests. Receipt denormalization has a rejection note. |
| C06 Artifacts | skip | Worker lines 175–208 cover draft/CAS/receipt transactions, global content, explicit grants, backlinks, showcase and all seven shared DDL definitions. Metadata proposal has a disposition. |
| C07 Related | skip | Worker lines 38–55 and 334–361 cover normalization, one-of targets, uniqueness, revisions, receipt replay and web/native conversions. Malformed-receiver candidate is explicitly rejected. |
| C08 Processes/wakes | recommend | Worker lines 46–103 and 732–799 cover WakeSource/WakeMessage, packing, ExitSlot, dispatch, timers, process groups, monitor storage/projection and tests. F08 remains; process-inventory restoration is rejected. |
| C09 Codex driver | skip | Worker lines 202–233 cover session/request state, startup/retirement, isolation/catalog, stdout/terminal decoding, both Python peers and integration fixtures. Both malformed-peer proposals have dispositions. |
| C10 Claude driver | skip | Worker lines 295–307 cover requests, receipt/event classification, launch isolation, preflight, process cleanup, fixture peer and lifecycle tests. Both peer-shape candidates have dispositions. |
| C11 Shared drivers | recommend | Worker lines 174–199 cover SessionRegistry, EventHub retention/replay, completion races, process groups, stderr drain, fake driver, scoped MCP fixtures and shared types. F10 covers shell projection. |
| C12 Lash runtime | recommend | Worker identifies all eight files and explicitly covers observation routing, session bootstrap, fingerprints, dynamic catalog, shared ConfigStore, result schemas and cleanup. F09 crosses the tool/storage/runner condition boundary. |
| C13 Config | recommend | Worker lines 380–425 cover env/bootstrap, TOML document/cache, provider parsing, prompt provenance, model selection, skill discovery, subagent variants, wire snapshots and preference table. F11 retained; roster redesign rejected. |
| C14 Protocol/connection | recommend | Worker lines 32–91 and 627–671 cover all shared envelopes, WSS/Iroh framing/auth, hello snapshots, dedupe, browser reconnect/history, bounded request state, reducers and tests. F03/F04 retained. |
| C15 Web Threads | recommend | Worker lines 122–160 cover action/error correlation, draft/upload state, mention identity, timeline joins, detail merging, navigation/tree/status/icon state and tests. F05 retained; root-owned regressions explicitly deduplicated. |
| C16 Web artifacts | skip | Worker lines 130–169 cover compiler worker messages/lifecycle, runtime bundle cache, iframe policy, draft persistence, requests/stale guards, showcase state, downloads and tests. Both proposals have explicit dispositions. |
| C17 Web Related | skip | Worker lines 27–41 map exact parser, link, cache/request and rendering symbols to consumers/tests; subsequent five candidate checks supply concrete skip reasons. Coordinator skip note exists. |
| C18 Web views | recommend | Worker lines 24–47 trace Canvas to reducers/transport and ViewRenderer through forms, fields, registry and events; explicitly assess closed node/token catalogs and unused viewSlot. F13/F14 retained with coordinator narrowing. |
| C19 Web settings | recommend | Worker lines 289–348 enumerate settings state, drafts, pending/error settlement, plugin descriptors, notifications, local preferences and tests. F15/F16 retained; lesser cases explicitly skipped. |
| C20 Web shell | recommend | Worker lines 205–219 distinguish bootstrap, UI, Markdown, preferences/theme, browser utilities, PWA, timers, styles/assets and tests with substantive skip explanations. F12 retained; cosmetic theme proposal rejected. |
| C21 Client core | recommend | Worker lines 533–570 cover config/identity, lifecycle/commands/observers, pending queues, optimistic echo, reset, detail/Related, turn streams and tests. F02 retained; public config redesign rejected. |
| C22 FFI/Android | recommend | Worker lines 798–850 explicitly cover every assigned path: FFI records/enums/callback conversions, generated Kotlin, pairing/token/settings persistence, notification delivery, Compose/QR lifecycle, manifest/resources and tests. F17/F18 retained. |
| C23 Plugins | skip | Worker lines 352–388 trace folder/manifest generation through registration, supervision, scoped capabilities, settings/KV, routes/tools, browser loader/slots/push and tests. Empty installed registry is verified, not used to skip framework inspection. Both candidates have dispositions. |
| C24 Views/instruments | recommend | Worker lines 26–60 and 345–380 cover primitive validators, binding traversal, template cache, source/params/patch projection, history fence, JSON Patch, instrument action derivation and fixtures. F19 retained; cache cleanup rejected as nonmaterial. |
| C25 Host operations | recommend | Worker lines 135–160 cover auth/pairing, device persistence, schema validation/pragmas, reset, push payload/retry, health, service hardening and runtime broadcasts. F20 retained; push reset consolidated into F18. |
| C26 Build/tooling | recommend | Worker lines 35–112 cover CI/release, manifests/locks, wrapper/tool versions, native/bindgen pipeline, hooks, source-size and static gates, Vite/TS/lint and build metadata. F23 retained; blanket lock policy deferred. |
| C27 Test infrastructure | recommend | Worker lines 140–165 cover mock world/receipts/projections, websocket fixtures, Vitest environment, artifact/browser smokes and current/retired harnesses. F21/F22 retained. |

This supports the authoritative cluster totals: **17 recommend, 10 skip, zero unfinished clusters**. A rejected worker candidate is not unfinished coverage when its remaining surface has explicit review evidence and the coordinator records the rejection.

## Evidence presence and overlap checks

All worker/verified Markdown references in the inventory, report and disposition index resolve. A textual scan of explicit source-path references across worker and verification reports found 350 distinct paths. Missing-path exceptions are proposed files (`preview-policy.ts`, `PushRegistration.kt`), deliberately absent retired runners used as F22 evidence, and the unambiguous shorthand `templates/spec.rs` for `crates/hirsel-host/src/templates/spec.rs`; none is a claimed existing implementation missing from this snapshot. Checked explicit source line starts are within their files.

Reopened or searched current source for the retained findings' actual representation anchors: terminal CLI FIFO admission; the four history-less mutation variants; HelloBroadcastDedupe's view map; everAuthed; Thread action/error correlation; image MIME disposition and stored blob paths; monitor activity/regex conversion; shell stderr projection; StoredProvider parsing; PaneHeader capabilities; view placement/order; Debug preference and provider latch; FCM data and registration/reset; validate_form/FormNode; production ConnectInfo; mock create projection; retired harness; and both Android generation recipes. Those symbols and quoted code fragments exist in the fixed tree. This is evidence-presence verification, not a new behavioral certification of all 23 findings or every worker consumer count.

No unresolved definition ownership overlap was found. In particular:

- C02 alone owns `cli_turn.rs`; C15 alone owns web Thread store/messages.
- C14 owns connection/envelope definitions; native core and FFI/Android own their local conversion representations under C21/C22.
- F02/F05 share a mutation wire seam but have distinct identity and result-attribution outcomes; the dependency is recorded.
- F13/F14 reference C24's host ViewManager as a consumer/implementation coordination boundary; C24 explicitly deduplicates them.
- F18 consolidates C22 token refresh and C25 history-reset findings instead of leaving two accepted owners for one recommendation.
- C26 build publication and C27 executable fixtures are distinct; plugin sync is explicitly C23-owned.

## Exclusions and bookkeeping

**No bogus material exclusion identified.** The 87 excluded paths are instruction/product/research/design material, static reference assets, a license, ignore metadata and an empty placeholder. The executable files under `docs/research/prospect-subagent-drivers-2026-09-09/verification-probes/` deserve explicit attention: their README says they assert the September 9 defects, use a captured historical source baseline and extracted definitions, and are packaged research reproductions. Read-only reference search found no active source/CI/static-gate consumer outside research. Their exclusion as frozen research evidence is justified; they should not be counted as current regression-test coverage. Current executable Python driver peers, cross-system browser harnesses, native generation, plugin generation, resources and gates are owned and reviewed.

The issue exclusions were treated as the supplied snapshot, without live verification. They suppress duplicate findings, not inspection of their containing subsystems. Planned Android artifact viewing (#10) is explicitly distinguished from current native conversion coverage. No broad issue exclusion was found to conceal an unreviewed subsystem.

**Bookkeeping correction before publishing the final audit:** `ownership.json.file_rows[*].status` still says `queued` for all 470 rows, while `clusters[*].status` and the rendered inventory report completed review. The declared status policy makes clusters authoritative and the worker evidence supports them, so this is not a material coverage gap. Regenerate these per-file statuses from their owner clusters or explicitly remove their implication of current completion status. Do not open or silently broaden any completed cluster to fix this metadata inconsistency. The C02 inventory text also retains “pending audit-of-audit”; finalization should replace that provisional wording with the completed independent gate outcomes.

## Final source integrity

After writing this report, repeated `git rev-parse HEAD HEAD^{tree}` and `git status --porcelain`: expected HEAD/tree unchanged and porcelain empty. No repository files were modified. Coverage PASS remains limited to this fixed snapshot and the supplied audit evidence.
