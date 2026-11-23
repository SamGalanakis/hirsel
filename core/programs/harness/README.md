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

Open: a `std.sim.fs` crash cut at the same boundary.
