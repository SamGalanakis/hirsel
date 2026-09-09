# Scope and exclusion map

Research date: 2026-09-09. Read-only product/domain-model prospect; no implementation, runtime restart, task mutation, or ticket creation authorized by this research request.

## Repo framing

Hirsel is a single-owner orchestration product with a Rust host, shared Rust/native-client support and SolidJS web client (seven workspace crates). One globally aware Agent presents a flat inventory of durable Tasks with generated instruments and anchored conversation. The important invariants are durable task identity/visibility, explicit lifecycle transitions, and one global conversational agent across task focus changes.

## Aspects and reference

Use only pingdotgg/t3code, locally cloned at /tmp/ref-t3code, revision 6c583620ff7ad3235b135af7107c0543467eecfa. Compare (1) durable work identity versus emitted events, (2) activity/attention versus lifecycle/list membership, (3) creation-to-snapshot/live-client contract tests and commands. Do not expand into generic code quality, providers, deployment, or security. User supplied T3 Code; it covers all scoped aspects, so no additional reference is necessary.

## Open planned work

- GitHub SamGalanakis/hirsel: gh issue list --state open --limit 1000 returned [] (2026-09-09).
- Open PR #1: Add diff-hygiene and secret-scan merge gates. Out of scope; do not rediscover this.
- Linear project search hirsel returned no projects. Full text issue search hirsel returned seven matches, all Done, hasNextPage=false: FIG-144 Provider credential lifecycle; FIG-1599 Tool-intent record identity over-scopes; FIG-1562 heap closures outlive their program; FIG-1556 store-compatibility preflight; FIG-1494 attachment GC model design; FIG-1573 active-turn input orphan recovery; FIG-234 exec-internal failure semantics.
- Limitation: Linear exclusion is scoped by Hirsel text/project discovery, not an audit of every issue in the shared Figments/Lash workspace. No Hirsel-specific open task/event redesign ticket was found.

## Settled ground: do not re-propose as new findings

- Current authority: CONTEXT.md, PRODUCT.md, DESIGN.md, docs/product-direction.md supersede historical UI directions. Task is already the stated durable visible work object; proposing Tasks as a product concept is not new. The delta can be making storage, tools, and visibility obey it.
- One Owner, one global Agent and composer; flat Task inventory. Focus changes subject, not agent/session. No per-task conversation destination, nested task tree, feed/dashboard or separate notification inbox.
- Task identity and Anchor survive generated instrument recomposition. One task may move through multiple instrument stages.
- Settlement is open/done and explicit. Conversation/read alone must not settle a Task; stage actions may continue rather than complete work. Changes beyond this are design decisions, not implementation recommendations to sneak in.
- Semantic constrained JSON instrument; renderer owns layout, safety, accessibility and fallback. Do not recommend arbitrary HTML/apps.
- Visible Task does not mean host workflow/retry/task-spec abstraction. Runtime Processes remain separate execution mechanisms; retry/recovery is Agent judgment.
- Host uses embedded Lash/RLM + SQLite, native subagent drivers, turn output as conversation. Do not replace with T3 Code runtime/Effect/event-sourcing stack.
- Processes, Settings, Canvas are temporary utilities; no new destination inventory.
- Historical side-chat code is compatibility only; its deletion is already governed by ADR-0008's supported-client sunset criterion.
- Existing actual frontend selector intentionally excludes every info Event (app/src/store/selectors.ts); current bug is known: request buy groceries => events_notify => persisted info event #5 => absent task index. Do not sell rediscovery as a novel finding; explore the missing invariant and model alternatives.
- Prior research docs/research/generative-ui-2026-07.md and design exploration docs/effortless-orchestration/ already led to current Task Margins and JSON UI; do not re-open the visual world.

## Complete ADR/source inventory for reviewers

Read area-relevant docs directly, especially supersession notices. All current ADRs are listed below; recommendations must name any ruling they propose changing.

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
