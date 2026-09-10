# Prospect: native subagents and lightweight skills

2026-09-09. Reference revisions: [bb 4ed2743](https://github.com/get-bb/bb/tree/4ed2743219e8a6ecd7d2c2535c68865dcf821b20), [T3 Code e16b8b0](https://github.com/pingdotgg/t3code/tree/e16b8b059c9f5ff6dfed1addecffb831c6aee043), and [Pi 05c6229](https://github.com/badlogic/pi-mono/tree/05c6229813414010445558db9a80c84e15d65e70). Local clones were inspected; the pre-existing conflicted /tmp/ref-pi-mono was preserved and a clean /tmp/ref-pi-skills clone used instead.

> **Current implementation note (2026-09-10):** Source links below describe the historical pre-cutover architecture. Native CLI turns now enter through `crates/hirsel-host/src/lash_runtime/cli_turn.rs` and commit terminal output through `crates/hirsel-host/src/storage/thread_completion.rs`; there is no general `tools/subagents.rs` execution path. The hermetic `cli_delivery_tests.rs` suite revalidated issue #6 across transient SQLite failures, exactly-once replay, separate wake retries, concurrent progress, and history replacement.

## Verdict

Keep Hirsel's native Rust driver architecture. The useful improvements are inside its command and process-lifetime contracts: correlate provider replies, distinguish active work from queued input, scope events to the correct provider thread, and deliver one durable terminal result even when startup or transport fails. bb and T3 contain transferable mechanisms for those guarantees; their larger plugin/SDK/runtime stacks are unnecessary here.

The separate skills request is implemented with Pi's small core idea: local SKILL.md metadata in guidance, lazy body loading through existing shell tools, and host-side /skill:name expansion. See [Skills](../../skills.md) for use and configuration. The Owner subsequently approved the five GitHub issues below and implementation of every fix, with current-run steering selected. All five fixes and the separately requested skills feature are implemented and validated locally. GitHub issues remain open for the normal code publication and merge step.

## How Hirsel currently delegates

The model-visible subagents.spawn contract comes from the enabled provider/model/variant catalog. Its execution creates a Lash supervisor process; ToolSuite resolves the requested model and starts the native driver. Codex launches an app-server process, starts a native thread, then submits the prompt through JSON-RPC. Claude launches headless stream-json with piped input/output and sends a user envelope. Both use the selected cwd/model/effort and full-auto mode.

Driver output becomes Started, Progress or Terminal. The host keeps a process record and persists/broadcasts updates. On the first Terminal it retires the CLI session, publishes the terminal bus event, and the Lash bridge appends the runtime terminal result used by waits and wake handling. subagents.prompt and interrupt route back through the driver; progress/list inspect host records. After owner loss/restart, delegated work is abandoned for Agent judgment, not automatically resumed. These boundaries follow ADRs 0003/0004/0015, while ADR 0016 keeps durable Thread ownership distinct from Process execution.

Current source: [driver contract](../../../crates/hirsel-drivers/src/types.rs), [Codex](../../../crates/hirsel-drivers/src/codex.rs), [Claude](../../../crates/hirsel-drivers/src/claude.rs), [host delegation](../../../crates/hirsel-host/src/tools/subagents.rs), [runtime bridge](../../../crates/hirsel-host/src/lash_runtime/bridges.rs). Detailed paths and line references are in the independently checked [verification record](verification-drivers.md).

## Verified improvement batch

This table records defects verified against the pre-change working tree; the implementation below addresses them. Priority is relative implementation order, not a claim that all conditions occur in every installed provider version.

| Priority | Checkable outcome | Evidence and transfer |
| --- | --- | --- |
| 1 | Command success means the native control succeeded; errors/deadlines reach the caller. | Actual-driver fake peer rejects turn/start while Hirsel returns success. Startup also sends initialize/thread-start without ordered acknowledgement. bb and T3 correlate response IDs; T3 performs initialize → initialized → thread-start. Keep native Claude text-input guarantees distinct from control receipts. |
| 2 | Only the root provider thread can change root turn identity or settle its run. | Replaying T3's captured native multi-agent notifications makes Hirsel report Done for a child completion, with no root completion in the replay. T3 filters root state updates by thread ID. This is triggered when native child traffic occurs; this round did not establish that every Hirsel provider configuration emits it. |
| 3 | A CLI is owned and supervised from spawn until one terminal settlement. | A malformed-startup fake Codex survives spawn's error return. A fake Claude exits zero without a result and leaves its event stream unsettled. Acquire the cleanup guard/start stderr draining immediately, roll back failed handoff, and settle missing-terminal exit irrespective of exit code. bb also bounds post-exit pipe draining; T3 owns children in a scope. |
| 4 | Define running follow-up input as steering or intentional queued work, then implement that exact contract. | A deterministic A-active/B-queued reply makes Hirsel target B on interrupt; host retirement at A's terminal can discard B. bb uses turn/steer with expectedTurnId; T3 deliberately uses turn/start but preserves active A separately. Recommend steering for Hirsel's current one-run/one-terminal model. This is a design choice, not an assertion that all Codex versions reject busy turn/start. |
| 5 | Terminal delivery is acknowledged only after durable append succeeds, or retries the same replay key. | The receiver marks a process seen before handling; bridge append failure only logs. An extracted-current-code probe shows republishing does not reach that receiver. Retained events remain recoverable by a new subscriber, so this is a stalled live-delivery path, not proof the result is destroyed forever. Retrying delivery does not violate the no-auto-restart ADR. |

The bb reader, T3 reader and independent Hirsel audit converged on acknowledgement, startup ownership and missing-terminal settlement. Both peer readers converged on the follow-up mismatch. The late native-child candidate was independently verified against captured wire data; the bridge finding came from the Hirsel audit and was re-derived separately. Verification refined provider-dependent claims and corrected source line ranges before synthesis.

Adopt hermetic fake native CLI peers with these fixes. Existing fake-driver tests and real-provider smoke tests already exist; the missing coverage is exercising actual protocol readers against rejected commands, startup failure, queued IDs, child-thread traffic and clean EOF. The [probe record](verification-drivers.md) documents what was actually executed and the remaining conditional cases.

## Skills: small prospect and implementation

[Pi evidence and choices](skills-prospect.md) were independently checked in [verification-skills.md](verification-skills.md). Adopted:

- A filesystem SKILL.md with real YAML frontmatter, a compact name/description/path catalog, and instructions loaded on demand. Agent discovery uses existing shell.run; there is no new execution runtime or per-skill tool schema.
- /skill:name plus trailing arguments expands at host submission. The visible message remains the short command; the full instructions and source directory are captured with the durable Agent request. Duplicate client IDs retain the original request even if the file changes or disappears. Both Thread submission and the older Owner-message route use atomic message/request persistence.
- Ordered roots, sorted traversal, canonical-directory deduplication, a SKILL.md recursion boundary, explicit-only skills, and a per-file size limit. Existing plugin instruction packs continue through their existing mechanism.

Hirsel is intentionally narrower than Pi: no package installer, extension hooks, directory-ancestor discovery, settings manager, prompt-template language or dedicated picker. Its Host has one set of roots; Thread focus does not select a project directory. Explicit configured roots win, then data-dir skills, cwd .agents/skills, then user .agents/skills. Installation into existing roots requires no restart; environment root changes do.

## Considered, not adopted

- ACP, Claude SDK adoption, a Node bridge, Effect/event-sourcing migration, or replacement of Lash/SQLite. These would conflict with settled choices or add machinery without fixing the measured guarantees.
- Automatic provider retry/resume after restart. ADR 0004 explicitly leaves that judgment to the Agent.
- Approval UI or a different permission policy. The current full-auto policy is deliberate.
- Copying T3's hard-stop semantics wholesale. Hirsel already distinguishes native interrupt from hard abandonment; acknowledgement and bounded cleanup can improve without collapsing them.
- Assuming peer RPC waits are universally bounded. T3's base await can be unbounded; bb timeouts are optional in the transport. Add explicit deadlines to the adopted contract.
- Broader model catalogs, attachments, account pooling, buffering redesign or product Thread changes. Existing catalogs and the earlier Thread prospect are excluded, and the rest are outside this scoped verified batch.
- Treating SKILL.md metadata as tool permission grants, automatically executing scripts, or injecting all skill bodies into every prompt.

## Tracking and validation

GitHub issues in SamGalanakis/hirsel are the task tracker, per the Owner's explicit instruction and updated CONTRIBUTING.md/agent entrypoints. Fresh issue enumeration returned no open issues. Open PR #1 (diff-hygiene and secret-scan merge gates) and settled ADR/prior-prospect ground are excluded in [exclusions.md](exclusions.md). The Owner subsequently requested tickets and implementation; issues #2–#6 track the five outcomes. See the issue table below.

Driver verification used isolated byte-identical source copies with fake native peers, plus a captured T3 Codex wire fixture and an extracted terminal-bus probe. It used no real provider inference and changed no live process. Full details and limitations: [verification-drivers.md](verification-drivers.md).

Final combined validation: `cargo test --workspace` passed with **397 passed, zero failures, four intentionally ignored**. The ignored cases are the two paid native-provider smokes, a public relay smoke, and an example doctest. `bash scripts/check-static.sh` passed: workspace/all-target clippy with warnings denied, Rust formatting, production file-size and plugin-sync checks, web lint and TypeScript. Diff checks include new files. The driver crate passed 34 tests; the host passed 296. The [host delivery implementation record](host-delivery-implementation.md) details the seven new receiver, bridge and handoff tests. No paid inference or live host restart was used. The final delta was checked against the pre-existing main-worktree files before integration; its index and both live host process identities were preserved.

## Approved implementation tracking

| Outcome | GitHub issue |
| --- | --- |
| Correlated native controls | [#2](https://github.com/SamGalanakis/hirsel/issues/2) |
| Root provider thread isolation | [#3](https://github.com/SamGalanakis/hirsel/issues/3) |
| Continuous child ownership and terminal settlement | [#4](https://github.com/SamGalanakis/hirsel/issues/4) |
| Current-run steering | [#5](https://github.com/SamGalanakis/hirsel/issues/5) |
| Durable terminal-result delivery | [#6](https://github.com/SamGalanakis/hirsel/issues/6) |
| Separately requested local skills feature | [#7](https://github.com/SamGalanakis/hirsel/issues/7) |

The Owner approved steering rather than queued provider turns. Preserve one-run/one-terminal lifetime and do not automatically restart abandoned work.

## Implemented contracts

- Codex startup awaits initialize, sends initialized, awaits thread/start, and awaits the initial turn/start. Request IDs correlate responses and errors; each control has a 30-second deadline covering writes and the response. Unsupported native server requests receive an explicit JSON-RPC error.
- Codex uses the thread identity from its correlated startup response. Native child notifications cannot change root identity or settle the root run. Follow-up input uses turn/steer with expectedTurnId and validates the returned turn identity; interrupt targets the active root turn.
- Claude uses replayed user UUIDs to acknowledge native input receipt and correlates interrupt control responses. Receipt does not guarantee the model incorporated input before completion: Claude's stream protocol has no Codex-style expected-turn guard. Unacknowledged input fails if terminal settlement wins the race.
- Both drivers own the CLI process group from spawn, drain stderr during startup, clean up cancelled or failed startup, and observe direct child exit independently of inherited stdout. Post-exit draining is bounded to 500 ms. Missing terminal output becomes failure, including clean exit zero; the event hub admits one terminal and closes its stream afterward.
- Host handoff owns rollback until the event pump is installed. Failed subscription, failed persistence, or cancelled handoff retires the acquired driver handle. Terminal delivery retries a stable replay key with bounded backoff; wake persistence retries separately. One failing delivery does not block another process, and acknowledgement follows successful handling. These retries deliver existing results and never re-execute delegated work.

Independent implementation review additionally caught and fixed blocked-stdin retirement, detached stderr draining, and the terminal-publication/pending-request admission race. Native-peer tests exercise the real driver readers without paid provider inference; SQLite-backed bridge tests exercise failed writes, lost successful-write receipts, wake failure, concurrent progress, and abort/replay.

[Codex cross-review](codex-cross-review.md) also moved server-response writes into the supervisor's event loop so stdin backpressure cannot block child-exit detection. A failed reply write or already exited child still permits bounded draining of a valid final notification.

The existing fork-wake branch acknowledges asynchronous dispatch acceptance; it does not wait for durable triage completion. The fallback wake path acknowledges after wake storage succeeds. See [Claude implementation and protocol evidence](claude-lifecycle-implementation.md) for its native receipt boundary and driver test details.

The real wake-queue regression also exposed a pre-existing malformed fallback draft: its manual construction omitted Lash's structural process-wake source and inherited authority, causing durable enqueue rejection. The bridge now supplies those fields through Lash's public draft builders, matching the pinned runtime's required draft shape. The wake retry test and issue #6 cover this correction.
