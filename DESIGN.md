---
name: hirsel
description: Task Margins — one globally aware orchestrator across a quiet field of task-shaped interfaces.
colors:
  bg-base: "oklch(0.17 0.012 220)"
  surface: "oklch(0.205 0.014 220)"
  surface-quiet: "oklch(0.235 0.016 220)"
  fg-primary: "oklch(0.94 0.01 165)"
  fg-muted: "oklch(0.72 0.025 175)"
  accent-primary: "oklch(0.79 0.105 158)"
  accent-ring: "oklch(0.84 0.10 158)"
  border-hairline: "oklch(0.90 0.02 180 / 11%)"
  status-attention: "oklch(0.78 0.13 78)"
  status-danger: "oklch(0.70 0.17 32)"
typography:
  task-focus:
    fontFamily: '"Inter Variable", "Inter", ui-sans-serif, system-ui, sans-serif'
    fontSize: "clamp(1.25rem, 2.2vw, 1.75rem)"
    fontWeight: 450
    lineHeight: 1.2
  body:
    fontFamily: '"Inter Variable", "Inter", ui-sans-serif, system-ui, sans-serif'
    fontSize: "0.875rem"
    fontWeight: 400
    lineHeight: 1.625
  mono:
    fontFamily: '"SFMono-Regular", "Cascadia Code", "Liberation Mono", Menlo, monospace'
    fontSize: "0.72rem"
    fontWeight: 400
    lineHeight: 1.45
rounded:
  control: "14px 26px 26px 14px"
  surface: "28px"
  organic: "52% 28% 42% 58%"
  pill: "9999px"
---

# Design System: Task Margins

## 1. Core idea

**Focusing a task changes the subject, never the interlocutor.** Hirsel is one continuous intelligence across a flat field of tasks. A task mark unfolds into whatever constrained interface the work needs—choice, boundary, range, form, diff, or status—and conversation appears as plain language in its margin. Selecting the focused task again clears focus and returns to the ambient field.

Task Margins has no nested navigation and no separate conversation object. Host compatibility structures stay at the protocol boundary and never enter the product vocabulary.

## 2. Physical scene and palette

Hirsel is used late at a desk and in quick phone glances. The resting scene is a warm blue-charcoal field, closer to oxidized steel than pure black. Regions are formed by whitespace and tonal shifts, not boxes. Mint is the sole interaction color; amber means waiting; coral is reserved for genuine blockage. Light mode is an off-white, cool-paper peer using the same relationships.

- **Canvas:** `oklch(0.17 0.012 220)` dark; `oklch(0.975 0.008 190)` light.
- **Quiet field:** one tonal step above the canvas, reserved for active controls and summoned utility surfaces. Conversation remains on the uninterrupted canvas.
- **Primary mint:** `oklch(0.79 0.105 158)` dark / `oklch(0.48 0.12 158)` light. Use for selected task, active choice, focus chip, and send.
- **Amber:** waiting or queued only. **Coral:** blocked or destructive only.
- Hairlines are optional boundaries for top bars, overlays, and focus. If whitespace can separate two regions, omit the line.

## 3. Typography

Inter remains the workhorse. The task name is quiet context; the generated question is the instrument's semantic focus.

- **Focused task:** 20–28px, weight 450, compact but never heroic.
- **Generated question:** 20–24px, weight 500. It must visibly lead the task name.
- **Body and conversation:** 14–16px, relaxed leading, no chat-bubble compression.
- **Meta:** 11–12px. Monospace only for machine tokens, ids, timings, commands, and shortcuts.
- Avoid oversized bold section headings, all-caps navigation, and decorative monospace.

## 4. Shell and interaction

### Task field

- Desktop: a quiet task index occupies the left margin; the selected task unfolds through the remaining field. It is a list of names and literal statuses, not cards.
- Phone: tasks become a horizontal, masked strip. The selected task owns the screen below it.
- Selecting a task focuses it; selecting the focused task again clears focus.
- Focus is shown by the unfolded instrument, task-row state, and the composer's tonal shift—never by a label inside the composer, by moving or resizing it, or by color alone.
- On load the most-needing task opens focused (blocked on you → needs you → unseen → moving, newest first); with no open task the field rests ambient. Focus is chosen once per load: a task arriving later never steals it.
- Ambient is the absence of focus — the deliberate zoom-out reached with Esc. It has no title, empty-state copy, or standing mode label.

### Generated task surface

- Render the Event's constrained JSON `ui` directly into the task field. Do not wrap it in a generic card.
- Different task needs must produce visibly different instruments while retaining the same typography and tokens.
- Interaction posts through the existing `event_action` path. Decisions settle in place; a quiet undo remains reachable.
- Ordinary choices and supporting data are flat rows separated by rhythm or hairlines. The standing composer is the only persistent capsule.
- Canvas views may appear inside the task when associated, or as a summoned utility when global. Canvas is not a destination.

### One column, task as pinned context card

- Every state is ONE centred column at the reading measure. Ambient is the conversation bottom-anchored on the composer; focus adds a **pinned task card** at the top of that same column, with the conversation flowing below it into the composer. Focus never splits the field into two columns and never gives the conversation a margin.
- The card is sticky at the top of the scroll container, so the task stays legible while its history is read. It sits on the canvas — its only boundary is one hairline — and it is capped at ~40dvh with its own scroll, so a task with tall generated fields can never push the conversation off screen. A content-thin task renders as the few lines it is.
- Because the card is pinned, a focused task opens where a conversation opens: at its newest line. There is no longer a subject to protect by holding the field at its top.
- This replaces the two-column focused layout (instrument left, conversation in a ~400px right margin). The reason is content density, honestly: role decided the allocation, so a two-line "session rotated" notice owned half the screen while the conversation carrying all the substance was squeezed into the margin. Allocate by content — and the conversation is primary in every state.

### Conversation

- Task conversation is plain prose following the pinned task card. No “Hirsel · task” heading, avatar, bubble, aside, card, or transcript border.
- Owner lines use stronger foreground; Hirsel lines use muted foreground. Order and language provide authorship.
- Conversation uses the **Pure Field** rule: no backdrop or enclosing silhouette. Owner lines receive one quiet hairline and slight indentation; Hirsel prose rests directly on the canvas. Collapsed tool disclosures are transparent at rest.
- The conversation follows the card in the same scroll flow at every width; phone and desktop differ only in inset.
- Earlier rows arrive just in time as the reader approaches the top. There is no “earlier messages” control or beginning marker; only a quiet loading line may appear when the reader reaches the edge before a prefetched page lands.
- With no relevant conversation, it does not render or reserve space.
- Tool timelines may unfold under the relevant Hirsel line on demand; they never create a third column.

### Composer

- One composer persists at the bottom of every state.
- The composer inherits the field: focused task messages use that task's anchor and mention; ambient messages are unanchored.
- Scope is never a separate composer mode. There is no scope chip, visible mode label, or instructional placeholder.
- The composer holds one width in every state: the reading measure it shares with the single column above it — task card and conversation alike — so its left and right edges are the same in ambient and in focus. Focus changes only its tone, to a restrained mint-local tint; ambient rests neutral. (Focus used to narrow it from the frame width to the measure. That moved the one permanently visible element — and its send target — sideways on every toggle, and left the ambient capsule wider than the conversation it sat under. Continuity of the standing element beats the width distinction.)
- The composer is an organic quiet capsule. Send is round. On phone it stays sticky at the thumb edge while task content moves behind it.

### Utilities

Home has no header bar: the field's top anchor is its content. One quiet floating `⋯` is the only standing chrome, and every utility — Processes, settings, model choice, raw timelines, connection diagnostics — is reachable from it or a keyboard command. Processes leads that menu and carries a running count, so active work stays directly inspectable without a bar. Connection state is visible only when abnormal; a healthy socket shows nothing. Every utility appears as a temporary sheet or inspector, keeps its own pane header, and closing it returns to the same focus state.

## 5. Motion and responsiveness

- A task unfolds with one 180–260ms fade/translate continuity animation. Re-selection never bounces or zooms.
- Clearing focus uses that same animation, in the same direction: focus and ambient are one surface changing subject, not two panels trading places. Nothing else moves through the swap — the composer, the task strip and the floating `⋯` stay exactly where they are, and may only change tone.
- Wide content (tables, code, process rows) scrolls inside its own box. Nothing widens the field, and the page itself never scrolls sideways at any width.
- Generated UI updates in place. Preserve semantic position where possible.
- At under 900px, the index becomes a horizontal strip. The column itself does not change shape with width — it is one column at every viewport; only the inset and the index do.
- At under 560px, the composer remains sticky above the safe area and communicates focus through tone rather than added copy.
- On phone, the quiet task identity remains 20px; the generated JSON heading owns the full question so framing is never repeated.
- Honor `prefers-reduced-motion`; all content is fully visible without animation.

## 6. Rules

### Do

- Keep the task name, current generated instrument, and composer legible in one glance.
- Let whitespace, indentation, and typography form regions; conversation never gains a decorative backdrop.
- Keep Hirsel globally aware by routing task-scoped messages through the main conversation.
- Make task state readable without color.
- Keep keyboard focus visible and phone targets at least 44px.

### Do not

- Do not ship Feed, Chat, Side Chat, Evidence, Agent, or Canvas as primary destinations.
- Do not render conversation in bubbles, cards, named panes, or a dedicated third column.
- Do not create nested task trees, breadcrumbs, graph canvases, or thread stacks.
- Do not turn generated UI into a dashboard of cards.
- Do not use glassmorphism, neon edges, heavy shadows, aggressive grids, or repeated rectangular frames.
