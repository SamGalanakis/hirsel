# Views Lifecycle Runbook

## Purpose

Prove the standalone generative-UI substrate end to end: show a resolved View, update the same
instance and re-broadcast it, route Owner interactions, resolve `ping:<id>` placement to the Event's
Chat anchor, replay active Views in a fresh `hello_ok.views`, then clear an instance with a
`view_removed` broadcast.

The shipped debug surface exposes show, list, and View events, but not update or clear. Consequently
this scenario uses `HIRSEL_AGENT=lash`, `HIRSEL_PROVIDER=codex`, and `HIRSEL_DRIVER=fake` solely to
invoke the real `views.update` and `views.clear` tools. Every success assertion is mechanical on
`/debug/views`, `/debug/events`, `/debug/chat`, `/debug/broadcasts`, or a fresh `/ws` hello; model
wording is never a gate. The data dir and runtime cwd remain neutral `/tmp` state, and no Sub-agent
is involved.

## Execute

Requires existing Codex OAuth credentials:

```bash
/workspace/code/hirsel-rbcov/e2e/protocol-compatibility/views-lifecycle/run.sh
```

## Gate 1: show and resolved state

`POST /debug/show-view` resolves the bundled `task-progress` template on `canvas`. Require the
returned instance in `/debug/views` with its placement and resolved JSON spec, plus an identical
`view_upsert` broadcast.

## Gate 2: update in place

Ask the Agent to call `views.update` exactly once for the recorded instance id. Require that same id
to carry the updated progress/labels in `/debug/views`, a second `view_upsert` to carry the new
resolved spec, and the committed Agent tool summary to contain successful `views_update`.

## Gate 3: owner interaction routing

`POST /debug/view-event` on the canvas instance with action `advance` must return and persist a
normal Owner Chat message containing the structured action data.

## Gate 4: Task placement preserves its anchor without settling

The test actor invokes the digest producer once to obtain a real Event and anchor—no standing
schedule is created. Show another View at `ping:<event-id>`, submit its `view_event`, and require the
resulting Owner message's `ref` to equal the Event anchor. The anchor-refed ingress must resolve the
Event to done.

## Gate 5: reconnect replay

Open a brand-new authenticated WebSocket with the dependency-free Node ≥22 helper. Its
`hello_ok.views` must include both the updated canvas View and the Ping-placed View with their exact
placements/specs.

## Gate 6: clear and removal broadcast

Ask the Agent to call `views.clear` exactly once for the canvas instance. Require a successful
`views_clear` tool summary, `view_removed` with the same id, and absence from `/debug/views`.

## Success Gates

- Gate 1: show → resolved `/debug/views` row + `view_upsert`.
- Gate 2: update in place → changed spec + second `view_upsert`.
- Gate 3: canvas `view_event` → persisted Owner interaction.
- Gate 4: `ping:<id>` interaction → anchor-refed Owner ingress + resolved Event.
- Gate 5: active Views replay exactly in fresh `hello_ok.views`.
- Gate 6: clear → `view_removed` + dropped active row.

## Report

Record both View ids, the Event/anchor pair, the two upserts, interaction Chat ids, the hello replay
fields, the removal broadcast, and the successful update/clear tool summaries. Stop rather than
inventing update/clear state if the real provider cannot execute the requested tools.
