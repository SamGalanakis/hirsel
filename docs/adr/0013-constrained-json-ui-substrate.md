# Constrained JSON UI as the event/view substrate

> **Current product clarification (2026-07-23):** The constrained JSON substrate remains authoritative. Its primary visible role is now the adaptive Task instrument, not a card inside an event queue. Structured actions may transition the same open Task through multiple generated stages before a later action settles it.

Settles how events ([ADR-0012]) and Canvas views render, and revises the deferred UI-templates
direction in `SCOPE.md`. Realizes `docs/product-direction.md` §4.

## Context

Every event card and every Canvas view is machine-authored UI: the Agent (or a sub-agent, or a
scheduled job) decides what to show. `SCOPE.md` previously pointed the generative-UI tier at "HTML
templates in sandboxed iframes with a JSON-RPC-over-postMessage bridge." That direction is heavy
(iframe sandboxing, a bridge, cross-client parity) and — worse — lets generated markup go off-brand:
nothing stops an agent-authored page from a gradient, a second accent, or a non-hairline fill.

hirsel already ships a **constrained** generative-UI tier for Canvas: `app/src/views/` (a closed
component catalog + `tokens.ts` that maps semantic tones/states onto design tokens, forbidding
hard-coded colors) and `ViewRenderer.tsx`, with `views_show` validating against the catalog at the
tool boundary. The event queue needs exactly this — but interactive, and at card scale.

## Decision

**Every event and view carries a `ui`: a validated JSON component tree, rendered by one renderer onto
DESIGN.md tokens.** The constrained catalog — not agent-authored HTML — is the substrate.

**One substrate, three regions.** The same vocabulary renders at card size in the queue, at panel size
in a Side Chat drill-in, and at full size in Canvas. One renderer for phone, desktop, and Android
(the native core, [ADR-0010], renders the same tree).

**Vocabulary v1** (extend the existing `views` catalog): `eyebrow`, `heading`, `text`, `keyValue`,
`badge`, `status`, `divider`, `optionList` (letter-keyed `options[]` with a `recommended` flag),
`viewSlot` (an embedded block — a diff, a small table — the "accompanying dynamic UI" of a judgment),
plus **interactive** nodes `field` and `submit`. The nodes carry only **semantic tokens** (`tone`,
`state`, `recommended`, `boundary`) — never a hex, a class, or a size. The renderer alone owns the
palette and the type scale.

**Taste-safe by construction.** Because the vocabulary cannot express an arbitrary color, a glow, a
second accent, or type above the 16px ceiling, machine-generated UI is on-brand *by construction, not
by review* — DESIGN.md is enforced in the vocabulary itself. This is the render-layer twin of the
decision-time "taste boundary" ([ADR-0012]): the machine cannot violate taste in what it shows any more
than in what it commits.

**Interactive: interaction-back.** A tap on an `optionList` option, or a `submit` of `field` values,
posts a structured `event_action` back to the producer's scope (a judgment's option resolves it and
may carry a "record rule" payload; a scheduled job's control posts to that job). This is the one
genuinely new protocol op beyond the event envelope; it mirrors the existing `view_event` path.

**Blessed templates vs. free composition.** The hero types are **blessed parameterized templates** —
the judgment card is structurally identical every time (eyebrow · fork · optionList · viewSlot) so the
decide-in-3s reflex holds and the Agent fills only data slots. **Free composition** from the catalog is
reserved for the long tail (a scheduled job's bespoke digest, a one-off review). Both modes; that
split. Cards stay **minimal chrome** ([ADR-0012]): the vocabulary is spent on the event, not on
metadata rows.

**Safety rails.** Validate at the tool boundary (reject malformed trees, retry) as `views_show`
already does; **version the vocabulary**; and **degrade an unknown node to a fallback chip — never
throw, never blank, never lose content.** Text is set as text (no HTML injection); the only transform
is `` `backtick` `` → monospace, so machine tokens get mono and prose never can (Monospace-Earns-It,
enforced).

## Consequences

- The `views` catalog gains interactive nodes and the `event_action` round-trip; `ViewRenderer` (or a
  shared successor) renders both events and views.
- Producers emit JSON, not HTML — more reliably LLM-authorable, inherently sandbox-safe (no arbitrary
  markup/script), and identically renderable across clients.
- The blast radius of a new component is one catalog + one renderer entry per client, not an open HTML
  surface; the trade is expressiveness bounded by the catalog (accepted — the bound *is* the taste
  guarantee).
- A vocabulary version skew degrades gracefully rather than breaking a card.

## Status

Accepted. Supersedes the "HTML-in-iframes" generative-UI direction noted in `SCOPE.md`. Built together
with [ADR-0012]; extends the Canvas catalog already in `app/src/views/`.

[ADR-0010]: 0010-native-mobile-on-rust-core.md
[ADR-0012]: 0012-typed-event-queue-and-scroller-home.md
