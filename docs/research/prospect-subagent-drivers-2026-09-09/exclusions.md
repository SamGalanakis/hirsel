# Scope and exclusion map

This records the initial research scope. The authorized implementation follow-up at the end supersedes the initial read-only and no-ticket boundaries.

Date: 2026-09-09. Examine the current working tree at /workspace/code/hirsel (HEAD 2df5888b8838ec2ea3c3cdadf4707b03bc4c4897 plus pre-existing changes), not the older clean implementation in the research worktree. Research artifacts and working-convention edits live in /workspace/code/hirsel-subagent-prospect. Do not change implementation or live processes.

## Framing and aspects

Hirsel is a personal orchestration product with seven Rust workspace crates, a Rust host embedding Lash and SQLite, native Codex/Claude drivers, and web/native clients. A globally aware Agent coordinates durable Threads and delegates execution to external CLIs. Must preserve accurate command/result state, bounded owned process lifetimes, and delivery of terminal results to the addressed work without blocking the Owner conversation. Compare native protocol control, lifecycle/cleanup, streaming/results, and provider contract verification.

References explicitly requested: get-bb/bb (/tmp/ref-bb, 4ed2743219e8a6ecd7d2c2535c68865dcf821b20) and pingdotgg/t3code (/tmp/ref-t3code, e16b8b059c9f5ff6dfed1addecffb831c6aee043). Both cover scoped aspects; no additional reference needed. Previous prospect covered product/work identity, not provider transport lifecycle; this round covers that previously omitted ground.

## Tracker

User explicitly confirmed GitHub issues are Hirsel's task tracker; this is now documented in the research branch CONTRIBUTING.md and agent entrypoints. Live gh issue list --repo SamGalanakis/hirsel --state open --limit 1000 returned [] today: no open issues to exclude. Open PR #1, Add diff-hygiene and secret-scan merge gates, is excluded. Preliminary Linear lookup was done before clarification and is not the exclusion authority.

## Settled decisions and existing work

- Keep native Rust SubagentDriver seam and native headless CLI protocols (ADR 0003). Do not propose ACP, SDK/runtime replacement, or moving Claude subscription credentials into Hirsel.
- Keep Lash/RLM, one host, SQLite (0001/0002). No T3 Effect/event-sourcing rewrite.
- Abandoned delegated work is not mechanically retried/restarted (0004). Any provider resume suggestion must be an explicit Agent operation, not automatic recovery.
- Wake policy belongs to prompts; ephemeral fork triages background results (0005/0015). Do not replace with hardcoded wake/retry rules.
- Model/variant catalog validation and generated tool schemas already exist; do not rediscover them as missing.
- Current PRODUCT.md and ADR 0016 supersede old Task/Event/global-conversation descriptions: durable Threads own messages, turns, activity; process lifecycle is separate. Prior prospect docs/research/prospect-task-model-2026-09-09/report.md led to this adopted decision. Exclude product domain redesign and settled Thread ownership.
- Explicit global artifacts (0017) are settled and unrelated.
- Shared process-group kill on retirement, bounded process progress, terminal summaries, fake-driver fixtures and real-subagent e2e already exist: inspect exact guarantees before claiming absent coverage.
- Keep full-auto permission mode as current explicit decision. Do not recommend approval UI as a correctness fix.
- Research findings only; decisions with design tradeoffs must be presented before implementation. No tickets requested yet.

## ADR inventory

- docs/adr/0001-single-host-binary-sqlite-no-restate.md: Single host binary with sqlite persistence, no Restate
- docs/adr/0002-agent-runs-in-rlm-mode.md: The Agent runs in RLM mode, not standard tool-calling
- docs/adr/0003-native-subagent-drivers-not-acp.md: Drive Sub-agents over native headless protocols, not ACP
- docs/adr/0004-no-task-abstraction.md: No task abstraction; recovery is Agent judgment, not machinery
- docs/adr/0005-agent-manages-its-own-wakes.md: The Agent manages its own wakes; the host installs no wake policy
- docs/adr/0006-wss-first-iroh-later-transport-agnostic-protocol.md: Transport-agnostic client protocol; WSS first, iroh as milestone two
- docs/adr/0007-chat-output-is-turn-output.md: Chat output is turn output, not a tool call
- docs/adr/0008-side-chats.md: Side threads: seeded forks that produce the Owner's reply
- docs/adr/0009-reply-resolves-pings.md: Task settlement is action-authoritative
- docs/adr/0010-native-mobile-on-rust-core.md: Native mobile apps on a shared Rust client core; Android first; web becomes the desktop client
- docs/adr/0011-device-pairing-over-iroh.md: Device pairing and per-device auth over iroh
- docs/adr/0012-typed-event-queue-and-scroller-home.md: Typed event queue and the vertical scroller home
- docs/adr/0013-constrained-json-ui-substrate.md: Constrained JSON UI as the event/view substrate
- docs/adr/0014-in-tree-plugin-folders.md: In-tree plugin folders
- docs/adr/0015-ephemeral-fork-triage-and-model-topology.md: Ephemeral fork triage for non-owner wakes; the resident/fork/advisor model topology
- docs/adr/0016-threads-own-conversation.md: Threads own conversation and work
- docs/adr/0017-explicit-global-artifacts.md: 0017: Explicit global artifacts with conversation references

All readers/reviewers: skip covered ground. If a finding extends planned or settled work, name that item and state only the delta. Read applicable current docs for supersession details.

## Authorized implementation follow-up

After the verified report, the Owner approved all fixes and selected current-run steering. GitHub issues #2, #3, #4, #5 and #6 now cover controls, root identity, lifetime, steering and delivery respectively. They are this round's implementation scope, and exclusions for later prospect rounds.

Issue #7 separately tracks the Owner-requested local skills feature, also implemented in this round. Final integration and validation are recorded in report.md.
