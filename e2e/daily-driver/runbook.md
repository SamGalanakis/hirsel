# Daily Driver Runbook

## Purpose

Prove the **whole daily-driver loop composes in one continuous session** — the SCOPE Slice-1 path
Sam actually lives in from his phone: a warm question answered in Chat, then a real delegation he
steps away from, then progress, then a completion that comes back as a named `requires_response`
Ping with Quick Replies, then a tap that auto-resolves it, then the Agent's acknowledgement. The
individual judgments (channel discipline, delegation hygiene, interruption etiquette, reporting)
are each gated in their own runbook; **here the gate is that they chain end-to-end without a reset
between them**, the way a real day runs.

Real-Agent only (Codex OAuth in `~/.codex/auth.json`); a scripted double cannot exhibit the
surface-choice judgment this loop depends on. `HIRSEL_DRIVER=fake` so the Sub-agent's terminal
timing is deterministic — the "hours later" completion is the fake driver's terminal event, not a
wall-clock wait. There is exactly **one `debug/reset`**, at the top: every gate below runs against
the same session, in order.

## Shared Helpers

Use the standard `post_json` / `wait_jq` / `assert_no_jq_for` / `max_chat_id` / `open_ping_count`
helpers from `e2e/channel-discipline/runbook.md`, plus:

```bash
rr_count() { curl -sS -H "authorization: Bearer $HIRSEL_TOKEN" "$BASE/debug/pings" | jq '[.pings[] | select(.status=="open" and .requires_response==true)] | length'; }
agent_since() { curl -sS -H "authorization: Bearer $HIRSEL_TOKEN" "$BASE/debug/chat" | jq --argjson b "$1" '[.messages[] | select(.author=="agent" and .id > $b)] | length'; }
```

Host env (pick a verified-free port with `ss -tlnp`; never 3089 — that is the live instance):

```bash
export CARGO_TARGET_DIR=/workspace/.cargo-target-chat-native
export HIRSEL_AGENT=lash HIRSEL_PROVIDER=codex HIRSEL_DRIVER=fake
export HIRSEL_TOKEN=dev-token HIRSEL_DEBUG=1
export HIRSEL_DATA_DIR=/tmp/hirsel-e2e-daily-driver
export HIRSEL_LISTEN=127.0.0.1:<verified-free-port>
cargo run -p hirsel-host
```

```bash
post_json debug/reset '{}'   # the ONLY reset in this runbook
```

## Gate 1: morning warm exchange → Chat, no Ping

The day opens with a question Sam asks while he is right there. It is a reply, not async work:
answered in **Chat**, with **no Ping**.

```bash
BEFORE="$(max_chat_id)"; PINGS_BEFORE="$(open_ping_count)"
post_json debug/owner-message '{"client_id":"dd-warm","body":"Morning — quick one while I'\''m here: is the host listening on a loopback-only port by default? One line.","ref":null}' >/dev/null

wait_jq debug/chat '.messages[] | select(.author=="agent" and .id > '"$BEFORE"')' 120 >/dev/null
assert_no_jq_for debug/pings '.pings[] | select(.status=="open")' 15
test "$(open_ping_count)" = "$PINGS_BEFORE"
```

## Gate 2: delegate real work + a hand-off note, then step away

Sam hands off a task and leaves. Before the spawn the Agent drops one short Chat note saying what
it is delegating (delegation hygiene), and a Sub-agent process comes up.

```bash
BEFORE="$(max_chat_id)"
post_json debug/owner-message '{"client_id":"dd-deleg","body":"Now the real thing: delegate the failing-test fix to a Sub-agent in /tmp/hirsel-e2e-dd-work (create it). Tell me what you'\''re handing off, then when it'\''s done let me know whether to ship — I'\''m stepping out.","ref":null}' >/dev/null

# A short pre-report Chat note...
NOTE="$(wait_jq debug/chat '.messages[] | select(.author=="agent" and .id > '"$BEFORE"')' 120 | jq -r '[.messages[] | select(.author=="agent" and .id > '"$BEFORE"')] | first | .body')"
test "$(printf '%s' "$NOTE" | wc -c)" -lt 400
# ...and a live Sub-agent.
PROC="$(wait_jq debug/processes '.processes[] | select(.kind=="subagent")' 120 | jq -r '.processes[] | select(.kind=="subagent") | .id' | tail -1)"
test -n "$PROC"
```

## Gate 3: progress is observable, then the work finishes

```bash
# Progress: process_upsert broadcasts accrue for this process while it runs.
wait_jq debug/broadcasts '.events[] | select(.type=="process_upsert" and .process.id=="'"$PROC"'")' 60 >/dev/null
# Terminal: the Sub-agent reaches done (the "hours later" wake).
wait_jq debug/processes '.processes[] | select(.id=="'"$PROC"'" and .state=="done")' 180 >/dev/null
```

## Gate 4: completion returns as ONE requires_response Ping with Quick Replies

The exchange has long cooled, so a decision-carrying completion is Ping material — exactly one
`requires_response` Ping, named + described + with Quick Replies, carrying both the outcome and the
ship/no-ship question on the same Ping (never a report Ping plus a separate question Ping).

```bash
PING_ID="$(wait_jq debug/pings '.pings[] | select(.status=="open" and .requires_response==true and (.name|length>0) and (.description|length>0) and (.quick_replies|length>=1))' 120 | jq -r '[.pings[] | select(.status=="open" and .requires_response==true)] | last | .id')"
sleep 3
test "$(rr_count)" = "1"
ANCHOR="$(curl -sS -H "authorization: Bearer $HIRSEL_TOKEN" "$BASE/debug/pings" | jq -r '.pings[] | select(.id=='"$PING_ID"') | .anchor')"
test "$ANCHOR" != "null"
```

## Gate 5: Sam taps a Quick Reply → the Ping auto-resolves

```bash
BEFORE="$(max_chat_id)"
QR="$(curl -sS -H "authorization: Bearer $HIRSEL_TOKEN" "$BASE/debug/pings" | jq -r '.pings[] | select(.id=='"$PING_ID"') | .quick_replies[0].value')"
post_json debug/owner-message "$(jq -nc --arg b "$QR" --argjson r "$ANCHOR" '{client_id:"dd-answer",body:$b,ref:$r}')" >/dev/null

# Anchor-refed Owner reply moves the Ping to done via ping_upsert.
wait_jq debug/pings '.pings[] | select(.id=='"$PING_ID"' and .status=="done")' 30 >/dev/null
wait_jq debug/broadcasts '.events[] | select(.type=="ping_upsert" and .ping.id=='"$PING_ID"' and .ping.status=="done")' 30 >/dev/null
```

## Gate 6: the Agent closes the loop, and nothing was double-filed

The reply lands back with the Agent as a persisted Agent Chat acknowledgement (the loop closes),
and the Ping's outcome was not also posted as a Chat line (channel discipline holds across the
whole session).

```bash
wait_jq debug/chat '.messages[] | select(.author=="agent" and .id > '"$BEFORE"')' 120 >/dev/null
# The Ping description's distinctive word must not also appear in any Agent Chat message.
PING_DESC="$(curl -sS -H "authorization: Bearer $HIRSEL_TOKEN" "$BASE/debug/pings" | jq -r '.pings[] | select(.id=='"$PING_ID"') | .description')"
KEY="$(printf '%s' "$PING_DESC" | tr 'A-Z' 'a-z' | grep -oE '[a-z]{5,}' | head -1)"
test -n "$KEY"
curl -sS -H "authorization: Bearer $HIRSEL_TOKEN" "$BASE/debug/chat" \
  | jq -e --arg k "$KEY" 'all(.messages[]; (.author=="agent" and (.body|ascii_downcase|contains($k))) | not)'
```

## Success Gates

- Gate 1: a warm question answered in Chat with zero Pings.
- Gate 2: a short pre-spawn delegation note plus a live Sub-agent process.
- Gate 3: `process_upsert` progress for that process, then terminal `state:"done"`.
- Gate 4: exactly one `requires_response` Ping (named, described, Quick Replies, anchored) carrying
  the decision.
- Gate 5: a Quick-Reply Owner reply, anchor-refed, auto-resolves the Ping (`ping_upsert` → done).
- Gate 6: a persisted Agent acknowledgement, and the Ping outcome not double-filed into Chat.

All six pass **in one session with a single reset** — the point of this runbook is that the loop
holds together continuously, not that each gate passes in isolation (that is the discipline
runbooks' job).

## Report

Per `e2e/RULES.md`: record the Chat/Ping/process ids that proved each gate and the resolving
`ping_upsert`. Terseness and wording are model-behavior findings; the surface choices, the
single-Ping decision, the anchor-refed auto-resolution, and the closing acknowledgement are the
mechanical gates. A run is void if the tester posted any Chat message, Ping, or reply by hand, or
issued a second reset mid-run.
