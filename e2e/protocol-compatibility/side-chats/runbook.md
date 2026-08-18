# Side Chats Runbook

> Compatibility runbook: this exercises the legacy `ping_id` + conclude/confirm flow retained for
> older clients. The current product keeps a Task open while its generated instrument advances or
> settles in place; that contract is exercised by [`../../generated-task-ui/runbook.md`](../../generated-task-ui/runbook.md).
> This runbook makes no current product-surface claim.
> The retained runtime is disabled by default; this runbook opts in with
> `HIRSEL_COMPAT_SIDE_SESSIONS=1`.

## Purpose

Prove the complete side-thread loop (ADR-0008/0009): open a Task into a seeded side session, converse on a distinct `sc` scope while the main thread stays untouched, resume the same live session, draft a conclusion, confirm it into the main thread as the Owner's anchor-refed reply without settling the Task, delete the ephemeral transcript, and see the main Agent react.

## Start

Pick a verified-free port first (`ss -tlnp`; foreign processes squat on this VM; never use 3089 — that is the live instance).

```bash
export CARGO_TARGET_DIR=/workspace/.cargo-target-sidechat
export HIRSEL_AGENT=scripted
export HIRSEL_TOKEN=dev-token
export HIRSEL_DEBUG=1
export HIRSEL_DRIVER=fake
export HIRSEL_COMPAT_SIDE_SESSIONS=1
export HIRSEL_DATA_DIR=/tmp/hirsel-e2e-side-chats
rm -rf "$HIRSEL_DATA_DIR"
export HIRSEL_LISTEN=127.0.0.1:<verified-free-port>
cargo run -p hirsel-host
```

In another shell (`PORT` = the same port):

```bash
BASE=http://127.0.0.1:$PORT

post_json() {
  curl -sS -X POST "$BASE/$1" -H "authorization: Bearer $HIRSEL_TOKEN" -H 'content-type: application/json' -d "$2"
}

wait_jq() {
  path="$1"
  filter="$2"
  timeout="${3:-30}"
  end=$((SECONDS + timeout))
  while [ "$SECONDS" -lt "$end" ]; do
    json="$(curl -sS -H "authorization: Bearer $HIRSEL_TOKEN" "$BASE/$path")" || return 1
    if printf '%s' "$json" | jq -e "$filter" >/dev/null; then
      printf '%s\n' "$json"
      return 0
    fi
    sleep 0.25
  done
  printf 'Timed out waiting for %s filter %s\n' "$path" "$filter" >&2
  curl -sS -H "authorization: Bearer $HIRSEL_TOKEN" "$BASE/$path" >&2 || true
  return 1
}

max_chat_id() {
  curl -sS -H "authorization: Bearer $HIRSEL_TOKEN" "$BASE/debug/chat" | jq '[.messages[].id] | max // 0'
}
```

## Scenario A: Scripted Deterministic

Execute all scripted gates directly with:

```bash
export CARGO_TARGET_DIR=/workspace/.cargo-target-scaffold
e2e/protocol-compatibility/side-chats/run.sh
```

Reset, then use the existing fake-driver delegation path to create a genuine Agent-sent Ping:

```bash
post_json debug/reset '{}'
post_json debug/owner-message '{"client_id":"side-seed","body":"Please delegate a trivial repo fix, then ask me before applying the result.","ref":null}' >/dev/null
PINGS_JSON="$(wait_jq debug/pings '.pings[] | select(.status == "open" and .requires_response == true and (.name | length > 0) and (.description | length > 0))' 30)"
PING_ID="$(printf '%s' "$PINGS_JSON" | jq -r '.pings[] | select(.status == "open" and .requires_response == true) | .id' | tail -1)"
ANCHOR_ID="$(printf '%s' "$PINGS_JSON" | jq -r '.pings[] | select(.id == ('$PING_ID')) | .anchor')"
```

### Gate 1: creation is empty and scoped

```bash
OPEN_JSON="$(post_json debug/open-side-chat '{"ping_id":'"$PING_ID"'}')"
SC="$(printf '%s' "$OPEN_JSON" | jq -r '.sc')"
printf '%s' "$OPEN_JSON" | jq -e '.resumed == false and (.messages | length == 0) and (.sc | startswith("side:"))'
```

### Gate 2: side conversation stays scoped, main Chat untouched

```bash
MAIN_BEFORE="$(max_chat_id)"
post_json debug/side-message '{"sc":"'"$SC"'","body":"Ship after the final check."}' >/dev/null
wait_jq debug/side-chats '.side_chats[] | select(.sc == "'"$SC"'") | .messages | length >= 2' 10 >/dev/null
wait_jq debug/side-chats '.side_chats[] | select(.sc == "'"$SC"'") | .messages[] | select(.author == "agent" and .body == "(side chat) noted: Ship after the final check.")' 10 >/dev/null
wait_jq debug/broadcasts '.events[] | select(.type == "turn_event" and .sc == "'"$SC"'")' 10 >/dev/null
wait_jq debug/broadcasts '.events[] | select(.type == "agent_activity" and .sc == "'"$SC"'" and .state == "thinking")' 10 >/dev/null
```

Main Chat must be byte-for-byte untouched by the side conversation:

```bash
test "$(max_chat_id)" = "$MAIN_BEFORE"
curl -sS -H "authorization: Bearer $HIRSEL_TOKEN" "$BASE/debug/chat" | jq -e 'all(.messages[]; .body != "Ship after the final check." and .body != "(side chat) noted: Ship after the final check.")'
```

### Gate 3: reopening resumes the live transcript

```bash
RESUME_JSON="$(post_json debug/open-side-chat '{"ping_id":'"$PING_ID"'}')"
printf '%s' "$RESUME_JSON" | jq -e '.resumed == true and .sc == "'"$SC"'" and (.messages | length >= 2)'
```

### Gate 4: conclude drafts but does not persist the draft

```bash
BEFORE_COUNT="$(curl -sS -H "authorization: Bearer $HIRSEL_TOKEN" "$BASE/debug/side-chats" | jq '.side_chats[] | select(.sc == "'"$SC"'") | .messages | length')"
DRAFT_JSON="$(post_json debug/conclude '{"sc":"'"$SC"'"}')"
printf '%s' "$DRAFT_JSON" | jq -e '.sc == "'"$SC"'" and (.text | contains("Draft reply regarding")) and (.text | contains("Ship after the final check."))'
curl -sS -H "authorization: Bearer $HIRSEL_TOKEN" "$BASE/debug/side-chats" | jq -e '(.side_chats[] | select(.sc == "'"$SC"'") | .messages | length) == '"$BEFORE_COUNT"
```

### Gate 5: confirm posts edited text without settling, then tears down

Confirm with OWNER-EDITED text (not the draft verbatim — the edit surviving to main Chat is part of the contract):

```bash
CONFIRM_BEFORE="$(max_chat_id)"
post_json debug/confirm-conclusion '{"sc":"'"$SC"'","text":"Ship it after the final check."}' >/dev/null
wait_jq debug/chat '.messages[] | select(.author == "owner" and .body == "Ship it after the final check." and .ref == '"$ANCHOR_ID"')' 10 >/dev/null
wait_jq debug/pings '.pings[] | select(.id == '"$PING_ID"' and .status == "done")' 10 >/dev/null
# Post-cutover the host broadcasts event_upsert (events generalize pings; same id space).
wait_jq debug/broadcasts '.events[] | select(.type == "event_upsert" and .event.id == '"$PING_ID"' and .event.status == "done")' 10 >/dev/null
wait_jq debug/side-chats 'all(.side_chats[]; .sc != "'"$SC"'")' 10 >/dev/null
wait_jq debug/broadcasts '.events[] | select(.type == "side_chat_closed" and .sc == "'"$SC"'")' 10 >/dev/null
```

### Gate 6: the main Agent reacts to the conclusion

The confirmed conclusion goes through the normal owner-message ingress, so the main Agent must take a turn on it (scripted mode acknowledges anchor-refed replies):

```bash
wait_jq debug/chat '.messages[] | select(.author == "agent" and .id > '"$CONFIRM_BEFORE"')' 30 >/dev/null
```

### Gate 7: resolve-the-Ping-mid-side-chat, then conclude — idempotent

```bash
post_json debug/owner-message '{"client_id":"side-seed-2","body":"Please delegate another trivial repo fix, then ask me before applying the result.","ref":null}' >/dev/null
PING2_JSON="$(wait_jq debug/pings '.pings[] | select(.status == "open" and .requires_response == true and .id != '"$PING_ID"')' 30)"
PING2_ID="$(printf '%s' "$PING2_JSON" | jq -r '.pings[] | select(.status == "open" and .id != '"$PING_ID"') | .id' | tail -1)"
ANCHOR2_ID="$(printf '%s' "$PING2_JSON" | jq -r '.pings[] | select(.id == ('$PING2_ID')) | .anchor')"
SC2="$(post_json debug/open-side-chat '{"ping_id":'"$PING2_ID"'}' | jq -r '.sc')"
post_json debug/side-message '{"sc":"'"$SC2"'","body":"Working this out."}' >/dev/null
wait_jq debug/side-chats '.side_chats[] | select(.sc == "'"$SC2"'") | .messages | length >= 2' 10 >/dev/null

# Resolve the Ping out from under the open side chat.
post_json debug/resolve-ping '{"ping_id":'"$PING2_ID"'}' | jq -e '.status == "done"' >/dev/null
wait_jq debug/pings '.pings[] | select(.id == '"$PING2_ID"' and .status == "done")' 10 >/dev/null

# Conclude + confirm must still work; reply auto-resolution is a no-op for the already-done Ping.
post_json debug/conclude '{"sc":"'"$SC2"'"}' | jq -e '.text | length > 0'
post_json debug/confirm-conclusion '{"sc":"'"$SC2"'","text":"Already done, still concluded."}' | jq -e '.ok == true'
wait_jq debug/chat '.messages[] | select(.author == "owner" and .body == "Already done, still concluded." and .ref == '"$ANCHOR2_ID"')' 10 >/dev/null
wait_jq debug/pings '.pings[] | select(.id == '"$PING2_ID"' and .status == "done")' 5 >/dev/null
wait_jq debug/broadcasts '.events[] | select(.type == "side_chat_closed" and .sc == "'"$SC2"'")' 10 >/dev/null
wait_jq debug/side-chats 'all(.side_chats[]; .sc != "'"$SC2"'")' 10 >/dev/null
```

## Scenario B: Real Lash Agent With Codex Provider And Fake Driver

Requires Codex OAuth credentials in `~/.codex/auth.json`. Mechanical gates (scoping, resolution, teardown) must pass; model-behavior gates (seed awareness, reaction wording) are reported honestly if missed.

```bash
export CARGO_TARGET_DIR=/workspace/.cargo-target-sidechat
export HIRSEL_AGENT=lash
export HIRSEL_PROVIDER=codex
export HIRSEL_TOKEN=dev-token
export HIRSEL_DEBUG=1
export HIRSEL_DRIVER=fake
export HIRSEL_DATA_DIR=/tmp/hirsel-e2e-side-chats-codex
rm -rf "$HIRSEL_DATA_DIR"
export HIRSEL_LISTEN=127.0.0.1:<verified-free-port>
cargo run -p hirsel-host
```

Send a requires-response Ping directly (faster than full delegation):

```bash
post_json debug/reset '{}'
post_json debug/owner-message '{"client_id":"side-real-send","body":"Use pings.send to send one requires_response Ping named quarterly-report-format with description Choose the quarterly report format, content exactly \"Choose the quarterly report format\", and quick replies pdf and slides. End the turn with no chat text.","ref":null}' >/dev/null
PINGS_JSON="$(wait_jq debug/pings '.pings[] | select(.status == "open" and .requires_response == true and (.content | contains("quarterly report format")) and (.name | length > 0) and (.description | length > 0))' 180)"
PING_ID=...; ANCHOR_ID=...   # extract as in Scenario A
```

### Gate B1: the side session is seeded (model-behavior)

```bash
SC="$(post_json debug/open-side-chat '{"ping_id":'"$PING_ID"'}' | jq -r '.sc')"
MAIN_BEFORE="$(max_chat_id)"
post_json debug/side-message '{"sc":"'"$SC"'","body":"In one sentence: what Ping are we discussing here?"}' >/dev/null
wait_jq debug/side-chats '.side_chats[] | select(.sc == "'"$SC"'") | .messages[] | select(.author == "agent")' 120 >/dev/null
```

The Agent's first side reply must reference the Ping (for example, mention "quarterly report"). If the transport worked but the model missed it, report a prompt-behavior miss, not a mechanical failure.

### Gate B2: scoping is mechanical — must pass

```bash
wait_jq debug/broadcasts '.events[] | select(.type == "turn_event" and .sc == "'"$SC"'")' 10 >/dev/null
wait_jq debug/broadcasts '.events[] | select(.type == "agent_activity" and .sc == "'"$SC"'")' 10 >/dev/null
test "$(max_chat_id)" = "$MAIN_BEFORE"
```

### Gate B3: conclude → draft → confirm edited → main-chat reply + resolution + teardown + reaction

```bash
post_json debug/side-message '{"sc":"'"$SC"'","body":"Recommend pdf, monthly cadence."}' >/dev/null
wait_jq debug/side-chats '.side_chats[] | select(.sc == "'"$SC"'") | .messages | length >= 4' 120 >/dev/null
DRAFT="$(post_json debug/conclude '{"sc":"'"$SC"'"}' | jq -r '.text')"
test -n "$DRAFT"
CONFIRM_BEFORE="$(max_chat_id)"
post_json debug/confirm-conclusion "$(jq -nc --arg sc "$SC" --arg text "EDITED: $DRAFT" '{sc:$sc,text:$text}')" >/dev/null
wait_jq debug/chat '.messages[] | select(.author == "owner" and (.body | startswith("EDITED: ")) and .ref == '"$ANCHOR_ID"')' 10 >/dev/null
wait_jq debug/pings '.pings[] | select(.id == '"$PING_ID"' and .status == "done")' 10 >/dev/null
wait_jq debug/side-chats 'all(.side_chats[]; .sc != "'"$SC"'")' 10 >/dev/null
wait_jq debug/broadcasts '.events[] | select(.type == "side_chat_closed" and .sc == "'"$SC"'")' 10 >/dev/null
wait_jq debug/chat '.messages[] | select(.author == "agent" and .id > '"$CONFIRM_BEFORE"')' 120 >/dev/null
```

The final agent message is the main Agent reacting to the conclusion; whether its wording references the conclusion content is a model-behavior observation — report it.

### Gate B4: resolve-mid-side-chat idempotency (mechanical — must pass)

Repeat Scenario A Gate 7 against a second real-Agent-sent Ping (or instruct the Agent to resolve its own Ping while the side chat is open, which also exercises `pings.resolve`).

## Report

Record for each gate the observed id/field per `e2e/protocol-compatibility/RULES.md`. A run is void if the Ping, side reply, conclusion row, or done state was fabricated outside the normal host paths. Real-variant model-behavior misses are findings, not forced passes.
