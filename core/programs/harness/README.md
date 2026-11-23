# harness

A Hirsel LLM harness. One Place holds one session: a model, a narrowed tool
set and an append-only list of entries. A turn is rows and short `store.Tx`
transitions, and no code runs between effects, so a place that stops or dies
at any point reopens and carries on from its rows.

The gateway can fork a session at a transcript cut. A subagent is an Outside
tool that calls another session's place and waits for its answer.

## Quickstart

From the repository root with Node.js 24 or later, build the toolchain and
point `HIRSEL` to the executable inside the printed output directory.

```sh
python3 tools/buck2/prepare.py
toolchain=$(.buck2/bin/buck2 build -j4 //:hirsel-toolchain --show-output | awk '$1 == "root//:hirsel-toolchain" { print $2 }')
export HIRSEL="$PWD/$toolchain/hirsel"
```

Run the offline check first. The script builds a debug gateway with
`--limit 536870912` and starts it on loopback. The TypeScript client sends
a chat, forks it, replays a recorded call for the cache diff, and settles
a release proposal.

```sh
core/programs/harness/serve.sh --scripted
```

Expect `Chat READY`, `Fork 1 FORKED`, a cache diff whose first changed section
is `release-preview`, and `Release approved, not deployed`. The diff compares
the current notes agent with the example candidate on the same recorded fork
call's base. The development gateway has no launcher. Selecting Publish
settles the proposal and returns `approved, not deployed` to the tool.

Set `OPENROUTER_API_KEY` in `~/.config/hirsel/openrouter-dev.env`. The script
sources that file and passes the key to the gateway's Secret grant. Then run
the live chat and fork against OpenRouter.

```sh
core/programs/harness/serve.sh --live
```

The chat asks the model to save one note. The fork asks it to reply with
`FORKED`. The client prints both answers, the same cache diff, and the
provider's request and prompt-token counts. Append a quoted prompt to
change the chat. Set `OPENROUTER_MODEL` before the command to
choose a model listed by OpenRouter.

If the model ID is refused, check it at `https://openrouter.ai/models`.
For provider HTTP errors, the client prints the status and the provider's
message when available. If the gateway is unreachable, start it with
`serve.sh --gateway` and use its loopback port.

For a gateway you can call from your own Node.js client, run
`core/programs/harness/serve.sh --gateway` and use
`http://127.0.0.1:<port>` from its `port` line. Closing stdin stops it.
Create a session with a bearer credential and use the same credential to
send a message, fork or decide a release. The agent can call
`write_source` to prepare a file and `propose_release` to file it. Read the
filed ID with `client.releases()`, then call `client.settle(id, "Publish")`
or `client.settle(id, { Reject: "your reason" })`. At the gateway's stdin,
`diff SESSION_ID` compares the example candidate after a chat on the fork.

## What a harness author writes

An agent is a plain value built by `harness.agent(name, route, model)`. Tools
are added with `harness.outside` or `harness.inside`, and limits are set on
`agent.limits`. The agent is never stored: each entry below builds it again,
so a new release changes the agent without a migration.

```
fn my_agent() -> harness.Agent[Grants] {
    let mut a = harness.agent("notes", "scripted", code.describe(model))
    list.push(mut a.tools, harness.outside("bump", "adds one", "{}", Once, code.describe(bump)))
    a
}
fn wired() -> Result[harness.Wiring, harness.Refused] { harness.wiring(code.describe(advance), code.describe(effect)) }
```

`../notes` is a complete program built on the harness: every part an app
composes, wired into one agent, built with `hirsel build` and run. `laws.hirsel`
holds the places the laws run.

## The four forwarding entries

A place registers four entries. Each one forwards to the harness with the
agent and the wiring:

| entry | kind | forwards to |
| --- | --- | --- |
| `advance(tx, h, caller, wake)` | transact trigger target | `harness.advance` |
| `effect(grants, caller, key)` | granted trigger target | `harness.effect` |
| `begin` | fenced attempt entry | registered by `harness.register` |
| `settle(tx, attempt, m)` | fenced attempt entry | `harness.settle` |

`harness.migrate` and `harness.handles` are the place's `migrate` and
`handles`. `harness.entries(wiring)` is its `TriggerConfig.entries`.
`harness.open_session` and `harness.submit` run in any write transaction;
`harness.window` reads the transcript under the session ACL when registered with `query_as`.

## How a turn runs

- `submit` appends the user entry, creates the run and calls `advance`. A
  repeated client key with the same bytes returns its first disposition; a
  submission during a run is answered `Busy` and leaves no row.
- `advance` reads the session row and the active run. To prepare a model
  call it seals an effect row with only a cutoff, `(head_pos, tail_pos,
  route, policy)`, and writes the effect trigger and a deadline trigger. No
  request body is stored or passed as a trigger argument.
- `effect` is the granted worker. It calls `begin`, which records the
  dispatch durably. It then reads `entries[head_pos..=tail_pos]` through
  `runner.trigger_snapshot`, makes the one outside call, and calls `settle`.
- `settle` is first-writer-wins. A model answer commits as an `Assistant`
  entry (model.answer) before any tool is derived from it. Derivation runs in
  the next `advance`.
- Each tool call becomes one member row. An advance runs at most one Inside
  tool, under `bound bytes`, and schedules itself again when more remain.
  Outside members are sealed like a model call. When every member is
  settled, the results are appended in model order and the next request is
  prepared.

## MCP tools

Use `harness.mcp_tools.discover` outside the transaction to list a configured
stateless MCP server. Publish the complete listing with
`harness.mcp_tools.replace` against `h.mcp_catalog` in a trusted write
transaction. Each refresh replaces only that server's rows. Use a new server
ID when changing the endpoint or account behind it.

Set `agent.mcp_peer` to a static resolver that maps catalog server IDs to
the endpoints and grants available to the worker. Keep its server IDs scoped
to the agent. The next request offers catalog tools under their stable
`harness.mcp_tools.offered_name(server, name)` names, subject to the session's
tool narrowing. The `mcp__` prefix is reserved for those names.

A schema or description change produces a tools-only System patch when the
sections have stayed the same. Approvals cover the full catalog fingerprint
and the arguments. A stale approval conflicts. The worker checks the catalog
against the pinned request before using the one static MCP call body. Calls
use the Once policy and return text content.

## Large Outside results

Outside tool results up to 64 KiB go in the journal. Above that size the
worker calls the agent's `store_result` function before it settles. The
assembly gives that function access to its `std.object.Client` and uses
`harness.store_payload` to write the body with a conditional PUT at
`sha256/<hex digest>` under the client's bucket prefix. Only the reference
is settled and appended. A write failure is a tool failure.

`harness.read_blob` takes a caller-supplied byte budget and verifies the
length and SHA-256 before returning bytes. Use the same bucket and prefix for
as long as any journal entry can refer to them. If a Once worker dies after
the object write, the object can remain unreferenced and the effect becomes
InDoubt on recovery, just as it would after any interrupted dispatch.

An assembly can also stage the result of a parked Inside tool before calling
`harness.resume`. An Inside tool's direct `Now` result has a 64 KiB limit.

## System sections and reactions

`agent.sections` is a list of `Section{key, render}`. List order is prompt
order. Each time a request is prepared, every section renders from rows under
`bound bytes limits.section_bytes` and `bound steps limits.section_steps`.
A render that returns `None` removes its section. One that fails or overruns
keeps the text it showed last.

`prepare` then appends the smallest `System(Patch)` entry that brings the log
to what was rendered, or none if nothing changed:

- A change for a shown key replaces its text in place. A new key is added at
  the end, and `None` removes a key.
- If the rendered order differs from that, the patch is a baseline holding
  every section in order. So is the first request. A baseline that would
  need a kept section's text is put off to the next request, and the prompt
  stays as it was.
- The offered tool definitions (name, about, input, rev, after narrowing) are
  in every baseline. Otherwise they are included only when they change.

The session row caches each shown section's hash and the tools' hash, so
planning reads no earlier entry. The worker builds `Request.system` and
`Request.tools` by folding the System entries up to the cutoff
(`harness.request`). A resend is therefore built from the log, never from
the agent's current code. Notes are not sent.

`agent.reactions` is a list of `After{name, run}`. Every reaction runs in the
transaction that appends an entry, with the entry and its position, under
`bound bytes limits.inside_bytes` and `limits.inside_steps`. It may write its own rows and triggers. It
cannot veto or edit the entry: a failing reaction has its writes rolled back,
and a `Note("reaction failed", name)` is appended. Reactions do not run for
that note. Appending entries from a reaction is not built yet.

## Repair, gates and approvals

Each derived call passes, in order:

1. The limits. A call past `limits.calls` fails, and a tool that is narrowed
   out or unknown is `Gone`.
2. The tool's `repair`, if it has one. It is a pure function from argument
   bytes to argument bytes, run under `bound bytes limits.gate_bytes` and
   `bound steps limits.gate_steps`. A fault or an overrun fails the call.
   Everything after this step sees the repaired arguments, and the member
   row stores them.
3. The agent's `gates`. A gate is `fn(store.Tx, Ask) -> Verdict`: it gets a
   read-only transaction and `Ask{caller, call}`, where `caller` is the
   submitter of the run, never the trigger's caller. Each returns `Pass`,
   `Hold(reason)` or `Deny(reason)` under the same bounds. The verdict is the greatest one
   under `Pass < Hold < Deny`, so adding a gate can only make a call
   stricter, and gate order never matters. A gate that overruns denies.

A held call waits under the approval key
`sha256(tool name, tool rev, repaired arguments)`. `harness.decide` approves
or refuses it. Only the run's caller may decide, and only with the key the
call is held under. A key for other arguments or another revision is refused
`Conflict` and changes nothing, so an approval can never carry over to a
call it was not shown. The first decision wins: repeating it returns its
`Ruling`, and the opposite decision conflicts. A hold expires
`limits.hold_ms` after it is written. A timer then denies the call with
"approval expired". A decision at or after expiry is `Lapsed` even if that
timer has not run.

An Inside tool is `fn(mut store.Tx, Invoke{key, args}) -> Result[Reply, Fault]`.
`Reply` is `Now(bytes)` or `Later`. With `Later` the tool has written
whatever row will wake it (a timer, a webhook row, another place's reply),
and its call stays `Parked`. The app's entry for that wake calls
`harness.resume(key, outcome)` after authorizing its caller. Only the first
resume of a parked call in the active run counts. The call expires
`limits.tool_ms` after it parks. A resume at or after that deadline yields
`Ignored` and the call settles as `Failed("tool deadline")`.

`harness.cancel(caller, run)` revokes the caller's open approvals and parked
calls, cancels outstanding effect triggers and releases the session. A
dispatched effect without a receipt is in doubt. Cancelling a terminal run
returns its recorded outcome.

An Inside tool runs inside `advance`'s transaction under
`bound bytes limits.inside_bytes` and a step bound of `limits.inside_steps`
(`store.rollback_writes_on_overrun`). If it returns an error or overruns
either bound, its staged writes are rolled back and the failure is
committed. Its reads still count in the transaction's read set.

## Effects after a crash

- **Once.** If an attempt began and never settled, the next attempt is told
  `Doubt` by `begin` and records `InDoubt`. It never sends again, and it
  does not wait for the deadline.
- **Repeatable(n).** The effect resends with the same key, up to n
  dispatches, before its deadline. The earlier dispatch's reservation is
  counted as unresolved spend.
- **Deadline.** If an effect is not settled by its deadline, its trigger is
  cancelled. A dispatched effect is recorded as in doubt; an undispatched one
  is recorded as failed.
- **Spend.** Spend is kept as `known` (settled usage), `reserved` (calls in
  flight) and `unresolved` (calls that may have been charged).

## Live preview on the gateway

Start a `harness.preview` hub and pass its door to the HTTP front and the
worker grants. In the worker, pass `harness.effect` a target for the
session's place name. The model then gets a draft source, which a streaming
model can pass to `provider.ask_with_preview`. Outside tools publish
a running status keyed by their call ordinal. They never publish arguments
or results.

On a preview-enabled front, a browser connects to `/sessions/{id}/events`
with its bearer credential. The place binds it to a Caller. Only the run's
Caller receives its draft. When an application revokes read access, it can
call `harness.preview.revoke` to clear that Caller's existing watchers.

A named `preview` event replaces the draft with a bounded snapshot: its
`offset` is where `text` sits in the attempt's draft. The key contains the
store identity, run, transcript entry, dispatch and attempt. The front sends a
snapshot only when a stream joins, after a lost delta, when the hub dropped
text the browser had not received, or for a new key. Otherwise it sends
`preview-delta` events (`seq`, `at`, `text`) that append at the draft's end,
coalesced up to 2 KiB or 100 ms. A delta whose `at` is not the held end means
the reader lost its place: reconnect. A `preview-clear` event removes the draft. A committed row at
`key.entry + 1` supersedes the draft. Reconnecting starts from the current
snapshot, if any. Connect without a bearer credential for transcript rows
alone.

The hub retains at most the configured text bytes for each observed
dispatch, and each SSE connection pulls one snapshot at a time. Deltas can
be dropped under load. A gap starts a fresh draft; a full draft keeps its
newest bytes and moves its offset.
Dropping the last observer clears the draft. Preview adds no journal writes,
and the front and workers can be configured without it.

## Provider wire formats

`provider.hirsel` turns a `harness.Request` into the request body of the
Anthropic Messages API or the OpenAI Chat Completions API, and reads the
streamed reply back into a `harness.Answer`. It is pure: the transport passes
each response chunk to `provider.feed` and asks `provider.answer` at the end.

- **An answer needs its terminator.** `answer` succeeds only after Anthropic's
  `message_stop` event or OpenAI's `data: [DONE]`, and only with a stop
  reason. A stream that ends sooner, or carries an error event, has no answer.
  The request was already sent by then, so the worker records doubt and does
  not seal a partial reply.
- **Deltas stay in memory.** Text and tool-argument pieces build up in a
  `Stream` held by the worker, under `Bounds` on event bytes, body bytes, text
  bytes, call count and argument bytes. Only the finished answer is stored.
- **Usage.** Anthropic reports input tokens in `message_start` and cumulative
  output tokens in `message_delta`, so the last value wins. The OpenAI body
  asks for `stream_options.include_usage`, and the usage arrives in a final
  chunk with no choices.
- **Bodies.** Anthropic gets the system text at top level and every tool result
  of one assistant turn as `tool_result` blocks in one user message, with
  `is_error` set for failures. OpenAI gets a system message, assistant
  arguments as a JSON string, and one `tool` message per result. `worded` is
  the one place a tool outcome becomes text the model reads.
- **Checked fragments.** Tool input schemas and call arguments are raw JSON
  embedded in the body, so `body` checks each one before writing.

- **Transport.** `provider.ask(net, clock, provider, request)` is the
  agent's model function body. It connects with `open` or `open_tls`; a
  failure there is `Refused`, since nothing was sent. Once
  `http_client.request_started`, every failure (a cut stream, a close with no
  reply, a malformed or over-bound stream) is `Unavailable`, which the worker
  records as doubt with spend unresolved. A non-200 reply is a definite
  rejection, `Refused`. The key is a `Secret` sent only as a protected header
  (`x-api-key`, or `Authorization: Bearer`), and secrets travel only over a
  verified TLS stream, so plain HTTP endpoints cannot carry one.

## The gateway

`harness.gateway` runs each session as a keyed session of an in-process platform
host (`platform.local`, RUNNER-SPEC §14.9), which keeps the catalog in its own store
and commits a session's rows with its group, so the two never disagree. A place
unloads only by its own idle stop (§14.4), which never cuts active work: an idle
session holds no memory. An app gives it the factory that builds a session's
place (`platform.local.seeded_factory`, whose `make` sees the gateway's seed, the
services every session's grants are made from), its migration, its store
options and the idle time.

- `create(door, key, Fresh(model, narrow) | Adopt(prefix))` seeds a new
  session's store, then adds its group and its catalog row; a repeated key
  returns the same id, and an id is never reused. `Adopt` takes a fork's
  exported prefix.
- `route(door, id)` wakes the session, starting its place if it is unloaded,
  and returns a lease whose status holds its address. The lease does not hold
  the place; a client's session does. Wire calls go to the place itself. A
  binding is needed for a caller that reconnects, and its request positions
  continue across connections.
- `delete` unlists the session and removes its group, and answers once its
  place has drained and its store is deleted.
  `listing` and `stop` do what they say.
- **Wake.** The host keeps the wake row (§14.1). Only a clean stop writes
  Clean; a trigger due later wakes the session then. Any other end leaves
  Dirty and starts it again, and a restarted gateway's host starts every Dirty
  or due session.
- **Status.** A session's `Status` view reads "calling tools" while a member
  of its round can still move by itself, and "waiting" when every open call
  waits on an approval or a resume, so a client holding the session can let
  go and the place can stop.
- `unload(door, id)` closes a session's place now, as an operator would. The
  host answers when the stop ends and keeps serving meanwhile. A
  cut run (`Forced`, `Unstarted`) is woken again at once. A place that cannot
  start stays Dirty until the next route or gateway start; there is no backoff.
- **Subagent.** `ask(door, key, seeding, child, text)` is an Outside tool body.
  It starts the child session named by `key` (`child_key(place name, trigger
  context)`, so every attempt of one effect reaches one child), submits `text`
  under a credential derived from the key, and returns the child's last
  assistant line once its status is "answered". It holds no lease or
  connection while it waits: `await_down` wakes it when the child's place
  stops. A look wakes the child, so after a look that finds the child as the
  last one did, `ask` waits for a stop that someone else's wake caused. The
  gateway's seed holds the door (`doorway()` makes the inbox before the
  gateway). The effect entry and `grant_bytes` must hold the wire client's two
  1 MiB tasks. An effect that runs out of bytes raises, its run stays
  unsettled, and the place idles out.

## The HTTP front

`harness.gateway_http.serve(door, front, listener)` serves a gateway over HTTP/1.1,
one request per connection, and holds each browser's event stream itself, so a
session's place can unload while its page stays open.

- Every route requires `Authorization: Bearer <base64 credential>`. A missing or
  malformed credential returns 401. `GET /sessions` lists the caller's visible
  sessions. `DELETE /sessions/:id` requires membership in that session.
  `POST /sessions` takes a creation key, seeds the new session with the caller as
  its first reader, and returns the id.
- `POST /sessions/:id/messages` (a JSON `Submit`, answers 202) and
  `POST /sessions/:id/decisions` (a JSON `Decision`, answers the `Ruling`)
  route to the place, waking it, and call its entry. The browser is the
  Caller: it sends `Authorization: Bearer <base64 credential>` and
  `Hirsel-Position: <p>`, its next request position by WIRE's client rules,
  so a retried request at one position runs once. An entry's refusal is 422,
  a stranger 403, a position bound to other bytes 409.
- `GET /sessions/:id/events` is a server-sent event stream of the session's
  transcript rows (`Shown { at, line }`). A `batch` event with the store
  version follows each image's rows; apply the rows before it together, and
  treat the first batch on a stream as the complete image. The front follows the place only
  while it is busy, then lets go; it learns of a later change from the
  gateway's start counts (`marks`), and reads the new rows with `peek`, which
  never starts a place. Opening the stream wakes the session once. A comment
  every `beat_ms` keeps the connection open. The stream starts at the last
  `?tail=N` lines (default and maximum `Front.tail_lines`) and keeps that lower
  bound while it lasts, so later rows only append.
- `GET /sessions/:id/history?before=B&lines=N` (bearer credential, `N` at most
  256) answers one JSON `Page { from, rows }`: the rows at positions
  `from <= at < B`. The page stops at the newest fork cut in the requested
  range. Passing `from` as the next `before` walks the fork's pinned prefix
  and any earlier cuts until `from` is 1. Appends never move a page's upper limit.
- `GET /sessions/:id/waits` lists the active run caller's open approvals. Use the
  returned `run`, `round`, `ordinal` and `key` in a `Decision`. `expires` is the
  wall-clock deadline in milliseconds.
- `POST /sessions/:id/forks` takes `{key, cut}` and returns the new session ID.
  It shares entries before the cut. Repeating the key returns the same ID.
- A development front enables `GET /sessions/:id/releases` to list filed
  proposals and `POST /sessions/:id/releases` to settle one. The POST takes
  `{id, decision}` with `"Publish"` or `{Reject: reason}`, and uses the same
  `Hirsel-Position` rules as a message. Both routes require the session's reader.

A session ACL denial returns 403 before the front sends any event stream or
history headers. Denied requests leave a cold place asleep.
- The front uses `runner.ingress.caller` for bearer credentials. Places register
  `harness.window` and `harness.history_cuts` with `runner.query_as`.
  Each query reads the Caller's conversation ACL row, including when preview is
  disabled. A denied page is empty and a denied stream ends. An authorized
  stream still needs the Caller to own the run to see its preview. Trusted
  application code updates membership with `harness.set_reader` and nudges the
  gateway after the commit so quiet streams check again.
- While a session is busy, its following stream holds one of the place's wire
  sessions. The gateway's host sets every session's place to four (RUNNER-SPEC
  §14.9), so a page, a status read, a message or a decision never waits for the run.
  A front call whose connect is refused retries until `within_ms`, which covers a
  finished session the place has not yet reaped.
- `ts-client/` is a TypeScript browser client for this front, with an example page
  and a test against a running gateway.
- `std.http` cannot hold a gateway door in a handler or stream producer
  (HTTP-10, HTTP-11), so the front runs its own accept loop. WebSocket is not
  served.

## Laws

| law | claim | oracle |
| --- | --- | --- |
| C1 `test_once_tool_crash` | A place stopped while a Once tool is in flight reopens without resending the tool. The tool result is InDoubt at once, the model answer is not bought again, and the run ends Answered. | The scripted services' own call counts, which outlive the place, plus the transcript and spend derived by hand from the script. |
| M1 `test_crash_matrix` | Inject EIO at each filesystem operation of two runs and reopen the crash image. The second run includes a gate and approval, a reaction and prompt patch, and an Inside tool that parks and resumes. The runner exits after poison. Each Once effect dispatches at most once. | Service counts, approval and tool rows, and the transcript and spend derived from each script. |
| D1 `test_deadline_races` | Model and tool replies race their deadlines. Run cancellation races approval, parked resume and model settlement. The first commit keeps its receipt. A resume at the parked deadline expires the call. | Service counts, durable receipts and spend derived from the scripted replies and transaction order. |
| G1 `test_gates_fold_by_maximum` | Over every sequence of 0 to 3 gates, the verdict is the greatest gate verdict under Pass < Hold < Deny. | The maximum of each sequence's ranks, computed from the sequence itself. |
| G2 `test_approval_bound_to_arguments` | A held call is approved only by its run's caller and only under the key of the tool, revision and repaired arguments. Keys for the unrepaired arguments or another revision conflict, and a stranger is refused; the call is sealed only after the matching approval. A repeat approval returns Approved, and a later refusal conflicts. | Keys built by hand from the hand-repaired arguments, and the outcome each decision must have. |
| I1 `test_inside_rolls_back_on_error` | An Inside tool's writes commit with its result, and a tool that returns an error leaves none of its writes while its failure commits. | The row each tool writes, by hand. |
| P1 `test_system_patches_fold_to_sections` | Over ten prepares (shows, removals, a failed render, two reorders, a put-off baseline, a narrowing), the fold of the log's System entries is the agent's rendered sections in order, and each prepare appends exactly the smallest patch. | The section texts each step writes and the patch each step must append, by hand. |
| R1 `test_reactions_commit_with_entry` | A reaction's row commits with the entry. A failing reaction leaves the entry, none of its own writes, and a Note naming it. | The rows each reaction writes, by hand. |
| H1 `test_late_decisions_lapse` | A decision at or after a hold's expiry is Lapsed and the call is denied "approval expired", with or without the expiry timer having run. A timer that runs early changes nothing. | Hand-derived outcomes for hold times of 0 and 24 hours. |
| K1 `test_parked_call_resumes_once` | An Inside tool that returns Later runs once and stays parked through further wakes. The first resume settles it with the given result and the round goes on; a second resume is ignored. | The tool's own run count and the bytes passed to resume. |
| J1 `test_round_bytes_bounded` | Preparing a model call writes the same bytes after 10 entries as after 1,000, within 64 bytes. Settling a 65,537-byte Outside result keeps the journal smaller than the result. Recovery after an acknowledged PUT and before settle produces InDoubt without redispatch. A settled reference reads back byte-for-byte and a body with the wrong digest is refused. | The snapshot diff, recovered journal byte account, scripted dispatch counts and the simulated S3 object at the fixture's independently computed SHA-256 key. |
| F1 `test_fork_shares_prefix` | A fork cut after a finished tool round holds exactly the parent's entries before the cut and their visible lines, with no run. Its first request after a new submit is those messages plus the new one, under a fresh baseline. A cut inside a tool round (one of two calls answered) conflicts. | The parent log and the lines each entry shows, written by hand. |
| B1 `test_front_holds_stream` (gateway_laws) | Through the front: a message wakes the session and the open stream shows its rows up to "waiting"; the place then stops under the open stream and is not started again in a quiet second; the browser's approval wakes it and the same stream shows the run's end; halting the front ends the stream. | The rows derived by hand from the script, and the gateway's start and stop counts. |
| W1 `test_wake_resumes_transcript` (gateway_laws) | A session whose place stopped by itself while a call waits for approval is woken by the route that carries the approval. The log read while it was unloaded is the first three entries of the finished log, and the run finishes as the script says. | The five-entry log derived by hand from the script. |
| S1 `test_subagent_once_child_crash`, `test_subagent_once_parent_crash` (gateway_laws) | A subagent's Once tool runs at most once across a cut of either place. The cut call settles InDoubt, there are exactly two sessions, and both logs are as the script says. The cut is a stop with a client left open on the place, so it ends Forced at the grace deadline while the held call is in flight. | The test's own count of `tally` runs and the logs derived by hand. |
| S1 `test_documented_streams_read` | The Anthropic docs' tool-use stream and an OpenAI tool-call stream read to their documented answer (text, calls, joined arguments, stop reason, usage), fed whole or in pieces of 1, 2, 7 or 61 bytes. | The Anthropic streaming docs' example, copied verbatim, and a stream built from OpenAI's documented chunk schema; the expected answers are read off the fixtures by hand. |
| S2 `test_answer_needs_terminator` | A stream cut before `message_stop` or `[DONE]`, cut inside a tool argument, or ending in an error event gives no answer. | Each provider's documented success terminator. |
| W1 `test_request_bodies_follow_wire_format` | Bodies follow each provider's documented message shape: Anthropic groups one turn's tool results into one user message with `is_error`, and OpenAI sends arguments as a string and asks for usage. | The message shapes in each provider's API reference. |
| T1 `test_doubt_only_after_send` | Over a simulated TLS network: no listener is Refused; a stream cut before its terminator or a close with no reply after the request is Unavailable (doubt); a 400 reply is Refused; the documented streams read to their documented answers. | Whether request bytes may have reached the provider in each script, and the documented answers. |

M1 runs over `std.sim.fs` with a 64 KiB port room. A first life that neither ends nor goes idle within a second of its fault is stopped, and the cut follows; 92 of the 217 cases need that stop (REPORT F10). M1 takes about 150 seconds.
