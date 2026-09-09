# Doctrine review: durable Tasks through the Agent contract

Read-only review, 2026-09-09. Hirsel inspected in the working tree based on `2df5888b8838ec2ea3c3cdadf4707b03bc4c4897`, including the pre-existing Lash upgrade edits; T3 Code reference `6c583620ff7ad3235b135af7107c0543467eecfa`. Read `exclusions.md` before inspecting either implementation; read T3 Code's `AGENTS.md` and `docs/user/thread-sidebar.md` before auditing Hirsel. No runtime changes, live-data mutation, or implementation. Findings are source-derived; no test suites or browsers were run.

## Borrowed doctrine, with limits

- T3 Code `AGENTS.md:33–37` asks for the smallest model that makes correct behavior unsurprising. Its glossary identifies a thread as durable work/conversation history (`AGENTS.md:55–56`); its user guide says to start a separate thread for a separate task (`docs/user/thread-sidebar.md:3–11`). The transferable rule is explicit work identity, **not** T3 Code's per-thread Agent/session model.
- `AGENTS.md:65–75` requires entry-point, client, wire-contract, reverse-state, connection-mode, and documentation coverage. The relevant test doctrine is observable behavior and focused proof (`AGENTS.md:104–110`). Hirsel need not adopt Effect, event sourcing, or T3 Code's implementation stack.
- The thread guide separates list placement from activity (`docs/user/thread-sidebar.md:64–67`) and explains explicit settlement/reopening (`75–79`). T3 Code also has automatic settlement (`81–92`); that policy must **not** be imported into Hirsel, whose ADR-0009 preserves explicit open/done settlement.

## 1. Define ordinary-work creation independently of notification tone

**Delta beyond the known missing groceries Event:** the Agent has no honest creation contract for durable work that asks no question and reports no digest. The three creation contracts are judgment, FYI, and summary (`crates/hirsel-host/src/lash_runtime/tool_defs.rs:67–93`). Judgment requires 2–4 options; notify unconditionally persists Info and summary unconditionally persists Summary (`crates/hirsel-host/src/tools/events.rs:88–108,112–157`). The persisted kind has exactly these three values (`crates/hirsel-proto/src/event.rs:37–43`). None expresses “keep this ordinary work visible until explicitly done.”

The Agent prompt compounds the mismatch: it instructs “create exactly one Task” by choosing judgment/info/summary, and treats Task creation as routing a result outside a warm exchange (`prompts/agent.md:33,45–47`). This is a result-publication model beneath a durable-work product. The known Info exclusion (`app/src/store/selectors.ts:123–134`) is one symptom, not the finding.

**Smallest-model application:** give ordinary work an explicit creation operation and a typed result carrying its durable Task id. Its minimum state is identity, Anchor, description/instrument, open/done settlement, and attention. This can reuse the existing SQLite row and compatibility envelope; a separate database table is not required to establish the invariant. Distinguish work membership from update tone at the producer boundary.

**Alternatives:** a new `task/work` variant in the existing record is a smaller storage change but leaves more legacy Event semantics to fence off. A first-class Task wire type separates the concepts clearly but widens protocol/client work. Merely displaying Info would display session housekeeping; routing everything through Summary substitutes a digest for work and inherits the settlement problem below. Choose representation after agreeing the invariant, not before.

**Decision implications:** this realizes `CONTEXT.md:16–28`, not a new product object. ADR-0004's current clarification (`docs/adr/0004-no-task-abstraction.md:3–7`) rejects host execution/retry specifications; a visible record does not schedule, retry, or restore any Process. The historical “no task table” wording should be reconciled if a physical Task table is chosen, without changing recovery ownership.

## 2. Make attention mutable on the same Task across ordinary wakes

Hirsel already preserves id and Anchor for adaptive instruments. However, the sole current recomposition tool requires the exact Task whose generated action woke the turn (`crates/hirsel-host/src/lash_runtime/executor.rs:203–218`). A normal Owner request or background completion wake cannot use it. Storage updates only description and UI (`crates/hirsel-host/src/storage/events.rs:153–184`), preserving kind and `requires_response`; validation couples judgment and response demand (`451–470`). Tests deliberately enforce this action-turn restriction (`crates/hirsel-host/src/lash_runtime/tests.rs:1011–1046`).

Consequently, a Task progressing “work moving → decision needed → work moving” cannot update its response classification through that operation: a Summary stays outside the needs-you count, while an open Judgment remains in that count even if its next instrument is an autonomous checkpoint (`app/src/store/selectors.ts:157–164`). The ordinary status label likewise derives from kind, alongside read and the optional blocking flag (`app/src/components/tasks/task-model.ts:17–22`). This is an inference from the restricted update fields and selectors, not a replayed live failure; presentation and read state are not generally immutable.

**Smallest-model application:** permit the authoritative Agent to update an explicitly identified Task's presentation and attention during ordinary orchestration turns, keeping identity, Anchor, and settlement independent. Keep generated-action targeting checks on the generated-action path; do not turn every background update into an implicit Owner action. No new Process state machine is required.

**Alternatives:** keep `recompose` narrow and add an explicit Task-update command; or generalize the producer update contract while retaining action-specific target validation. A new Task per judgment would break the stated continuous identity/instrument model (`CONTEXT.md:28`), so it is a substantive product change, not an equivalent implementation.

**Doctrine attribution:** T3 Code's explicit durable-work identity and separate activity/list semantics motivate separating the changing fact from the durable object. Hirsel's existing identity-preserving storage operation is the correct starting point. This extends its current action-only producer authority and must be recorded as such; it does not change ADR-0009 settlement or ADR-0004 recovery.

## 3. Do not equate “read quiet work” with “finished work”

An open, read Summary without the optional blocking flag is labeled “moving” (`app/src/components/tasks/task-model.ts:17–22`), yet `isEventFinished` declares it finished because it is read and does not require a response (`app/src/store/selectors.ts:147–152`). Selecting a Task chip marks it read (`app/src/components/tasks/TaskShell.tsx:330–332`; wire operation in `app/src/lib/event-decide.ts:77–84`). The Host's clear-finished SQL uses the same read/non-response rule (`crates/hirsel-host/src/storage/events.rs:213–230`). Thus using Summary as the new ordinary-work type makes merely inspected open work eligible for a “Clear finished” action.

This does **not** mean reading immediately changes `status` to done: `mark_ping_read` only writes `read` (`storage/events.rs:305–311`). The defect in the proposed Summary workaround is the later cleanup eligibility and contradictory “moving” label. Existing selector tests intentionally preserve this rule (`app/src/store/selectors.events.test.ts:99–107`).

**Smallest-model application:** durable Task finish eligibility should come from explicit settlement. Read state should describe observation; attention should describe whether the Owner is needed. If notification dismissal remains useful internally, its rule must not classify ordinary work as finished. Preserve done/reopen and explicit archive/snooze affordances; decide whether archive means dismissal or merely hiding, rather than silently changing that meaning during the Task cutover.

**Doctrine attribution:** T3 Code documents both the way out and the way back (`AGENTS.md:73`, thread guide `75–79`). Hirsel already has a stronger explicit-settlement rule (`docs/adr/0009-reply-resolves-pings.md:3–7`). The proposal enforces Hirsel's rule; importing T3 Code's time/PR settlement policy would contradict it.

## 4. Prove the Agent-to-inventory contract, including reconnect and native clients

The current tests can all pass while the product contract fails: tool tests verify emitted kinds and stored rows (`crates/hirsel-host/src/lash_runtime/tests.rs:1050–1106`); frontend tests explicitly remove every Info from Tasks (`app/src/store/selectors.events.test.ts:150–176`). Each local contract is internally coherent. The product promise spans the two.

Rust's shared client receives Events through both `HelloOk` and `EventUpsert` (`crates/hirsel-client-core/src/transport.rs:294–320`) and exposes raw `pings` in its snapshot (`crates/hirsel-client-core/src/store.rs:115–124`); the Task-membership policy currently lives in the web selector. A web-only selector edit therefore does not establish one shared product rule. This is a contract coverage gap, not a claim that an inspected native screen currently malfunctions.

**Concrete proof obligation for the redesign:** create ordinary work through the actual Agent tool executor; capture the resulting Event/Task and Host frame; require the same id and Anchor to appear in the web Task inventory from both a live upsert and a reconnect snapshot. Repeat after read, background attention changes, instrument recomposition, and explicit done/reopen. A housekeeping notification must remain outside that inventory. Share the same semantics/fixtures with Rust client-core. This tests observable membership and transitions, not a mirrored implementation or a nondeterministic full LLM run.

The prompt is part of the same cutover: `prompts/agent.md:12` incorrectly says a reply already resolves its event, whereas ADR-0009 says discussion is lifecycle-neutral; `:49–51` instructs @name addressing and compaction while authoritative Task refs are `#<id>` (`CONTEXT.md:22`). Rewrite creation, update, settlement, and stable-reference instructions together with the tool schema, so the Agent receives one usable model. Do not treat these as standalone prose cleanup.

**Doctrine attribution:** T3 Code's actual “Hit every surface” rules (`AGENTS.md:65–75`) and behavioral-test doctrine (`104–110`) expose this seam. Adopt their coverage discipline while keeping Hirsel's existing transports and global Agent.
