# Normal User E2E Coverage Report

Date: 2026-07-10

## Coverage Audit

| Scenario / product surface | Covered where | Gap before this pass | Result after this pass |
| --- | --- | --- | --- |
| Attachment protocol | `e2e/attachments` | Only proved upload/replay/fetch and scripted stored-path notes. | Added `e2e/attachment-agent-behavior` for real vision plus text-file tool use. |
| Real Sub-agent delegation | `e2e/delegation-loop` used fake driver only. | No end-to-end real Codex Driver coverage. | Added `e2e/real-subagent`; real Codex fixed a repo and interruption reached `cancelled`. |
| Abandoned process recovery | None. | ADR-0004 promise was untested. | Added `e2e/abandoned-recovery`; no respawn passed, abandoned visibility failed. |
| Multi-turn conversation memory | `e2e/restart-persistence` only asked one-off exact replies across restarts. | No ordinary multi-turn recall before and after restart. | Added `e2e/multi-turn-memory`; recall passed before and after restart. |
| Compaction | None. | `prompts/agent.md` documents `control.continue_as`, but no runbook checked it. | Added `e2e/compaction`; `continue_as` was visible, but post-compaction recall failed. |
| Ping read state | `e2e/ping-read` | Read/unread only; no reply/resolution lifecycle. | Added `e2e/pings-lifecycle`; reply, Owner resolution, and Agent resolution are covered. |
| Delegation loop | `e2e/delegation-loop` | Covered fake-driver terminal and Quick Reply flow. | Still covered; real-driver gap moved to `real-subagent`. |
| Send/queue/cancel | `e2e/send-queue-cancel` | Covered send injection, next-turn queueing, queued cancel, active-turn cancel. | No new gap found in this pass. |
| Monitors | `e2e/monitors` | Covered debug-created and Agent-created monitors, wake, restart. | No new gap found in this pass. |
| Timers | `e2e/timers` | Covered one-shot timer wake. | No new gap found in this pass. |
| Turn timeline and tool visibility | `e2e/turn-timeline` | Covered ordered `turn_event` and tool summaries. | New scenarios also exercise `continue_as`, `shell_run`, `subagents_*`, and `pings_resolve` visibility. |
| Debug resolution surface | `POST /debug/resolve-ping` | Explicit Owner resolution needed a debug gate. | The canonical debug route now exercises it and emits `ping_upsert`. |

Residual lower-priority gaps: worktree hygiene/no redundant sibling enforcement is still only indirectly observed through Agent tool choices, not asserted as a dedicated scenario. Channel-discipline edge cases such as delayed completion choosing Ping vs Chat are partially exercised but not exhaustively scored.

## Execution Results

| Runbook | Gate | Status | Evidence |
| --- | --- | --- | --- |
| `real-subagent` | Fixture starts failing | pass | `/tmp/hirsel-e2e-real-subagent-fix-repo`, initial `python3 -m unittest -v` failed. |
| `real-subagent` | Real Codex process with explicit model | pass | `proc-6e3f8e19-cf4b-4d92-bca4-ddcf38d95e65`, `agent=codex`, `model=gpt-5.5`. |
| `real-subagent` | Progress events accumulate | pass | Multiple `process_upsert` events for the process. |
| `real-subagent` | Terminal wake and Agent report | pass | Process `state=done`; Agent chat id 2 reported fix and verification. |
| `real-subagent` | Mechanical repo proof | pass | Final `python3 -m unittest -v`: 1 test passed. |
| `real-subagent` | Interrupt long process | pass | `proc-1b9f0e66-75c4-4b0f-8340-78065f545251` reached `state=cancelled`, summary `interrupted`. |
| `abandoned-recovery` | Start fake long Sub-agent | pass | `proc-cacd239c-c044-4eeb-b2bc-caed4c9f45b8` running before SIGKILL. |
| `abandoned-recovery` | Reboot same data dir | pass | Host rebooted on `/tmp/hirsel-e2e-abandoned-recovery-data`. |
| `abandoned-recovery` | No mechanical respawn | pass | 20s poll found no running subagent after reboot. |
| `abandoned-recovery` | Abandoned process visible | fail-mechanical | `/debug/processes` after reboot was `{"processes":[]}`, not `state=abandoned`. |
| `abandoned-recovery` | Agent judgment wake | not reached | RULES abort after abandoned visibility failure. |
| `multi-turn-memory` | Message 3 recalls messages 1 and 2 | pass | Agent chat id 6: `CODENAME=ORCHID-17; WINDOW=THURSDAY-MORNING`. |
| `multi-turn-memory` | Restart continuity | pass | After restart, Agent chat id 8 repeated both facts. |
| `multi-turn-memory` | No turn failure | pass | Transcript contained no `Agent turn failed`. |
| `compaction` | `continue_as` visible | pass | `turn_event` `tool_done`, name `continue_as`, `ok=true`. |
| `compaction` | Post-compaction recall | fail-prompt-behavior | Agent chat id 5 returned `md9034f1898db4d23a720ec5c628f2975` instead of `GREEN-742`. |
| `attachment-agent-behavior` | Owner replay includes two attachments | pass | Owner chat id 1 had PNG and text blob. |
| `attachment-agent-behavior` | Text attachment second line | pass | Agent chat id 2 included `SECOND-LINE-TOKEN-8842`. |
| `attachment-agent-behavior` | Image word | pass | Agent chat id 2 included `IMAGE_WORD=LIME`. |
| `attachment-agent-behavior` | Tool use on stored path | pass | Agent chat id 2 had `tool_calls=[{"name":"shell_run","ok":true}]`. |
| `pings-lifecycle` | Named requires-response Ping | pass | Scripted Ping 1 was `@delegated-fix-ready`, with description and Quick Reply. |
| `pings-lifecycle` | Anchor-refed Owner reply | pass | Owner `ref=2` moved Ping 1 to done with `ping_upsert`. |
| `pings-lifecycle` | Lifecycle-neutral mention | pass | Owner message 7 mentioned Ping 2; Ping 2 remained open. |
| `pings-lifecycle` | Explicit Owner resolution | pass | `/debug/resolve-ping` moved Ping 2 to done. |
| `pings-lifecycle` | Real Agent sends/resolves moot Ping | pass | Real `@moot-question` had a description; `pings_resolve` was committed. |
| `ping-read` | Read persistence | pass | Ping 1 changed to `read=true`, emitted `ping_upsert`, and remained read after restart. |
| `delegation-loop` | Scripted loop | pass | Process reached done; named Ping 1 auto-resolved from Quick Reply 3; Agent acknowledged. |
| `side-chats` | Scripted loop | pass | Scoped resume/draft gates passed; Conclusion auto-resolved Ping 1; already-done Ping 2 remained idempotent. |
| Provider smoke | Named Ping and reply | pass | Ping 1 was `@smoke-test`, description `Smoke test`; Agent reply named `@smoke-test`. |

Run logs are in `/tmp/hirsel-e2e-*.run.log`; final debug snapshots are in `/tmp/hirsel-e2e-*-final.*.json` where the run reached final snapshot.

## Product Findings

1. `abandoned-recovery`: after SIGKILL and reboot, orphaned subagent work is not surfaced through `/debug/processes` as `abandoned`; the debug process list is empty. The no-respawn safety gate passed.
2. `compaction`: `continue_as` is observable and succeeds, but the compacted frame did not preserve/use the seed fact on the next question. It returned an internal-looking `md...` id instead.
3. Explicit Owner Ping resolution is available at `/debug/resolve-ping`; reply-driven resolution remains the normal path.
4. `HIRSEL_MODEL=gpt-5` failed with the current Codex ChatGPT account: "The 'gpt-5' model is not supported when using Codex with a ChatGPT account." The runbooks use the branch default `gpt-5.5`.

## Commits

- `73ebf43` Add normal user e2e runbooks
- `d7c1dc5` Remove generated runbook bytecode
- `bbb42e3` Ignore Python bytecode
- `cee0b7e` Use supported Codex model in runbooks
- `47b29b9` Gate compaction on continue_as events

## Git Log

```text
47b29b9 Gate compaction on continue_as events
cee0b7e Use supported Codex model in runbooks
bbb42e3 Ignore Python bytecode
d7c1dc5 Remove generated runbook bytecode
73ebf43 Add normal user e2e runbooks
fcf980f Finish timeline event id reconciliation
5691c5e Merge branch 'timeline-pwa'
75a115f Correlate tool timeline events by call id
4223349 Merge branch 'timeline-host'
66594c3 Add headless timeline scenario and adapt processes scenario
78ce291 Route turn_event frames and script a timeline mock sequence
0e92f41 Render the running turn as a lash-style timeline
```

## Git Status At Report Draft

```text
?? e2e/normal-user-coverage-report.md
```
