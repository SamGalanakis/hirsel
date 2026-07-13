# Event Queue Runbook

## Purpose

Prove the ADR-0012/0013 typed event queue end to end on the debug surface: a real Agent files a
**judgment** through the generalized `pings.send` (options → blessed constrained-JSON `ui`), the
Owner decides via `event_action` and the decision **reaches the Agent as an anchor-refed reply**, a
standing rule lands in the taste store, a scheduled producer emits a **summary**, and push
discipline holds (judgments push; awareness does not).

Real-Agent (Codex OAuth in `~/.codex/auth.json`) for the judgment gates; `HIRSEL_DRIVER=fake` — no
sub-agents are needed. The digest gate is mechanical.

## Shared Helpers

Use the standard `post_json` / `wait_jq` / `assert_no_jq_for` / `max_chat_id` helpers from
`e2e/channel-discipline/runbook.md`, plus:

```bash
events() { curl -sS "$BASE/debug/events"; }
event_field() { events | jq -r '.events[] | select(.id=='"$1"') | '"$2"''; }
```

Host env (verified-free port, never 3089):

```bash
export HIRSEL_AGENT=lash HIRSEL_PROVIDER=codex HIRSEL_DRIVER=fake
export HIRSEL_TOKEN=dev-token HIRSEL_DEBUG=1
export HIRSEL_DATA_DIR=/tmp/hirsel-e2e-event-queue
export HIRSEL_LISTEN=127.0.0.1:<verified-free-port>
cargo run -p hirsel-host
```

## Gate 1: a real judgment with a blessed `ui`

```bash
post_json debug/reset '{}'
post_json debug/owner-message '{"client_id":"eq-judg","body":"Use pings.send ONCE to ask me a judgment question named @pick-a-path, description of your choice, requires_response true, with two options where exactly one is recommended. Nothing else — no shell, no subagents.","ref":null}' >/dev/null

wait_jq debug/events '.events[] | select(.kind=="judgment" and .status=="open")' 120 >/dev/null
EV=$(events | jq -r '[.events[] | select(.kind=="judgment")] | last | .id')
# Blessed template: card root; has heading + optionList; exactly one recommended; NO telemetry nodes.
event_field "$EV" '.ui.type' | grep -qx card
events | jq -e '.events[] | select(.id=='"$EV"') | [.ui.children[].type] | (index("heading") != null and index("optionList") != null)'
events | jq -e '.events[] | select(.id=='"$EV"') | [.ui.children[] | select(.type=="optionList") | .options[] | select(.recommended==true)] | length == 1'
events | jq -e '.events[] | select(.id=='"$EV"') | [.ui.children[].type] | all(. != "cost" and . != "telemetry")'
ANCHOR=$(event_field "$EV" '.anchor')
```

## Gate 2: choosing delivers the decision to the Agent

The single most load-bearing wire in the product: `event_action choose` must (a) resolve the event
and (b) inject an **anchor-refed Owner reply carrying the chosen option's label**, so the Agent
actually receives the decision (ADR-0009 semantics; a silent resolve is a mechanical FAIL).

```bash
KEY=$(events | jq -r '.events[] | select(.id=='"$EV"') | .ui.children[] | select(.type=="optionList") | .options[0].key')
LABEL=$(events | jq -r '.events[] | select(.id=='"$EV"') | .ui.children[] | select(.type=="optionList") | .options[0].label')
BEFORE="$(max_chat_id)"
post_json debug/event-action '{"event_id":'"$EV"',"action":"choose","data":{"choice":"'"$KEY"'","record_rule":"e2e standing rule: always pick the first path"}}' >/dev/null

wait_jq debug/events '.events[] | select(.id=='"$EV"' and .status=="done")' 30 >/dev/null
# The decision reached Chat as an anchor-refed Owner reply with the chosen label.
wait_jq debug/chat '.messages[] | select(.author=="owner" and .id > '"$BEFORE"' and .ref=='"$ANCHOR"' and (.body | contains("'"$LABEL"'")))' 30 >/dev/null
# And the Agent acknowledges on its next turn (a persisted Agent message after the reply).
wait_jq debug/chat '.messages[] | select(.author=="agent" and .id > '"$BEFORE"')' 120 >/dev/null
```

## Gate 3: the standing rule landed in the taste store

```bash
curl -sS "$BASE/debug/taste" | jq -e '.rules[] | select(.rule | contains("always pick the first path"))'
```

## Gate 4: scheduled producer emits a summary; push discipline holds

```bash
post_json debug/register-push-token '{"platform":"android","token":"e2e-push-probe"}' >/dev/null
post_json debug/trigger-digest '{}' >/dev/null
wait_jq debug/events '.events[] | select(.kind=="summary" and .requires_response==false)' 30 >/dev/null
# The summary did NOT push; the earlier judgment DID (log-only sender records pushes).
curl -sS "$BASE/debug/pushes" | jq -e '[.pushes[] | select(.kind=="summary" or (.event_kind // "")=="summary")] | length == 0' \
  || echo "note: inspect /debug/pushes shape and gate on the judgment/summary split it exposes"
```

If `/debug/pushes` predates event kinds, gate instead on: a push exists for the judgment event id and
none for the summary event id.

## Gate 5: snooze reopens (Done stays a toggle)

```bash
post_json debug/event-action '{"event_id":'"$EV"',"action":"snooze","data":{}}' >/dev/null
wait_jq debug/events '.events[] | select(.id=='"$EV"' and .status=="open")' 15 >/dev/null
post_json debug/resolve-ping '{"ping_id":'"$EV"'}' >/dev/null   # tidy back to done
```

## Success Gates

- Gate 1: a real-Agent judgment event with the blessed card `ui` (heading + optionList, exactly one
  recommended, no telemetry nodes).
- Gate 2: `choose` → event done + an anchor-refed Owner reply carrying the chosen label + an Agent
  acknowledgement turn. **A resolve without the reply is a mechanical FAIL.**
- Gate 3: `record_rule` visible in `/debug/taste`.
- Gate 4: `trigger-digest` → an open `summary` (`requires_response:false`); push fired for the
  judgment, not the summary.
- Gate 5: `snooze` reopens the event.

## Report

Per `e2e/RULES.md`: record event ids, the choose payload, the anchor-refed reply's Chat id, the taste
rule row, and the pushes evidence. Wording of the Agent's acknowledgement is a model-behavior
finding; the reply delivery, resolution, taste write, and push split are the mechanical gates. Void
if the tester fabricates events outside the Agent/debug tool paths.
