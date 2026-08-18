# Event Archive & Honest Undo Runbook

## Purpose

Prove the owner-facing manual archive lifecycle for an **open judgment**. Archiving is an
authoritative auto-dismiss (`status=done`, `archived=true`, `archived_at` stamped), removes the card
from the live unarchived feed while preserving history, and broadcasts `event_upsert`. Undo is
deliberately honest: `unarchive` only restores queue visibility; `reopen-ping` is the second action
that restores the judgment to open. A separately archived event must survive SIGTERM and reboot.

Fully mechanical: `HIRSEL_AGENT=scripted`, `HIRSEL_DRIVER=fake`, a disposable `/tmp` data dir, and a
free port chosen from 3300. The scripted delegation path creates both judgments; the tester never
inserts an Event.

## Execute

From any neutral working directory:

```bash
/workspace/code/hirsel-rbcov/e2e/protocol-compatibility/event-archive-undo/run.sh
```

The runner sources `e2e/protocol-compatibility/lib/runbook-lib.sh`, starts the prebuilt host from the neutral cwd, and uses
`/tmp/hirsel-e2e-event-archive-undo` for all durable state.

## Gate 1: archive auto-dismisses an open judgment

Create an open `requires_response` judgment through scripted delegation, then:

```bash
post_json debug/event-action \
  '{"event_id":'"$EVENT_ID"',"action":"archive","data":{}}'
```

The returned Event and `/debug/broadcasts` must show `status:"done"`, `archived:true`, a non-null
`archived_at`, and an `event_upsert` carrying the same authoritative state.

## Gate 2: it leaves the live feed without losing history

`/debug/events` must still contain the archived id. Filtering the same durable set to
`archived:false`—the resting live-feed contract—must not contain it.

## Gate 3: unarchive alone is not full restore

```bash
post_json debug/event-action \
  '{"event_id":'"$EVENT_ID"',"action":"unarchive","data":{}}'
```

Require `archived:false`, `archived_at:null`, and still `status:"done"`, plus the matching
`event_upsert`. This is the important non-claim: visibility restoration does not silently reopen a
decision.

## Gate 4: reopen completes honest undo

```bash
post_json debug/reopen-ping '{"ping_id":'"$EVENT_ID"'}'
```

The same Event must return to `status:"open"`, remain unarchived, and retain
`requires_response:true`.

## Gate 5: archived state survives restart

Create a second open judgment through the same scripted producer, archive it, record its exact
`archived_at`, stop with SIGTERM, and boot on the same data dir. `/debug/events` must return the same
id with `status:"done"`, `archived:true`, and the unchanged stamp.

## Success Gates

- Gate 1: open judgment → archive auto-dismiss + stamp + `event_upsert`.
- Gate 2: absent from the unarchived live feed, present in `/debug/events` history.
- Gate 3: `unarchive` clears archive fields but leaves `status=done`.
- Gate 4: `reopen-ping` returns it to open `requires_response` state.
- Gate 5: a second archived judgment and its timestamp survive SIGTERM + reboot.

## Report

Record both Event ids, the first archive/unarchive/reopen states, the archive broadcasts, and the
second Event's before/after `archived_at`. The run is void if the tester inserts or edits Event rows
directly.
