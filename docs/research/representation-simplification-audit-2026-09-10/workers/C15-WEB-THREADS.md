# C15-WEB-THREADS combined schemasmash and codebase audit

## Scope and snapshot

I inspected all 46 exact owners in `C15-WEB-THREADS.md`, plus the specified
protocol, reducer/store, host conversion, and test consumers. The review was
read-only and bounded to local source inspection; no tests, builds, installs,
network calls, live reads, commits, or source edits were performed.

The initial snapshot matched the dispatch: `git rev-parse HEAD` returned
`3ee0621a603659ab0168f565b99012b642415419`, `git rev-parse HEAD^{tree}`
returned `a4aac830c45398a66591f2c44b707aaf3cef281b`, and
`git status --porcelain` was empty.

## Finding F1 — connected Thread action errors have no destination correlation

**Verdict: confirmed, reachable, high confidence.** A connected action selected
from a navigation row can fail, but its error is stored as a global request
failure. The failure is therefore rendered in the focused conversation and in
the navigation drawer even when neither is the Thread that the user acted on.

### Evidence and concrete state

The browser action frame has no request identity:

* `app/src/threads/types.ts:68-74` defines
  `thread_action` as `{ thread_id, action, data, expected_revision? }` while
  every other request-like frame carries `client_id`.
* `app/src/threads/store.ts:118-120` sends that frame directly and keeps no
  action-pending map.
* `app/src/threads/ThreadNavigation.tsx:117` renders `ThreadActions` for every
  drawer row, while `app/src/threads/ThreadShell.tsx:114` renders the current
  conversation's `ThreadError`.

The host cannot recover the destination after the action is decoded:

* `crates/hirsel-proto/src/client.rs:95-102` has no `client_id` field on
  `ClientToHost::ThreadAction`.
* `crates/hirsel-host/src/protocol.rs:155-174` extracts an optional generic
  `client_id` and echoes it only when the frame had one.
* `crates/hirsel-host/src/protocol.rs:456-465` dispatches the action with no
  acknowledgement or correlation result.
* `crates/hirsel-proto/src/host.rs:123-129` makes `Error.client_id` optional,
  with comments limiting its intended correlation to uploads and queue
  cancellation.

On a connected stale `set_icon`, invalid action, or unknown Thread, the
reachable state is therefore `threadState.error = { operation: "request",
detail }` with no `threadId` (`app/src/threads/store.ts:263-271`). The matching
consumer explicitly treats an absent `threadId` as applicable to every focused
Thread (`app/src/threads/ThreadError.tsx:3-10`), and navigation displays every
`request` failure (`app/src/threads/ThreadNavigation.tsx:102`). A concrete
scenario is focused Thread A, action-menu row Thread B, then B's rejected action:
the B error appears under A and in the drawer, while the UI cannot identify or
retry B's operation.

The disconnected branch already attaches `threadId` (`store.ts:118-120`), and
`app/src/threads/ThreadShell.test.tsx:444-457` only exercises that local branch.
`app/src/threads/store.test.ts:67-73` asserts the uncorrelated outgoing frame;
`ThreadIconPicker.test.tsx:64-73` supplies an uncorrelated error in a one-Thread
fixture, so it cannot detect cross-Thread misattribution.

### Consumer query

The reproducible query below returned 34 matches across the web type/store/UI,
shared Rust frames, host dispatcher, and protocol documentation:

```text
rg -n 'thread_action|ThreadAction|operation: "request"|ThreadError|HostToClient::Error|ClientToHost::ThreadAction' \
  app/src/threads/types.ts app/src/threads/store.ts app/src/threads/ThreadError.tsx \
  app/src/threads/ThreadNavigation.tsx app/src/threads/ThreadShell.tsx \
  app/src/threads/ThreadActions.tsx app/src/threads/actions.ts app/src/protocol.ts \
  crates/hirsel-proto/src/client.rs crates/hirsel-proto/src/host.rs \
  crates/hirsel-host/src/protocol.rs app/PROTOCOL.md
```

### Target representation and smallest credible change

Use one transient request identity across the action path:

* Web `ThreadClientMessage` becomes
  `{ type: "thread_action"; client_id: string; thread_id: number; action:
  string; data: unknown; expected_revision?: number }`. `threadAction` creates
  a UUID and stores `PendingThreadAction { clientId, threadId, action }` in a
  map keyed by that UUID. `ThreadFailure` gains `operation: "action"`, required
  `threadId` and `clientId` for this variant. Errors remove the exact pending
  action and render only for that Thread; timeouts/reconnect clear or replay
  the exact pending entry according to the chosen action policy.
* `hirsel-proto::ClientToHost::ThreadAction` carries required `client_id: String`.
  Add a targeted success frame
  `HostToClient::ThreadActionResult { client_id: String, thread_id: u64 }` so a
  pending action is settled without treating a broadcast `ThreadUpsert` as an
  acknowledgement. `HostToClient::Error` echoes the same id on action failure;
  the existing `ThreadUpsert` remains the shared state update.
* The host dispatcher passes the ID through `handle_thread_action`, sends the
  result after the returned Thread is published, and leaves no durable action
  table: action identity is transient; the durable Thread row remains the sole
  state owner. Update `app/PROTOCOL.md` and the web/Rust protocol mirrors.

This removes the representable combination “action B failed, global failure
with no destination” and prevents an unrelated current Thread from consuming
the error. There is no duplicate-truth write path applicable to F1: the defect
is missing request ownership, and `ThreadUpsert` is a state broadcast rather
than a second durable action record.

### Risk and validation

The cutover is current-protocol-only, consistent with the repository's schema4
decision. Native FFI/generated consumers and any protocol fixtures must be
regenerated or updated together; old uncorrelated action frames should be
rejected or treated as an explicitly global protocol error. Required inspection
validation is: two-Thread web test dispatching a connected drawer action on B
while A is focused; action failure with echoed B client ID; unrelated global
error remains global; concurrent B actions resolve independently; success result
clears only its pending action; reconnect/late error cannot attach to a reused
ID. No validation command was run by instruction.

The issue is independent of tracked #2: that exclusion concerns native
subagent command acknowledgements, whereas this path is the web `thread_action`
row action and host error routing.

## Duplicate and excluded mechanisms

The tool-summary identity loss is real but is **not reported as a new finding**.
`app/src/protocol.ts:18-23`, `app/src/threads/work-summary.ts:9-50`, and
`app/src/threads/ThreadWork.tsx:41-46` reduce persisted tool calls to
`name/ok` and match a running duplicate by name. The host has stable event IDs
at `crates/hirsel-host/src/lash_runtime/timeline.rs:120-158,180-190` but drops
them in summaries at `:287-320`; the standard lane writes both
`tool_completed` activities and `chat_messages.tool_calls`
(`thread_queue.rs:254-280`, `storage/thread_completion.rs:97-107`). The
reconnect-with-a-missing-start/same-name mispairing is covered by the root's
independent regression under tracked #13, so this review does not duplicate it.
The consumer query
`rg -n 'tool_calls|tool_completed|toolSummary|remainingTools|resolveStartedTools'
app/src/components/chat app/src/threads app/src/protocol.ts
crates/hirsel-host/src/lash_runtime/thread_queue.rs
crates/hirsel-host/src/lash_runtime/timeline.rs
crates/hirsel-host/src/storage/thread_completion.rs
crates/hirsel-host/src/thread_tool_bridge/telemetry.rs`
returned 25 matches.

Other explicit no-finding areas after inspecting every owner:

* Composer, paste/drop, text input, and attachment state use a closed upload
  union, one staged-file owner, per-history/per-Thread draft keys, and bounded
  file/paste admission; existing focused tests cover the transitions.
* Thread refs keep composed text as the sole mention truth, validate against
  the current history, and have picker/keymap tests. Timeline folding joins
  start/done events by event ID and preserves ordering; the known persisted
  tool-summary issue is the tracked #13 item above.
* Conversation/model/store joins filter exact Thread/turn/message IDs and
  preserve live rows across detail loads. Stale terminal-vs-running merge
  behavior is also tracked under #13; no independent C15 defect was found.
* Navigation/tree/status/shell/actions/icons preserve independent lifecycle
  dimensions and bounded ancestry/orphan traversal. Top-level pin projection
  is the tracked #12 outcome; no new pin finding is reported.
* Fixtures and the listed composer, timeline, conversation, model, selection,
  status, store, tree, action, icon, shell, and work tests were inspected for
  coverage. No tests were executed.

## Post-report verification

After writing this report, the source worktree still matched
`HEAD=3ee0621a603659ab0168f565b99012b642415419` and
`HEAD^{tree}=a4aac830c45398a66591f2c44b707aaf3cef281b`; `git status --porcelain`
was empty. Only this report under `/tmp/hirsel-combined-audit/workers/` was
written.
