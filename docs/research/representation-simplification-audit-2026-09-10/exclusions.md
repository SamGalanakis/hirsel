# Exclusions and settled decisions

Issue state is supplied by the root snapshot map, not live service reads. Reopen only for a independently verified regression outside the accepted outcome.

- #14: Show one persistent artifact beside each Thread conversation. Existing tracked outcome; verify final integrated source before classifying a delta.
- #13: Replace Inspect execution with clear turn progress and visible failure outcomes. Existing tracked outcome; verify final integrated source before classifying a delta.
- #12: Restrict pins to top-level Threads and retain one nested list. Existing tracked outcome; verify final integrated source before classifying a delta.
- #11: Block artifact iframe self-navigation from making network requests. Existing tracked outcome; verify final integrated source before classifying a delta.
- #10: Support opening, previewing and downloading artifacts in Android. Planned native artifact viewer gap; never report as a new finding.
- #9: Preserve whitespace in artifact exact-match edits. Existing tracked outcome; verify final integrated source before classifying a delta.
- #8: Add configurable Thread icons for Owners and agents. Existing tracked outcome; verify final integrated source before classifying a delta.
- #7: Add local SKILL.md invocation and lazy skill discovery. Existing tracked outcome; verify final integrated source before classifying a delta.
- #6: Acknowledge subagent terminal delivery only after durable handling. Existing tracked outcome; verify final integrated source before classifying a delta.
- #5: Make subagents.prompt steer the current delegated run. Existing tracked outcome; verify final integrated source before classifying a delta.
- #4: Own subagent startup and settle every ended CLI run exactly once. Existing tracked outcome; verify final integrated source before classifying a delta.
- #3: Keep native Codex child notifications from changing or settling the root run. Existing tracked outcome; verify final integrated source before classifying a delta.
- #2: Await native subagent command acknowledgements and surface rejection. Existing tracked outcome; verify final integrated source before classifying a delta.

- Thread pin, settlement, attention, visibility, read and execution are independent dimensions (PRODUCT/ADR0016). Do not collapse valid orthogonal state just because booleans exist. Final top-level pin constraint supersedes old PRODUCT copy if final source has it.
- Artifacts are mutable global content without versions; explicit references are access grants; backlinks must remain scoped. Shared ownership and current-content cards are intentional (ADR0017).
- No automatic delegated-work restart; monitors are the intentional rerunnable exception (ADR0004).
- Native Rust CLI drivers and full-trust compiled in-tree plugins are settled (ADR0003/0014).
- Protocol is transport-agnostic; native iroh and browser WSS intentionally differ (ADR0006/0011).
- Final shipped schema4 only, direct current.sql layout and exact catalog validation. No compatibility/migrations in shipped source. History deletion allowed if necessary for a verified final model; no gratuitous deletion.
- Agent-managed wakes and ephemeral Thread-local triage are policy boundaries (ADR0005/0015).
- FFI-generated Kotlin is generated output; owning Rust conversion and generation pipeline are the implementation seams.

- #16: interrupted CLI FIFO head blocks later Thread work after restart. Root independently verified F01; implementation in flight in separate worktree. Report as confirmed/tracked, not duplicate new issue.

- #18: CI stderr-drain fixture bash login/20k printf exceeds 2s. Root/Sol own post-snapshot diagnosis and repair; do not duplicate. Existing #4 is broader lifecycle context.

- Root fresh feature review regressions under #13: same-history web mergeDetail overwrites terminal with stale Running; persisted tool summary discards actual tool ID and guesses same-name completion pairing. Root owns fixes; see /tmp/hirsel-feature-review-contracts*.md and /tmp/hirsel-contract-{reconnect,tool-pair}-probe.json. Record matching audit mechanisms as confirmed/in-flight, not new issues.
- Root fresh feature review regression under #12: legacy child-pin projection violates final root-only pin contract. Root owns clean cutover; do not duplicate.
- Root-accepted audit F02: missing captured history on identity-bound mutation commands (ThreadAction, SendThreadMessage, CancelTurn, CreateThread with parent). Canonical evidence verified/F02-thread-action-history.md; consolidate any matching discovery under F02 instead of proposing separate wire cutovers.
- Independently verified audit F03/F04: removed hello-snapshot view cache suppresses recreation; web everAuthed latch masks auth rejection after reconnect. Canonical notes in verified/. Root informed; no duplicate new recommendation.

- Root tracking now maps F02 to #19, F03 to #20, F04 to #21 and F05 to #23. F05 is dependent on the F02 wire lane; avoid overlapping API implementations.
- #22: root independently found view-only fresh catalog bypass; separate schema implementation is underway. C24/C26 and other audits must deduplicate this mechanism.

- Coordinator latent-input skips: C01 nested action-id mismatch, C07 malformed Related item/envelope mismatch, C09 malformed/empty Codex frames. Do not repeat absent a real producer/race witness; dispositions in verified/. C03 reverse-receipt read failure was factually wrong (query_row ignores later rows), and duplicate mention labels do not justify relational schema churn.
- C08 policy correction: do not restore Subagent ProcessInfo inventory; Rust monitor-only is current contract and Thread execution owns CLI work. The real false last-fired display is narrowed to latest activity wording; no new wake timestamp absent a product requirement.

- Root independently accepted F06/#24 inline blob policy and F07/#25 current-root blob resolution; separate Sol implementation includes isolated browser and stopped-directory relocation/queued-image checks. F08/#26 is only truthful latest-activity display; no last_wake field or restored subagent producer.

- Coordinator F09: malformed regex monitor is accepted at tool/storage boundary then compiles to false forever. Canonical owner C12 ingress; C08 storage/runner consumers. Evidence verified/F09-monitor-condition-validation.md. Distinct from F08 label; do not create a second condition finding.

- Root accepted F10/#28 shell timeout stderr preservation and F11/#29 editable provider required-field validation; same bounded host-fix lane, no new protocol/schema/timing. C13 roster union and C16 policy-module/source-preference proposals rejected as nonmaterial or unsupported semantics.

- F12 (owner C20) expanded prompt Saving badge is hidden by PaneHeader close fallback; render independent badge/close siblings. C19 should dedupe.
- F13/F14 (owner C18, C24 host consumers): advertised chat placement has no current renderer; Canvas-only clean contract. ViewManager BTreeMap snapshot order conflicts with live latest-upsert order; prefer ordered host collection and unchanged wire over new ordinal. Canonical notes verified/F13-canvas-only-view-contract.md and F14-view-order-ownership.md. C24 should dedupe these mechanisms.

- Root independently accepted F12/#30, F13/#31 and F14/#32. Settled target: remove placement entirely, reject unsupported chat input; preserve latest-upsert order with a single ordered host collection, no wire ordinal. Separate Sol implementation; C24 must dedupe.
