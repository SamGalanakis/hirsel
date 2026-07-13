# Channel Discipline Runbook

## Purpose

Prove the single most-exercised judgment in `prompts/agent.md` — **channel discipline**: a
result that arrives while the exchange is still warm is answered in **Chat**, work that lands
*outside* the live exchange becomes a **Ping**, a pure acknowledgment is filed **nowhere**, and
nothing is ever filed in **both** places. This is a real-Agent behavior runbook: the plumbing it
rides on is already proven elsewhere; here the gate is *which surface the output landed on*.

Requires Codex OAuth credentials in `~/.codex/auth.json`. Scripted mode cannot exhibit this
judgment and is not a valid substitute.

## Shared Helpers

Pick a verified-free port (`ss -tlnp`; never 3089 — that is the live instance).

```bash
export CARGO_TARGET_DIR=/workspace/.cargo-target-chat-native
export HIRSEL_AGENT=lash
export HIRSEL_PROVIDER=codex
export HIRSEL_TOKEN=dev-token
export HIRSEL_DEBUG=1
export HIRSEL_DRIVER=fake
export HIRSEL_DATA_DIR=/tmp/hirsel-e2e-channel-discipline
export HIRSEL_LISTEN=127.0.0.1:<verified-free-port>
cargo run -p hirsel-host
```

In another shell (`BASE` = the same host):

```bash
BASE=http://127.0.0.1:<verified-free-port>

post_json() { curl -sS -X POST "$BASE/$1" -H 'content-type: application/json' -d "$2"; }

wait_jq() {  # path filter [timeout]
  end=$((SECONDS + ${3:-60}))
  while [ "$SECONDS" -lt "$end" ]; do
    json="$(curl -sS "$BASE/$1")" || return 1
    printf '%s' "$json" | jq -e "$2" >/dev/null && { printf '%s\n' "$json"; return 0; }
    sleep 0.5
  done
  printf 'Timed out waiting for %s filter %s\n' "$1" "$2" >&2; return 1
}

assert_no_jq_for() {  # path filter seconds
  end=$((SECONDS + $3))
  while [ "$SECONDS" -lt "$end" ]; do
    json="$(curl -sS "$BASE/$1")" || return 1
    printf '%s' "$json" | jq -e "$2" >/dev/null && {
      printf 'Unexpected match for %s filter %s\n%s\n' "$1" "$2" "$json" >&2; return 1; }
    sleep 0.5
  done
}

max_chat_id() { curl -sS "$BASE/debug/chat" | jq '[.messages[].id] | max // 0'; }
open_ping_count() { curl -sS "$BASE/debug/pings" | jq '[.pings[] | select(.status=="open")] | length'; }
```

## Gate 1: warm exchange → answered in Chat, no Ping

A question Sam asks *now* is a reply, not async work. The Agent must answer in Chat and file no
Ping — never a "requires_response" interrupt for something he is actively waiting on.

```bash
post_json debug/reset '{}'
BEFORE="$(max_chat_id)"; PINGS_BEFORE="$(open_ping_count)"
post_json debug/owner-message '{"client_id":"cd-warm","body":"Quick one while I'\''m here: in one word, is 17 prime? Just answer.","ref":null}' >/dev/null

# Mechanical: an Agent Chat reply appears...
wait_jq debug/chat '.messages[] | select(.author=="agent" and .id > '"$BEFORE"')' 120 >/dev/null
# ...and no Ping was filed for a live-exchange answer.
assert_no_jq_for debug/pings '.pings[] | select(.status=="open")' 20
test "$(open_ping_count)" = "$PINGS_BEFORE"
```

Model-behavior finding: the reply should be terse (`yes`). Wording is a finding, not a hard gate.

## Gate 2: pure acknowledgment → filed nowhere as a Ping

"It worked" is not Ping material. A trivial glance the Agent performs and confirms must land in
Chat (or be silent) — never as a Ping, and never as a `requires_response`.

```bash
BEFORE="$(max_chat_id)"; PINGS_BEFORE="$(open_ping_count)"
post_json debug/owner-message '{"client_id":"cd-ack","body":"Confirm /tmp exists on the box, then just tell me here. Nothing else.","ref":null}' >/dev/null

wait_jq debug/chat '.messages[] | select(.author=="agent" and .id > '"$BEFORE"')' 120 >/dev/null
# No Ping of any kind was created for a pure acknowledgment.
assert_no_jq_for debug/pings '.pings[] | select(.status=="open")' 20
test "$(open_ping_count)" = "$PINGS_BEFORE"
```

If the Agent used `shell.run` for the glance, `/debug/broadcasts` shows a `shell_run` tool event —
record it as evidence the confirmation was real, not invented.

## Gate 3: outside the live exchange → a Ping, not a Chat interjection

Delegate long work, then go quiet. The terminal wake arrives *after* the exchange has cooled, so
its outcome is Ping material. The fake driver's completion is the "hours later" stand-in.

```bash
post_json debug/reset '{}'
BEFORE="$(max_chat_id)"
post_json debug/owner-message '{"client_id":"cd-outside","body":"Delegate a fix to a Sub-agent in /tmp/hirsel-e2e-cd-work (create it), and after it finishes, decide whether to ship. I am stepping away.","ref":null}' >/dev/null

# The delegation itself produces a Sub-agent process (proven in delegation-loop); wait for terminal.
PROC="$(wait_jq debug/processes '.processes[] | select(.kind=="subagent")' 120 | jq -r '.processes[] | select(.kind=="subagent") | .id' | tail -1)"
wait_jq debug/processes '.processes[] | select(.id=="'"$PROC"'" and .state=="done")' 180 >/dev/null

# Mechanical: the outcome surfaces as a Ping (named, described), NOT as a fresh Chat line the Owner never asked for.
wait_jq debug/pings '.pings[] | select(.status=="open" and (.name|length>0) and (.description|length>0))' 120 >/dev/null
```

## Gate 4: never filed in both places

The single most-cited channel-discipline rule ("never file the same event in both places"). Take
the outcome text the Agent chose for its Ping and prove that same outcome did not also get posted
as an Agent Chat message.

```bash
PING_DESC="$(curl -sS "$BASE/debug/pings" | jq -r '[.pings[] | select(.status=="open")] | last | .description')"
# The description's distinctive words must not also appear verbatim in any Agent Chat message.
KEY="$(printf '%s' "$PING_DESC" | tr 'A-Z' 'a-z' | grep -oE '[a-z]{5,}' | head -1)"
test -n "$KEY"
curl -sS "$BASE/debug/chat" \
  | jq -e --arg k "$KEY" 'all(.messages[]; (.author=="agent" and (.body|ascii_downcase|contains($k))) | not)'
```

A match here is a real channel-discipline violation (double-filing), not a wording nit — report it
as a mechanical fail with both the Ping and the offending Chat id.

## Success Gates

- Gate 1: an Agent Chat reply to a warm question, with zero Pings created.
- Gate 2: a pure acknowledgment produced no Ping of any kind.
- Gate 3: a post-cooldown delegated outcome surfaced as a named, described Ping.
- Gate 4: the Ping's outcome was not also posted as an Agent Chat message.

## Report

Per `e2e/RULES.md`: record the Chat/Ping ids that proved each surface choice. Terseness and exact
wording are model-behavior findings; *which surface the output landed on* is the mechanical gate.
A run is void if the tester posted any Chat message or Ping by hand.
