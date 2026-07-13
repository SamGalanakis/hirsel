# Typed event queue and the vertical scroller home

Generalizes [ADR-0009]'s Pings into a single typed event stream and makes that stream — not Chat — the
home. Realizes `docs/product-direction.md`.

## Context

hirsel's product thesis narrowed to: **the scarce resource is Sam's technical judgment and taste; the
system exists to maximize its leverage** — not by making him the operator (herdr) and not by taking
the taste calls itself (vibecoding), but by routing exactly the decisions that need him, well-framed,
and doing the rest autonomously.

Today the owner-facing surfaces are split: **Chat** (the home; [ADR-0007]) and **Pings** (agent→owner
questions with a name/description/`requires_response`/`quick_replies`, resolved by reply per
[ADR-0009]). A Ping is really "a thing the system needs from or wants to tell the owner" — which is
also what a completion summary, a scheduled digest, and a monitor alert are. Keeping Chat as the home
optimizes for presence and narration; the thesis wants a surface optimized for *decisions per hour and
zero context reconstruction*.

## Decision

**One typed event queue is the home. Chat is demoted to a drill-in.**

**Event model.** Generalize `Ping` into `Event` (`hirsel-proto`): add a `kind` and a `source`, and
carry a constrained-JSON-UI `ui` payload ([ADR-0013]) instead of only markdown.

- `kind` (open set): `judgment` (needs a decision — the hero type), `summary` (a digest, no decision),
  `info` (a quiet notification). `requires_response` Pings map to `judgment`; non-response Pings map
  to `info`/`summary`.
- `source`: who produced it — the main Agent, a sub-agent at its own taste boundary, or a
  **scheduled lash job**. The "morning brief" is not a bespoke screen; it is a scheduled process that
  emits a `summary` event. This generalizes timers/monitors ([ADR-0005]) into event producers.
- Lifecycle stays open/resolved with read state, per [ADR-0009]. A `judgment` resolves when decided;
  `summary`/`info` are read/dismissed (auto-read on pass).

**The interrupt-vs-accrue invariant.** An "anything can emit" queue dies if info drowns judgment, so
the axis is baked into the type and expressed as **ordering**: blocking judgments first → other
needs-you judgments → the awareness tail (`summary`/`info`), which auto-marks-read as it is passed.
Judgments may push (FCM, [ADR-0009]); awareness does not. The one red on the surface is the
"needs you" count. The Agent's channel-discipline (`prompts/agent.md`) shrinks from a fuzzy
Ping-vs-Chat call to "pick the event kind."

**The home is a vertical event scroller.** Full-viewport, one event per screen, scroll-snap paging,
flicked through and cleared (TikTok paging + Superhuman decisiveness). Not a sectioned list — a
focused pager so decide-in-3s becomes decide-with-full-attention, thumb-driven, walk-friendly.

- **Buttons carry the choice** (a multi-option judgment does not reduce to a binary swipe); **swipes
  accelerate**: up = next, right = accept the recommendation, left = snooze to the tail.
- Decide → confirm → auto-advance, with undo.
- A slim pager + a peek-to-overview keeps it from being a tunnel; an inbox-zero "queue clear" end
  state is the peak-end reward.

**Cards are minimal chrome.** The card is the event, not a telemetry panel. No wait-time, no cost, no
turn counts, no ambient run-detail — space is spent on the decision (the fork, the options, the
accompanying UI), never on metadata. Observability of the fleet lives in a separate read-only surface,
not on the card.

**A judgment can write a standing decision.** Deciding may also record a standing rule (a "record
rule" affordance) that seeds the deferred taste store — the compounding "codex" layer
(`docs/product-direction.md` §5) grows in from real decisions, and is otherwise out of scope here.

## Consequences

- `Ping` becomes `Event` across proto/host/PWA; `resolve_ping`/`read_ping`/`ping_upsert` generalize to
  event ops (with back-compat aliases only as long as needed). The reopen op (Done-as-toggle) carries
  over.
- The PWA home is rebuilt as the scroller; `ChatView` becomes a drill-in reached from an event, not
  the root. Side Chats ([ADR-0008]) are the chat drill-in from a judgment.
- Producers proliferate (agent, sub-agents, scheduled jobs) — the interrupt-vs-accrue ordering and
  easy muting/batching of scheduled producers are load-bearing against noise.
- Fleet observability moves off the card into its own read-only surface (sub-agents remain
  observable, not operable — no jump-in, no per-session permission UI).
- Cutover is built on a `feat/event-queue` branch, proven and design-gated before the live system
  moves.

## Status

Accepted. Supersedes the Chat-as-home framing of [ADR-0007] (Chat remains the drill-in and turn-output
surface; it is no longer the root). Built with [ADR-0013] (the JSON-UI substrate the cards render
through).

[ADR-0005]: 0005-agent-manages-its-own-wakes.md
[ADR-0007]: 0007-chat-output-is-turn-output.md
[ADR-0008]: 0008-side-chats.md
[ADR-0009]: 0009-reply-resolves-pings.md
[ADR-0013]: 0013-constrained-json-ui-substrate.md
