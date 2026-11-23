# harness

A Hirsel LLM harness. One Place holds one session: a model, a narrowed tool
set and an append-only list of entries. A turn is rows and short `store.Tx`
transitions, and no code runs between effects, so a place that stops or dies
at any point reopens and carries on from its rows.

Subagents and forks are not part of the harness. A subagent is an ordinary
Outside tool that calls another session's place and waits for it. A fork is
a new session place seeded from an exported prefix of entries.

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

`laws.hirsel` holds a complete place.

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
`harness.transcript` is the public Cover read of the session.

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

## Repair, gates and approvals

Each derived call passes, in order:

1. The limits. A call past `limits.calls` fails, and a tool that is narrowed
   out or unknown is `Gone`.
2. The tool's `repair`, if it has one. It is a pure function from argument
   bytes to argument bytes, run under `bound bytes limits.gate_bytes` and
   `bound steps limits.gate_steps`. A fault or an overrun fails the call.
   Everything after this step sees the repaired arguments, and the member
   row stores them.
3. The agent's `gates`. Each gate returns `Pass`, `Hold(reason)` or
   `Deny(reason)` under the same bounds. The verdict is the greatest one
   under `Pass < Hold < Deny`, so adding a gate can only make a call
   stricter, and gate order never matters. A gate that overruns denies.

A held call waits under the approval key
`sha256(tool name, tool rev, repaired arguments)`. `harness.decide` approves
or refuses it. Only the run's caller may decide, and only with the key the
call is held under. A key for other arguments or another revision is refused
`Conflict` and changes nothing, so an approval can never carry over to a
call it was not shown.

An Inside tool runs inside `advance`'s transaction under
`bound bytes limits.inside_bytes`. If it returns an error or overruns, its
own writes are rolled back and the failure is committed. Its step use is
bounded only by the entry's step reserve: `bound steps` cannot yet wrap a
body that writes to the transaction (MEM-30).

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

## Laws

| law | claim | oracle |
| --- | --- | --- |
| C1 `test_once_tool_crash` | A place stopped while a Once tool is in flight reopens without resending the tool. The tool result is InDoubt at once, the model answer is not bought again, and the run ends Answered. | The scripted services' own call counts, which outlive the place, plus the transcript and spend derived by hand from the script. |
| G1 `test_gates_fold_by_maximum` | Over every sequence of 0 to 3 gates, the verdict is the greatest gate verdict under Pass < Hold < Deny. | The maximum of each sequence's ranks, computed from the sequence itself. |
| G2 `test_approval_bound_to_arguments` | A held call is approved only by its run's caller and only under the key of the tool, revision and repaired arguments. Keys for the unrepaired arguments or another revision conflict, and a stranger is refused; the call is sealed only after the matching approval. | Keys built by hand from the hand-repaired arguments, and the outcome each decision must have. |
| I1 `test_inside_rolls_back_on_error` | An Inside tool's writes commit with its result, and a tool that returns an error leaves none of its writes while its failure commits. | The row each tool writes, by hand. |
| J1 `test_round_bytes_bounded` | The transaction that appends a user entry and prepares the model call writes the same bytes after 10 earlier entries as after 1,000, within 64 bytes. | `store.changes` between the snapshots around the transaction. A request body copied into any row or trigger argument would add about 198,000 bytes. |

Open: a `std.sim.fs` crash cut at the same boundary as C1.
