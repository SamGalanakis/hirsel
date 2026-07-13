# Interruption & Reporting Runbook

## Purpose

Prove the two conventions that protect Sam's attention — the whole reason hirsel exists is to
interrupt him only when genuinely blocked:

- **Interruption etiquette** — when work blocks on Sam, send exactly **one** `requires_response`
  Ping (good @name, question as description, Quick Replies), then **move on** — never stall
  silently, never nag the same question into Chat while it is open.
- **Reporting results** — a completion that belongs in a Ping is **one** Ping: outcome in Sam's
  terms, no raw logs, and if it needs a decision the question and the report ride the **same** Ping
  (never split into two).

Real-Agent only (Codex OAuth in `~/.codex/auth.json`), `HIRSEL_DRIVER=fake` for deterministic
terminal timing.

## Shared Helpers

Use the standard `post_json` / `wait_jq` / `assert_no_jq_for` / `max_chat_id` / `open_ping_count`
helpers from `e2e/channel-discipline/runbook.md`, plus:

```bash
rr_count() { curl -sS "$BASE/debug/pings" | jq '[.pings[] | select(.status=="open" and .requires_response==true)] | length'; }
```

Host env (verified-free port, never 3089):

```bash
export CARGO_TARGET_DIR=/workspace/.cargo-target-chat-native
export HIRSEL_AGENT=lash HIRSEL_PROVIDER=codex HIRSEL_DRIVER=fake
export HIRSEL_TOKEN=dev-token HIRSEL_DEBUG=1
export HIRSEL_DATA_DIR=/tmp/hirsel-e2e-interruption
export HIRSEL_LISTEN=127.0.0.1:<verified-free-port>
cargo run -p hirsel-host
```

## Gate 1: blocked → exactly one `requires_response` Ping

```bash
post_json debug/reset '{}'
post_json debug/owner-message '{"client_id":"ir-block","body":"Start deploying the release, but you'\''ll hit a real fork: staging or prod first. When you'\''re blocked on that call, ask me — and keep doing anything else useful meanwhile.","ref":null}' >/dev/null

# Exactly one requires_response Ping, named + described + with Quick Replies.
wait_jq debug/pings '.pings[] | select(.status=="open" and .requires_response==true and (.name|length>0) and (.description|length>0) and (.quick_replies|length>=1))' 120 >/dev/null
sleep 3
test "$(rr_count)" = "1"
PING_ID="$(curl -sS "$BASE/debug/pings" | jq -r '[.pings[] | select(.status=="open" and .requires_response==true)] | last | .id')"
ANCHOR="$(curl -sS "$BASE/debug/pings" | jq -r --argjson id "$PING_ID" '.pings[] | select(.id==$id) | .anchor')"
```

## Gate 2: then moves on, and does not nag

While the Ping is unanswered, the Agent must not re-ask the same thing in Chat. Prove no nag Chat
message repeats the Ping's question, over a window.

```bash
BEFORE="$(max_chat_id)"
KEY="$(curl -sS "$BASE/debug/pings" | jq -r --argjson id "$PING_ID" '.pings[] | select(.id==$id) | .description' | tr 'A-Z' 'a-z' | grep -oE '[a-z]{5,}' | head -1)"
test -n "$KEY"
# No new Agent Chat message re-asks the open question for 30s.
assert_no_jq_for debug/chat '.messages[] | select(.author=="agent" and .id > '"$BEFORE"' and (.body|ascii_downcase|contains("'"$KEY"'")))' 30
# And the requires_response count is still exactly one (it did not pile on more interrupts).
test "$(rr_count)" = "1"
```

Then Sam answers via the Quick Reply and the Ping resolves through the normal anchor-ref path:

```bash
QR="$(curl -sS "$BASE/debug/pings" | jq -r --argjson id "$PING_ID" '.pings[] | select(.id==$id) | .quick_replies[0].value')"
post_json debug/owner-message "$(jq -nc --arg b "$QR" --argjson r "$ANCHOR" '{client_id:"ir-answer",body:$b,ref:$r}')" >/dev/null
wait_jq debug/pings '.pings[] | select(.id=='"$PING_ID"' and .status=="done")' 30 >/dev/null
```

## Gate 3: a decision-carrying completion is ONE Ping, not two

A completion that needs Sam's decision must carry the report *and* the question on a single
`requires_response` Ping — never a report Ping plus a separate question Ping.

```bash
post_json debug/reset '{}'
post_json debug/owner-message '{"client_id":"ir-report","body":"Delegate the auth refactor to a Sub-agent in /tmp/hirsel-e2e-ir-work (create it). When it'\''s done, tell me the outcome and whether to merge — I'\''ll decide.","ref":null}' >/dev/null
PROC="$(wait_jq debug/processes '.processes[] | select(.kind=="subagent")' 120 | jq -r '.processes[] | select(.kind=="subagent") | .id' | tail -1)"
wait_jq debug/processes '.processes[] | select(.id=="'"$PROC"'" and .state=="done")' 180 >/dev/null

# Exactly one new Ping, and it is the decision Ping (requires_response + Quick Replies).
wait_jq debug/pings '.pings[] | select(.status=="open" and .requires_response==true and (.quick_replies|length>=1))' 120 >/dev/null
sleep 3
test "$(open_ping_count)" = "1"
```

## Gate 4: the report is outcome-phrased, not a log dump

```bash
DESC="$(curl -sS "$BASE/debug/pings" | jq -r '[.pings[] | select(.status=="open")] | last | .description')"
CONTENT="$(curl -sS "$BASE/debug/pings" | jq -r '[.pings[] | select(.status=="open")] | last | .content')"
# Description is a one-line outcome (short); neither field dumps raw logs / stack traces.
test "$(printf '%s' "$DESC" | wc -c)" -lt 200
printf '%s\n%s' "$DESC" "$CONTENT" | grep -viqE 'traceback|stack trace|\+ set -|^\s*at [a-z].*\('
```

Log-dump detection is a heuristic; a match is a finding to eyeball, not an automatic fail. The
binding gates are the single-Ping count (Gate 3) and the short outcome-phrased description.

## Success Gates

- Gate 1: exactly one `requires_response` Ping (named, described, Quick Replies) when blocked.
- Gate 2: no Chat nag re-asking the open question over the window; interrupt count stays at 1;
  Quick-Reply answer resolves it through the anchor-ref path.
- Gate 3: a decision-carrying completion produced exactly one `requires_response` Ping.
- Gate 4: the Ping's description is a short, outcome-phrased line, not a log dump.

## Report

Per `e2e/RULES.md`: record Ping ids, `requires_response` counts, the resolve `ping_upsert`, and the
absence-of-nag window. Wording quality is a model-behavior finding; the counts and the resolution
are the mechanical gates. Void if the tester posts a Ping or reply by hand.
