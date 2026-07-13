# Product Direction: hirsel as a judgment-leverage system

Status: north-star, agreed 2026-07 (supersedes the earlier "conversation-first" framing). This is the
*why* and the *shape*; `SCOPE.md` remains the v1-vs-deferred ledger and the ADRs remain the record of
individual decisions. Where this doc and older text disagree (notably: Chat is no longer the home;
the generative-UI tier is constrained JSON, not HTML-in-iframes), this doc wins and the ADRs will be
updated to match.

## 1. Thesis

**The scarce resource is Sam's technical judgment and taste. hirsel exists to maximize its leverage.**

Two failure modes bound the design:

- **herdr (the incumbent)** makes Sam the *operator* — he manually drives N coding-agent sessions,
  spending judgment-hours on dispatch, transcript-reading, and context reconstruction. It wastes the
  scarce resource.
- **"vibecoding"** makes Sam an *absentee stakeholder* who sets outcomes and rubber-stamps. It
  *discards* the scarce resource — the machine makes the taste calls, badly.

hirsel is neither. Sam is **the principal whose decisions and taste are the product.** The metric the
system optimizes is *high-quality technical decisions routed through Sam per hour, at maximum leverage
and zero waste.* Autonomy runs **between** his decisions, never instead of them.

## 2. The home is one typed event queue

Everything the system wants *from* Sam or wants to *tell* him is a typed **event** in a single inbox.
The queue is the primary surface; Chat is demoted to a drill-in reached *from* an event.

- **kinds** (open set): `judgment` (the hero — needs a decision), `summary` (a digest, no decision),
  `info` (a quiet FYI/notification), … more as they earn their keep.
- **producers**: the main Agent, sub-agents hitting their own taste boundary, and — the powerful part
  — **lash jobs Sam schedules.** The "morning brief" is not a bespoke screen; it is a scheduled
  process that emits a `summary` event at 7am. "Ping me if CI goes red" is a monitor emitting a
  `judgment`/`info`. Sam composes his own recurring intelligence and it all lands in one queue.

**The make-or-break invariant — the interrupt-vs-accrue axis.** An "anything can emit" queue dies if
info drowns judgment. So the axis is baked into the type: **judgments lead the feed and may push
(FCM); summaries/info accrue quietly, batch, and auto-read; the one red on the surface is the
"needs you" count.** The feed splits into **Needs you** and **For your awareness** sections. This also
simplifies the Agent's channel-discipline (`prompts/agent.md`): it shrinks from a fuzzy Ping-vs-Chat
call to "pick the event kind."

## 3. The judgment (hero type) at full fidelity

A judgment is minted when the fleet hits the **taste boundary** — anything that encodes a technical /
architectural / UX commitment. The fleet **stops cold** and surfaces the call *before* it is
load-bearing. That halt, made visible ("taste boundary — fleet stopped"), is the **anti-vibecoding
guarantee**: the machine never makes a taste call silently.

Each judgment carries:

- the **fork** as a one-line question;
- **2–3 prefab options**, letter-keyed, one **recommended** with a one-line *why* (and, later, the
  standing decision it's derived from);
- an **"unblocks"** line naming the paused work.

**Minimal chrome.** The card is the event, not a telemetry panel — no wait-time, no cost, no
turn counts, no ambient run-detail. Space is spent on the decision (fork, options, accompanying UI).
Fleet run-detail lives in a separate read-only surface, never on the card.

Three engagement depths, Sam's choice per item:

1. **Decide inline** — one tap on the recommendation (the default gesture).
2. **Drill in to Chat** — a scoped Side Chat when the memo isn't enough.
3. **Accompanying dynamic UI** — a diff, a table, an interactive chooser rendered on the card itself.

Deciding resolves the event and (in the background) writes to the taste store (§5).

## 4. Events are constrained dynamic JSON UI (the keystone)

Each event carries a **`ui`: a validated JSON component tree**. A small renderer walks it → DOM,
applying DESIGN.md tokens. hirsel already has the bones: `app/src/views/` (the catalog + tokens),
`ViewRenderer.tsx`, the **colors-forbidden** discipline, and `views_show`'s validation-at-the-tool-
boundary. The queue points that Canvas substrate at cards.

Why this is the keystone:

- **One substrate, three regions.** Judgment card, summary card, full canvas view — the same
  vocabulary rendered at card size in the queue vs. full size in the canvas. One renderer for phone,
  desktop, and Android.
- **Taste-safe by construction.** The vocabulary *cannot express* an arbitrary color, a glow, or a
  non-hairline fill — so machine-generated UI is on-brand by construction, not by review. The taste
  boundary reappears at the **render layer**: Sam's DESIGN.md is enforced in the vocabulary itself.
- **This revises the deferred UI direction.** `SCOPE.md` previously pointed the generative-UI tier at
  "HTML templates in sandboxed iframes." We choose **constrained JSON UI** instead: more reliably
  LLM-authorable, inherently sandbox-safe, render-consistent across clients, and taste-enforcing —
  none of which HTML-in-iframes gives.
- **Interactive.** Taps and field submits post back a structured **event-action** to the producer's
  scope (a judgment's option-tap resolves it; a custom job's control posts to that job). This
  interaction-back path is the one genuinely new protocol piece.

**Templates vs. free composition — the one hard rule.** The hero types are **blessed parameterized
templates**: the judgment card is pixel-identical every time so the decide-in-3s reflex holds and the
Agent just fills the data slots. **Free composition** from the catalog is reserved for the long tail
(a scheduled job's bespoke digest, a one-off review). Both modes; that split. Validate at the tool
boundary (reject malformed, retry); version the vocabulary; **an unknown node degrades to a fallback
chip — never breaks, never loses content.**

## 5. The taste store (deferred, runs in the background)

Every judgment Sam decides quietly writes a **standing decision** — no UI cost now. Seeded from his
`DESIGN.md`, `CLAUDE.md`, and model-taste table (already this, done by hand). Later it surfaces as the
**codex**: the fleet *cites* standing decisions, *auto-decides* recurring forks ("handled on your
preferences — override?"), and *litigates* them (files an amendment with evidence + a diff when
reality contradicts a rule). That compounding layer — a decision's leverage going from 1 to 41× — is
the endgame, grown in once decision volume exists, not built up front.

## 6. Observable, not operable

Sub-agents and the fleet are **visible** (ambient, read-only, git/space-aware state) but never typed
into. No jump-into-session, no per-session permission UI. Steering is **conversational** (intent in
the composer) and, later, **editing standing decisions** — never terminal-driving. This is a
deliberate rejection of herdr's operator model.

## 7. Mapping onto the existing stack (this is barely a rebuild)

| Concept | Existing primitive | Delta |
| --- | --- | --- |
| Event | `Ping` (`hirsel-proto`) | add `kind`, `ui`, `options`, `source`; `requires_response` Ping *is* the judgment type |
| Prefab options | `quick_replies` | rename/extend; letter-keyed, recommended flag |
| Dynamic UI / renderer | Canvas / `ViewRenderer.tsx` / `views/tokens.ts` | add interactive nodes (`option`, `field`, `submit`) + interaction-back |
| Chat drill-in | Side Chats (ADR-0008) | open from a judgment card |
| Scheduled producers | timers + monitors | generalize to "a scheduled lash job emits events" |
| Push | FCM gateway (ADR-0009) | judgments push; info/summary don't |
| Taste store | — (new) | seeded from DESIGN.md / CLAUDE.md; flat in v1 |

## 8. v1 build slice

- **proto:** `Event { id, kind, source, name, description, ui, requires_response, state, … }`; the
  constrained JSON-UI vocabulary v1 (extend the `views` catalog + interactive nodes); an
  `event_action` inbound op (interaction-back); an `event_upsert` broadcast.
- **host:** generalize `Ping` → `Event` with `kind`; a blessed **judgment template** the Agent fills;
  a scheduled-job producer path (a lash program + a schedule → emits events); write-through to a flat
  taste store on decide.
- **PWA:** the home *is* the event feed (Needs-you / For-your-awareness); the JSON-UI renderer
  extended to render cards; the judgment card as a blessed template; interaction-back wired; Side-Chat
  drill-in from a judgment.
- **Deferred within this direction:** the codex surface + auto-decide/amendment loop (§5);
  git-aware Spaces as a separate track; free-composition producer UIs beyond the blessed templates.

## 9. The home interaction (decided)

The home is a **Tinder-like vertical event scroller**: full-viewport, one event per screen, flicked
through and cleared. Not a sectioned list — a focused pager where each event owns the whole screen,
so decide-in-3s becomes decide-with-full-attention, thumb-driven, walk-friendly. Settled interaction
rules:

- **Vertical paging / scroll-snap**, one `100svh` event per screen; flick up = next.
- **Order is the priority axis** (the interrupt-vs-accrue invariant expressed as ordering): blocking
  judgments first → other needs-you judgments → the awareness tail (`summary`/`info`), which
  **auto-marks-read as it scrolls past**. A blocking judgment can jump the queue and push.
- **Buttons carry the real choice** — a multi-option judgment does not reduce to a binary swipe.
  **Swipes are accelerators**: up = next, right = accept the recommendation, left = snooze to the
  tail.
- **Decide → confirm → auto-advance**, with an undo; the "N need you" pager count is the surface's
  one red.
- A pure scroller loses at-a-glance, so a slim **pager + a peek-to-overview** (a compact list of the
  whole queue you can jump from) keeps it from being a tunnel.
- An **inbox-zero "queue clear"** end state — the peak-end reward of clearing the queue.

The scroller is the *presentation shell*; the cards inside it are unchanged constrained-JSON-UI events
(§4). A scheduled "digest" `summary` event can still ride at the top as the morning brief without
being a bespoke surface.

## Provenance

Narrowed over a design conversation and three independent model spikes (the "Decisions", "Codex", and
"Operation" directions) plus a constrained-JSON-UI renderer spike. The three spikes converged, unasked,
on demoting Chat and on the decision-memo atom; this doc is their synthesis under the judgment-leverage
thesis, with the event-queue generalization and the JSON-UI keystone added on top.
