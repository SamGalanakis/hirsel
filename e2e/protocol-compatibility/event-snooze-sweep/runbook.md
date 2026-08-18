# Event Snooze & Sweep Runbook

## Purpose

Prove the wave-3 event lifecycle additions end to end on the host debug surface plus the real
WebSocket wire:

- **Durable snooze** (`event_action snooze{until}`): a snooze without `until` is a retryable error
  naming the presets; a valid snooze persists `snoozed_until`; at the instant the HOST clears the
  field on its own timer, broadcasts the `event_upsert`, and **re-pushes an open judgment** (the
  return IS a new interrupt); returns survive a host restart (boot scan).
- **Unsnooze** (`event_action unsnooze`): clears the field immediately.
- **The sweep** (`clear_finished_events`, the new ClientToHost frame the web "Clear finished (n)"
  sends): archives exactly the finished set (done, or read awareness), stamps `archived_at` for the
  day-log, and never touches an open judgment.

Fully mechanical: `HIRSEL_AGENT=scripted` + `HIRSEL_DRIVER=fake`. The judgment is filed through the
scripted host/Agent delegation flow (never synthesized by the tester); the summary comes from the
scheduled-digest producer. The sweep gate drives the REAL `/ws` ingress — the exact frame the web
client emits — via the dependency-free `clear-finished.cjs` (Node ≥ 22 built-in WebSocket; runs
from any neutral cwd).

## Execute

```bash
e2e/protocol-compatibility/event-snooze-sweep/run.sh
```

Everything below is what the script does, gate by gate, for a manual run.

## Start Host

Standard neutral-workdir boot (see `e2e/protocol-compatibility/RULES.md`): scripted agent, fake driver, `/tmp` data dir,
verified-free port, `HIRSEL_DEBUG=1`. Source `e2e/protocol-compatibility/lib/runbook-lib.sh` and use `start_hirsel_host`.

```bash
export HIRSEL_AGENT=scripted HIRSEL_DRIVER=fake HIRSEL_TOKEN=dev-token
DATA_DIR=/tmp/hirsel-e2e-event-snooze-sweep
PORT="$(choose_port 3290)"
start_hirsel_host fresh
```

## Gate 1: a judgment and a summary exist through real producer paths

```bash
post_json debug/reset '{}'
post_json debug/register-push-token '{"platform":"android","token":"e2e-snooze-probe"}'
post_json debug/owner-message '{"client_id":"ess-delegate","body":"Please delegate a trivial task to a Sub-agent working in /tmp/hirsel-e2e-event-snooze-sweep-work (create it if needed), then ask me before applying the result.","ref":null}'
# EV: the scripted flow files an open requires_response judgment; it pushes (judgments push).
wait_jq debug/events '.events[] | select(.status=="open" and .requires_response==true)' 60
wait_jq debug/pushes '[.pushes[] | select(.payload.data.event_id=='"$EV"')] | length >= 1' 15
# SUMMARY: the scheduled producer emits read-later awareness.
post_json debug/trigger-digest '{}'
```

## Gate 2: a snooze without `until` is a retryable error naming the presets

`data:{}` (the pre-wave-3 shape) must be REJECTED, and the error must name the presets so a client
can recover. The event is untouched.

```bash
BODY=$(curl -sS -X POST "$BASE/debug/event-action" -H "authorization: Bearer $HIRSEL_TOKEN" \
  -H 'content-type: application/json' -d '{"event_id":'"$EV"',"action":"snooze","data":{}}')
echo "$BODY" | jq -e '.error | test("preset") and test("This evening")'
event_field "$EV" '.snoozed_until' | grep -qx null
```

Past timestamps and non-RFC3339 strings must fail the same way.

## Gate 3: a valid snooze parks the event; the host timer returns it and re-pushes

```bash
UNTIL=$(date -u -d '+4 seconds' +%Y-%m-%dT%H:%M:%SZ)
post_json debug/event-action '{"event_id":'"$EV"',"action":"snooze","data":{"until":"'"$UNTIL"'"}}' \
  | jq -e '.snoozed_until != null'
# The event_upsert carrying the parked state was broadcast.
wait_jq debug/broadcasts '.events[] | select(.type=="event_upsert" and .event.id=='"$EV"' and .event.snoozed_until != null)' 10
# The HOST clears the field at the instant — no client involvement.
wait_jq debug/events '.events[] | select(.id=='"$EV"' and .snoozed_until == null)' 20
# The returned open judgment RE-pushed: a second push for the same event id.
wait_jq debug/pushes '[.pushes[] | select(.payload.data.event_id=='"$EV"')] | length >= 2' 15
```

## Gate 4: a pending return survives a host restart

```bash
UNTIL=$(date -u -d '+10 seconds' +%Y-%m-%dT%H:%M:%SZ)
post_json debug/event-action '{"event_id":'"$EV"',"action":"snooze","data":{"until":"'"$UNTIL"'"}}'
restart_hirsel_host   # SIGTERM + boot on the same data dir
wait_jq debug/events '.events[] | select(.id=='"$EV"' and .snoozed_until != null or .snoozed_until == null)' 5
# After the instant passes, the rebooted host's scan/timer returns it.
wait_jq debug/events '.events[] | select(.id=='"$EV"' and .snoozed_until == null)' 30
```

## Gate 5: unsnooze returns the event immediately

```bash
UNTIL=$(date -u -d '+1 hour' +%Y-%m-%dT%H:%M:%SZ)
post_json debug/event-action '{"event_id":'"$EV"',"action":"snooze","data":{"until":"'"$UNTIL"'"}}' | jq -e '.snoozed_until != null'
post_json debug/event-action '{"event_id":'"$EV"',"action":"unsnooze","data":{}}' | jq -e '.snoozed_until == null'
```

## Gate 6: the sweep archives read awareness with `archived_at`, keeping the open judgment

Mark the digest summary read (finished awareness), then drive the REAL wire: the node script opens
`/ws`, completes `hello`/`hello_ok`, and sends `{"type":"clear_finished_events"}` — byte-identical
to the web client's sweep.

```bash
post_json debug/read-ping '{"ping_id":'"$SUMMARY"'}'
node "$REPO/e2e/protocol-compatibility/event-snooze-sweep/clear-finished.cjs" "ws://127.0.0.1:$PORT/ws" "$HIRSEL_TOKEN"
# The read summary is archived with a fresh RFC3339 archived_at (the day-log stamp)…
wait_jq debug/events '.events[] | select(.id=='"$SUMMARY"' and .archived==true and .status=="done" and .archived_at != null)' 15
events | jq -e '.events[] | select(.id=='"$SUMMARY"') | (now - (.archived_at | fromdateiso8601)) | (. >= 0 and . < 120)'
# …and the still-open judgment SURVIVED the sweep untouched.
events | jq -e '.events[] | select(.id=='"$EV"') | .archived==false and .status=="open"'
```

## Gate 7: a decided judgment falls to the next sweep

```bash
post_json debug/resolve-ping '{"ping_id":'"$EV"'}'
node "$REPO/e2e/protocol-compatibility/event-snooze-sweep/clear-finished.cjs" "ws://127.0.0.1:$PORT/ws" "$HIRSEL_TOKEN"
wait_jq debug/events '.events[] | select(.id=='"$EV"' and .archived==true and .archived_at != null)' 15
```

## Success Gates

- Gate 1: an open `requires_response` judgment filed by the scripted host flow (with its creation
  push) and a digest summary — no tester-synthesized events.
- Gate 2: `snooze` with no/invalid/past `until` → an error naming the presets; `snoozed_until`
  stays null.
- Gate 3: `snooze{until}` persists `snoozed_until` + broadcasts the upsert; the HOST clears it at
  the instant, and the returned open judgment gains a SECOND push for the same event id.
- Gate 4: a pending return survives SIGTERM + reboot on the same data dir.
- Gate 5: `unsnooze` clears the field immediately.
- Gate 6: `clear_finished_events` over the real `/ws` archives the read summary with a fresh
  `archived_at`, and the open judgment survives the sweep.
- Gate 7: once decided, the judgment falls to the next sweep with its own `archived_at`.

## Report

Per `e2e/protocol-compatibility/RULES.md`: record the event ids, the exact rejected-snooze error body, the `snoozed_until`
values observed before/after the timer return and the restart, the push counts per event id, and
the `archived_at` stamps. Void if the tester sets `snoozed_until`/`archived`/`archived_at` by any
path other than `debug/event-action`, the `/ws` sweep frame, and the host's own timer.
