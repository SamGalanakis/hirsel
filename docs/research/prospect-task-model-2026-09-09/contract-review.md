# Independent contract review

2026-09-09. Read-only source review; no runtime/data changes or implementation. Exclusions read first. References below are repository-relative unless prefixed `/tmp/ref-t3code`, pinned at `6c583620ff7ad3235b135af7107c0543467eecfa`. Findings are deltas beyond the already-known hidden info event #5. No new test execution was needed to establish these direct data-flow facts.

## 1. Ordinary open work has an accidental creation path, not a safe contract

It is inaccurate to say the host can create only decision Tasks: `events_summary` creates a non-response Summary with a custom constrained UI (`crates/hirsel-host/src/tools/events.rs:133-158`), storage initializes every Event open (`crates/hirsel-host/src/storage/events.rs:44-93`), and web includes Summary in the Task inventory (`app/src/store/selectors.ts:119-135`). An ordinary “buy groceries” Task can therefore be encoded as a Summary without asking a fake question. That is an abuse of a digest category, and still unsafe:

- Clicking its chip marks it read (`app/src/components/tasks/TaskShell.tsx:330-333`). Read alone only changes the read flag in storage (`crates/hirsel-host/src/storage/events.rs:305-311`), so do not claim that merely opening a Task immediately completes it.
- Web calls an open, read, non-response Event finished (`app/src/store/selectors.ts:147-152`). The host independently includes that row in bulk clear (`crates/hirsel-host/src/storage/events.rs:213-230`). Archive then writes `status='done'` (`:391-405`). Thus **read -> Clear finished -> done** settles ordinary unfinished work without an explicit Task completion action.
- Clear's Undo only unarchives (`app/src/lib/event-sweep.ts:22-34`), and host unarchive returns Done (`crates/hirsel-host/src/storage/events.rs:200-209`); it does not recover the original open state.
- The frontend count/IDs also do not define the server operation: web excludes info and snoozed rows from its finished set, while host bulk clear selects all done or read/non-response rows without a snooze filter (`app/src/store/selectors.ts:119-152`; `crates/hirsel-host/src/storage/events.rs:219-222`). This is a concrete survivor beyond the known info filter: one sweep can clear rows outside its displayed batch and Undo scope.

Recomposition cannot correct the category: it updates only description/UI and preserves kind/response classification (`crates/hirsel-host/src/storage/events.rs:153-184`); storage enforces Judgment iff requires_response (`:469-470`). A Task initially cast as awareness therefore cannot become an owner decision while preserving identity through the existing recompose seam. Conversely, an initial Judgment continues to count as needing the Owner after a non-settling stage because counting reads immutable kind plus Open (`app/src/store/selectors.ts:157-164`).

## 2. Task mentions are an outgoing hint, not durable conversation attribution

The intended membership rule is sound and already settled: focus establishes the anchor and includes the focused Task ID; explicit mentions can additionally place a message in other Task margins (`app/src/components/tasks/task-model.ts:81-125,130-153`). The transport/storage implementation cannot fulfill it:

- Host validates/resolves mentions and gives them to `OwnerTurn`, but `append_owner_message` receives only body, anchor and attachments (`crates/hirsel-host/src/lib.rs:473-488`; `crates/hirsel-host/src/storage/chat.rs:79-85`). Mention context also enters the Agent's turn text (`crates/hirsel-host/src/lash_runtime/turn.rs:41-56`); the missing persistence is the client conversation's structured attribution, not all Agent knowledge of mentions.
- Persisted/broadcast `ChatMessage` has no mentions field (`crates/hirsel-proto/src/chat.rs:22-33`). The shared Rust confirmed message similarly lacks it (`crates/hirsel-client-core/src/store.rs:42-63`).
- Web `send_local` saves mentions on pending sends, not on the optimistic display message (`app/src/store/reducer.ts:553-582`); confirmed reconciliation replaces that display row with the host message (`:209-230`). Cross-Task attribution is therefore absent even during normal live display, not merely lost on reload.
- The margin test supplies an invented fully attributed display row (`app/src/components/tasks/task-shell.test.ts:79-97`), verifying a pure selector using data the actual path does not emit.

The mock-server contract test also expects mentions in live echo and reconnect replay (`app/src/mock-server.contract.test.ts:152-177`). That is useful intended-contract coverage, but the Rust Host's `ChatMessage` does not satisfy it. Receive reduction and margin selection do not reconstruct structured mentions from body text (`app/src/store/reducer.ts:262-320,355-389`; `app/src/components/tasks/task-model.ts:95-117`); body parsing resolves outgoing mentions in the Composer (`app/src/components/chat/Composer.tsx:129`).

Preserve one global conversation. Persist Task mentions as message-to-Task references alongside the existing reply anchor, expose them in `msg`, history and `hello_ok`, and pass them through both clients. A task table rename alone cannot repair this gap. Anchor chains still work for single-task context; the failure is explicit multi-task/global citation membership, not universal conversation loss.

## 3. Snapshot/live storage is broader than web; native projection erases the distinction

The host already snapshots all open and done Events, including archived rows (`crates/hirsel-host/src/storage/events.rs:412-448`), inside the same SQLite transaction as message replay (`crates/hirsel-host/src/storage/chat.rs:172-190`). Creation persists before broadcasting EventUpsert (`crates/hirsel-host/src/tools/events.rs:172-191`). Rust core consumes hello and upserts without filtering (`crates/hirsel-client-core/src/transport.rs:294-320`; `store.rs:223-255`). These parts should be retained, with Task rows becoming the shared object.

The loss occurs at the native boundary: FFI Ping removes kind, archived/snoozed state, source and structured UI, extracting only direct child text (`crates/hirsel-client-ffi/src/lib.rs:204-240`). Android renders every snapshot ping, including archived/snoozed ones, through a legacy card (`android/app/src/main/kotlin/dev/hirsel/android/chat/ChatScreen.kt:205-217`). Its read state is local only (`:608-627`). Quick replies invoke `connection.send(text)` (`:217,689-692`); FFI accepts only a body (`crates/hirsel-client-ffi/src/lib.rs:434-438`), so these sends lack the Task anchor, mentions and structured action.

This is not a proposal to reinvent native Task Margins: ADR-0010 already requires that, and explicitly accepts native feature lag (`docs/adr/0010-native-mobile-on-rust-core.md:3-5`). The new requirement is a **cross-client data/command contract gate** for the model change: a frontend cannot project fields its core/FFI has already discarded. Pure web selector coverage does not establish supported-client correctness.

## Minimal strong shape

Suggested conceptual API, not a committed design or implementation:

```text
Task { id, anchor, title, description, instrument,
       settlement: Open | Done,
       attention: None | NeedsOwner { blocking },
       read, snoozed_until, archived_at }
TaskActivity { task_id?: TaskId, kind: Info | Summary | Judgment, content, source }
Message { id, body, ref?: MessageId, mentions: TaskId[] }

tasks.create(title, anchor, instrument?) -> Task(Open, attention=None)
tasks.update(id, description?, instrument?, attention?) -> same Task identity
tasks.act(id, action, data) -> host validates declared settlement intent
tasks.complete(id), tasks.reopen(id)
tasks.read(id), tasks.snooze(id, until), tasks.archive(id)
hello_ok.tasks / task_upsert carry the identical complete Task shape
```

In this proposal, create/update are Agent tools with Host-resolved Anchor; act/complete/reopen/read/snooze/archive denote Owner-facing Host commands, not blanket new Agent tools. Desired authority is subject to an explicit design decision, including reconciliation of historical Agent resolve/archive paths; this sketch does not claim the current implementation already enforces that division.

Activity can use the existing conversation/output channel or a small associated record; this does not require a separate notification destination or an event-sourcing framework. The key is that emitting information does not itself allocate work, and emitting information about existing work references its stable ID. Attention can change within that ID, independently from settlement. No task spec, retry policy, process ownership state machine, or per-task Agent is implied.

Recommended archive rule: bulk clear archives only explicitly Done Tasks; `read` never makes a Task clearable. For a single open Task, either reject archive or make the command explicitly “complete and archive.” Do not silently introduce T3's independent archive semantics: Hirsel currently normalizes Archived to Done. Retaining this implication minimizes the behavioral change while removing the awareness shortcut. If independent open/archive is wanted, record that separately as a product decision.

## T3 mechanisms worth borrowing; limits

T3 materializes identity on `thread.created` before messages or turns exist: the projector builds a thread with `latestTurn:null`, `messages:[]`, and archive/settlement fields separately (`/tmp/ref-t3code/apps/server/src/orchestration/projector.ts:320-351`); a concrete test asserts that initial shape (`projector.test.ts:43-101`). Borrow the **creation produces an inventory object before execution or attention** invariant, not the Thread-as-session model. T3 visit/read preferences are a distinct UI record (`/tmp/ref-t3code/apps/web/src/uiStateStore.ts:22-47,426-445`), and archive updates archive metadata (`/tmp/ref-t3code/apps/server/src/orchestration/projector.ts:379-398`). Borrow the separation of questions; do not copy client-local-only read persistence into a multi-client Hirsel contract.

Do not copy per-thread chats/providers/worktrees, turn-derived completion, T3's multi-field settled override scheme, Effect/event-sourced machinery, or its archive policy without an explicit decision. Those either exceed this gap or conflict with Hirsel's global agent and explicit Open/Done semantics.

## ADR implications

- ADR-0004's July clarification excludes host workflow machinery, not durable visible work. A product Task record is compatible with that clarification, but revise the older absolute “no Task entity/table” wording to avoid leaving a direct textual contradiction (`docs/adr/0004-no-task-abstraction.md:3-5`).
- ADR-0012's current “Typed Events remain the wire/storage basis of Tasks” must change if identity becomes an explicit Task record. Its inherited read/dismiss awareness lifecycle is exactly what must stop controlling Task settlement (`docs/adr/0012-typed-event-queue-and-scroller-home.md:3,30-38`).
- ADR-0009 and ADR-0013 remain authoritative: explicit completion and same-ID continuing stages are retained (`docs/adr/0009-reply-resolves-pings.md:3-7`; `docs/adr/0013-constrained-json-ui-substrate.md:3`). Make attention mutable without treating it as settlement. Native work fulfills ADR-0010, not a new mobile architecture.

## Focused acceptance tests

1. Create ordinary work through the actual Agent tool with no question/options/response requirement. Assert the returned ID exists exactly once after persisted storage reopen, live TaskUpsert reduction, fresh hello reduction, Rust-core/FFI projection and web/native inventory selection.
2. Read that Task and exchange global/task conversation. Run Clear finished. It remains Open and present; a separately explicitly completed Task is archived. Assert the server result IDs equal the displayed batch; Undo restores the exact previous state. Include read info activity and snoozed open work to catch scope drift.
3. Move the same Task from attention None -> NeedsOwner -> None using two generated stages. ID/anchor/margin stay fixed; needs-you count follows attention; a continuing action stays Open; explicit completion makes Done; reopen preserves identity/instrument.
4. From Task A cite Task B and send once through the real host boundary. Both live clients and fresh replay contain durable `[A,B]` mentions and the same margin membership; an ambient message with no mentions belongs to neither. Do not hand-build attributed display messages as the only coverage.
5. Native choose/continue actions must preserve ID/anchor and use the same host action validator as web; free text is lifecycle-neutral. Verify persisted read and archive/snooze filtering in the FFI output contract.

Retain existing coverage for same-ID generated recomposition/settle/reopen (`crates/hirsel-host/src/tests.rs:450-526`), idempotent storage lifecycle operations (`crates/hirsel-host/src/storage/events/tests.rs:69-176`) and hello/live dedupe. Extend across missing seams rather than replacing adequate tests; old awareness-specific expectations need deliberate revision with the ADR change.
