# Delegation Hygiene Runbook

## Purpose

Prove the three delegation conventions from `prompts/agent.md` that keep the Sub-agent fleet sane,
and in doing so exercise the `subagents.prompt` and `subagents.list` tools that no other runbook
covers:

1. **Delegation note** — one concise Chat note stating what is being delegated *before* the spawn.
2. **No redundant sessions** — a follow-up on live work goes to the existing session via
   `subagents.prompt`, never a sibling spawn (`subagents.list` is consulted first).
3. **Worktree hygiene** — two Sub-agents on one repo get two distinct working directories.

Real-Agent only (Codex OAuth in `~/.codex/auth.json`); these are judgment behaviors the scripted
double cannot exhibit. Uses `HIRSEL_DRIVER=fake` so terminal timing is deterministic — the gates
are about *how* the Agent delegates, not what a real Sub-agent produces (that is `real-subagent`).

## Shared Helpers

Use the standard `post_json` / `wait_jq` / `assert_no_jq_for` / `max_chat_id` helpers from
`e2e/channel-discipline/runbook.md`, plus:

```bash
subagent_count() { curl -sS -H "authorization: Bearer $HIRSEL_TOKEN" "$BASE/debug/processes" | jq '[.processes[] | select(.kind=="subagent")] | length'; }
```

Host env (pick a verified-free port, never 3089):

```bash
export CARGO_TARGET_DIR=/workspace/.cargo-target-chat-native
export HIRSEL_AGENT=lash HIRSEL_PROVIDER=codex HIRSEL_DRIVER=fake
export HIRSEL_TOKEN=dev-token HIRSEL_DEBUG=1
export HIRSEL_DATA_DIR=/tmp/hirsel-e2e-delegation-hygiene
export HIRSEL_LISTEN=127.0.0.1:<verified-free-port>
cargo run -p hirsel-host
```

## Gate 1: a Chat note precedes the spawn

```bash
post_json debug/reset '{}'
BEFORE="$(max_chat_id)"
post_json debug/owner-message '{"client_id":"dh-note","body":"Delegate a small refactor to a Sub-agent working in /tmp/hirsel-e2e-dh-work (create it). Tell me what you'\''re handing off before you start it.","ref":null}' >/dev/null

# An Agent Chat note exists, and a Sub-agent process exists. The note is short (a hand-off line, not a report).
NOTE="$(wait_jq debug/chat '.messages[] | select(.author=="agent" and .id > '"$BEFORE"')' 120 | jq -r '[.messages[] | select(.author=="agent" and .id > '"$BEFORE"')] | first | .body')"
wait_jq debug/processes '.processes[] | select(.kind=="subagent")' 60 >/dev/null
wait_jq debug/broadcasts '[.events[] | select(.type=="agent_activity")] | last | .state=="idle"' 60 >/dev/null && curl -sS -H "authorization: Bearer $HIRSEL_TOKEN" "$BASE/debug/processes" | jq -e '.processes[] | select(.kind=="subagent" and .state=="running")' >/dev/null
test "$(printf '%s' "$NOTE" | wc -c)" -lt 400   # a delegation note, not a wall of text
```

Model-behavior finding: the note should name *what* is delegated. That the note came *before* the
process is the ordering intent; because both land inside one turn, treat the presence of a short
pre-report Chat note plus a live process as the mechanical gate, and record wording as a finding.

## Gate 2: follow-up reuses the session (`list` → `prompt`, no sibling)

```bash
post_json debug/reset '{}'
post_json debug/owner-message '{"client_id":"dh-spawn","body":"Delegate a long build task to one Sub-agent in /tmp/hirsel-e2e-dh-reuse (create it).","ref":null}' >/dev/null
PROC="$(wait_jq debug/processes '.processes[] | select(.kind=="subagent")' 120 | jq -r '.processes[] | select(.kind=="subagent") | .id' | tail -1)"
test "$(subagent_count)" = "1"

# Now ask for MORE on the same work. The Agent must follow up on the running session, not spawn a twin.
post_json debug/owner-message '{"client_id":"dh-follow","body":"Actually also have that same Sub-agent add a test while it'\''s at it — don'\''t start a second one.","ref":null}' >/dev/null

# Mechanical: it used subagents.prompt on the existing process, and the Sub-agent count never grew.
wait_jq debug/broadcasts '.events[] | select(.type=="turn_event" and .event.kind=="tool_start" and .event.name=="subagents_prompt")' 120 >/dev/null
assert_no_jq_for debug/processes '[.processes[] | select(.kind=="subagent")] | length > 1' 20
test "$(subagent_count)" = "1"
```

A `subagents_list` tool event before the `subagents_prompt` is corroborating evidence the Agent
checked first — record it if present, but the binding gate is *no second Sub-agent + a prompt to
the first*.

## Gate 3: two Sub-agents, two working directories

```bash
post_json debug/reset '{}'
rm -rf /tmp/hirsel-e2e-dh-parallel; mkdir -p /tmp/hirsel-e2e-dh-parallel
post_json debug/owner-message '{"client_id":"dh-par","body":"Parallelize across TWO Sub-agents on the repo at /tmp/hirsel-e2e-dh-parallel: give each its own worktree/branch under that path so they never share a checkout. Create the dirs you need.","ref":null}' >/dev/null

# Two distinct Sub-agent processes come up.
wait_jq debug/processes '[.processes[] | select(.kind=="subagent")] | length >= 2' 120 >/dev/null
IDS="$(curl -sS -H "authorization: Bearer $HIRSEL_TOKEN" "$BASE/debug/processes" | jq -r '[.processes[] | select(.kind=="subagent") | .id] | unique | length')"
test "$IDS" -ge 2

# Mechanical filesystem proof: the Agent created at least two distinct working dirs under the repo path.
DIRS="$(find /tmp/hirsel-e2e-dh-parallel -mindepth 1 -maxdepth 2 -type d | sort -u | wc -l)"
test "$DIRS" -ge 2
```

`ProcessInfo` does not expose `cwd`, so distinct working directories are proven from the filesystem
the Agent actually set up, the same way `real-subagent` proves work from repo state. If the Agent
put both Sub-agents in one directory, that is a real worktree-hygiene violation — fail and report
the shared path.

## Success Gates

- Gate 1: a short Agent Chat hand-off note accompanies a live Sub-agent process.
- Gate 2: a same-work follow-up produced a `subagents_prompt` tool event with the Sub-agent count
  staying at exactly 1 (no sibling spawn).
- Gate 3: two distinct Sub-agent process ids and two distinct working directories on disk.

## Report

Per `e2e/RULES.md`: record process ids, the `subagents_prompt`/`subagents_list` tool events, and
the on-disk directories. Note wording quality of the delegation note as a model-behavior finding.
A run is void if the tester created processes or directories outside the Agent tool path.
