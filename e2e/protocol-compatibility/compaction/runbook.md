# Compaction Runbook

## Purpose

Prove the Agent can compact its own context with `control.continue_as`, that the operation is
**observable** through the host debug surface (a committed control tool event, not a private
side-effect), and — as the design intent — that a fact carried in the compaction `seed` survives
into the post-compaction frame.

The mechanical gate here is the **visible control event**: compaction that leaves no trace on
`/debug/broadcasts` or the committed Chat `tool_calls` is indistinguishable from the Agent silently
doing nothing. Post-compaction recall is the *behavioral* gate on top of it, and it currently trips
a **known upstream lash defect** — see "Known limitation" below.

Real timing is deterministic: `HIRSEL_AGENT=lash HIRSEL_PROVIDER=codex HIRSEL_DRIVER=fake`.

## Execute

```bash
e2e/protocol-compatibility/compaction/run.sh
```

`CARGO_TARGET_DIR` is set by the runner; the host is started fresh on a chosen free port (never
3089) and the runner sources the shared `e2e/protocol-compatibility/lib/runbook-lib.sh` helpers (`post_json` / `wait_jq` /
`wait_agent_message_after` / `max_chat_id` / `pass_gate`). Drive it by hand with those same helpers
if you are running the agent-driven variant.

## Gates

### Gate 1: the pre-compaction fact is acknowledged

Owner sends `GREEN-742` as an exact fact to remember; an Agent turn completes against it
(`wait_agent_message_after`). This seeds the thing compaction must preserve.

### Gate 2: the Owner asks the Agent to compact immediately

Owner instructs `control.continue_as` now, with the seed required to preserve `GREEN-742`.

### Gate 3 (mechanical, hard): a control/compaction tool event is visible

`/debug/broadcasts` shows a `turn_event` `tool_done` whose name matches
`continue_as|continue|control|compact` with `ok == true` (or, as a fallback, a `continue`/`control`
tool call committed on the Agent Chat message). If recall works but **no** control event is visible,
this runbook fails mechanically for a missing compaction surface — the whole point is that
compaction is observable.

### Gate 4 (behavioral, KNOWN-FAIL): post-compaction recall returns `GREEN-742`

Owner asks for the exact pre-compaction fact. The design intent is that the `seed` carried it across
the frame boundary, so the Agent answers `GREEN-742`. **This currently fails deterministically** (see
below). The runner records the outcome without aborting — Gate 3 is the mechanical gate; Gate 4 is
the intent this scenario exists to track, and it flips to a real pass the day the upstream fix lands.

## Known limitation — upstream lash #8 (continue_as seed drop)

Recall fails **deterministically**, not flakily, and the defect is in **lash, not hirsel** — do not
try to fix it here. hirsel drives the Agent through `session.queued_turn()` (the durable turn pump,
chosen for idempotent mid-turn injection and send/queue/cancel semantics). On frame materialization
that path recovers the `SwitchAgentFrame` payload from the assembled model-visible
`ToolCallRecord`s, but the RLM-internal Lashlang tool calls that carry the `continue_as` seed are
intentionally not emitted there, so lash falls back to empty `initial_nodes`
(`turn_boundary.rs:703`) and the seed is discarded. The compacted frame therefore loses the
pre-compaction fact and the recall answer comes back as an internal-looking `md…` id instead of
`GREEN-742`.

This is pre-existing on lash HEAD (byte-identical to alpha.88 — not a pin regression) and needs an
upstream fix that makes queued turns carry the seed the way direct `session.turn()` turns do.
Tracked as task #8. It is the same class of queued-turn-path defect as the qwc claim-renewal bug
(task #20). Cross-ref `e2e/protocol-compatibility/abandoned-recovery` for the sibling pattern of a documented known-open
finding a runbook gates *around* rather than *on*.

## Success Gates

- Gate 1: the pre-compaction fact turn completed.
- Gate 2: the Owner's compact-now instruction was delivered.
- Gate 3 (hard): a `continue_as`/control compaction tool event is visible on `/debug/broadcasts` or
  the committed Chat `tool_calls`.
- Gate 4 (known-fail): post-compaction recall of `GREEN-742` — expected to fail until lash #8 is
  fixed; recorded, not aborted.

A run is a mechanical **pass** when Gates 1–3 hold. Gate 4 is reported as `KNOWN-FAIL` (or as a real
pass if the upstream fix has landed and recall now returns `GREEN-742`).

## Report

Per `e2e/protocol-compatibility/RULES.md`: record the Chat ids for the fact and recall turns and the `turn_event` that
proved the control tool call. If recall returns an `md…` id instead of `GREEN-742`, report it as the
known lash #8 seed-drop finding with the observed id — not as a hirsel regression.
