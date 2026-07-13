---
name: hirsel
description: A calm terminal for one human and one long-lived agent — dark, quiet, exact.
colors:
  bg-base: "oklch(0.141 0.005 285.823)"
  surface-card: "oklch(0.21 0.006 285.885)"
  surface-raised: "oklch(0.2 0 0)"
  fg-primary: "oklch(0.985 0 0)"
  fg-muted: "oklch(0.705 0.015 286.067)"
  fg-faint: "oklch(0.708 0 0)"
  accent-primary: "oklch(0.55 0.13 264.05)"
  accent-ring: "oklch(0.62 0.14 264.05)"
  secondary: "oklch(0.274 0.006 286.033)"
  border-hairline: "oklch(1 0 0 / 10%)"
  input-hairline: "oklch(1 0 0 / 15%)"
  status-active: "oklch(0.72 0.115 221)"
  status-idle: "oklch(0.65 0 0)"
  status-success: "oklch(0.72 0.185 150)"
  status-danger: "oklch(0.704 0.191 22.216)"
  status-attention: "oklch(0.79 0.16 82)"
typography:
  title:
    fontFamily: '"Inter Variable", "Inter", ui-sans-serif, system-ui, sans-serif'
    fontSize: "1rem"
    fontWeight: 600
    lineHeight: 1.5
    letterSpacing: "0.01em"
    fontFeature: '"cv02" 1, "cv03" 1, "cv04" 1, "cv11" 1'
  body:
    fontFamily: '"Inter Variable", "Inter", ui-sans-serif, system-ui, sans-serif'
    fontSize: "0.875rem"
    fontWeight: 400
    lineHeight: 1.625
    letterSpacing: "normal"
  meta:
    fontFamily: '"Inter Variable", "Inter", ui-sans-serif, system-ui, sans-serif'
    fontSize: "0.75rem"
    fontWeight: 500
    lineHeight: 1.4
    letterSpacing: "normal"
  micro-label:
    fontFamily: '"Inter Variable", "Inter", ui-sans-serif, system-ui, sans-serif'
    fontSize: "0.68rem"
    fontWeight: 500
    lineHeight: 1.2
    letterSpacing: "0.03em"
  mono:
    fontFamily: '"SFMono-Regular", "Cascadia Code", "Liberation Mono", Menlo, monospace'
    fontSize: "0.72rem"
    fontWeight: 400
    lineHeight: 1.4
    letterSpacing: "normal"
rounded:
  sm: "6px"
  md: "8px"
  lg: "10px"
  xl: "14px"
  full: "9999px"
spacing:
  xs: "4px"
  sm: "8px"
  md: "12px"
  lg: "16px"
  xl: "24px"
components:
  bubble-owner:
    backgroundColor: "{colors.accent-primary}"
    textColor: "{colors.fg-primary}"
    rounded: "{rounded.xl}"
    padding: "8px 12px"
    typography: "{typography.body}"
  bubble-agent:
    backgroundColor: "{colors.secondary}"
    textColor: "{colors.fg-primary}"
    rounded: "{rounded.xl}"
    padding: "8px 12px"
    typography: "{typography.body}"
  button-primary:
    backgroundColor: "{colors.accent-primary}"
    textColor: "{colors.fg-primary}"
    rounded: "{rounded.md}"
    padding: "0 10px"
    height: "36px"
  button-ghost:
    backgroundColor: "transparent"
    textColor: "{colors.fg-muted}"
    rounded: "{rounded.md}"
    height: "36px"
  ping-card:
    backgroundColor: "{colors.surface-card}"
    textColor: "{colors.fg-primary}"
    rounded: "{rounded.xl}"
    padding: "12px"
  input-field:
    backgroundColor: "transparent"
    textColor: "{colors.fg-primary}"
    rounded: "{rounded.md}"
    height: "36px"
    padding: "4px 10px"
  state-chip:
    backgroundColor: "{colors.secondary}"
    textColor: "{colors.fg-muted}"
    rounded: "{rounded.full}"
    padding: "1px 6px"
    typography: "{typography.micro-label}"
---

# Design System: hirsel

## 1. Overview

**Creative North Star: "The Quiet Instrument"**

hirsel is a professional instrument, not a chat toy. It sits in the lineage of a good TUI — lash-tui, Linear, Superhuman — where confidence is carried by typography and spacing, never by decoration. The surface is dark by default — dark is the brand's resting state — but light now ships as a first-class, user-selectable peer (System / Light / Dark, defaulting to System) built from the same canonical tokens, so it must hold the same ≥90 bar in both clients. Everything reads at a glance: is anything blocked on me, what is my agent doing, and can I stay out of it. The instrument is dense but never noisy, exact rather than loud, and it earns trust by respecting attention. Its emotional register is calm competence, never cheer.

Density is a feature, not a compromise. The same information presents at phone width (a single ~560px column with shelves and full-screen sheets) and desktop width. On desktop (`rail`, ≥1100px) it becomes a persistent 3-pane shell — a left nav rail ∣ the center chat pane ∣ an always-present right context pane — that fills to a ~1600px cap and only then centers, so the width is used by real structure rather than a lonely centered column. In the 900–1099 mid-width band, where the nav rail has no room yet, a Slack-style split still opens to ~980px when a Side Chat is live. Monospace earns its place and only its place: tool names in the turn timeline, monitor commands, ids, and keyboard hints. Motion is restrained to state changes — a pulse on a live status dot, a shimmer on "Thinking…", a 200ms slide when a sheet opens — and everything honors `prefers-reduced-motion`.

This system explicitly rejects three neighbors. It is **not corporate SaaS chat** (Intercom/Zendesk): no chirpy rounded-friendly bubbles, no marketing tone, no "How can I help you today!" energy. It is **not a consumer assistant** (Siri/Alexa/the ChatGPT mobile app): no mascot personality, no over-explaining, no empty enthusiasm. It is **not a notification slot machine**: no badge spam, no red dots everywhere, no engagement-bait urgency. hirsel signals "needs you" with one restrained escalation and stays silent otherwise.

**Key Characteristics:**
- System / Light / Dark theme modes (default System), both schemes shipped from one canonical token set: dark rests on a near-black base (`oklch(0.141 …)`), light on an off-white canvas (`oklch(0.985 …)`, never pure white); hairline borders at 10% alpha do the structural work in both.
- One brand accent — a muted indigo (`oklch(0.55 0.13 264)`) — reserved for interaction and "attend to this."
- Inter (with cv-alternate feature settings) for everything human; a monospace stack for machine tokens only.
- Flat by default: separation via hairline borders and a `foreground/10` ring, not drop shadows.
- Restraint as respect: muted for everything, escalation is rare and legible without relying on hue alone.

## 2. Colors

A near-neutral dark palette with the faintest cool cast, one indigo accent, and a small, disciplined set of semantic status hues. Grays are OKLCH neutrals with a whisper of blue-violet (hue ~286) so the surface never reads as dead charcoal. **OKLCH is the source of truth** — every token is authored in OKLCH in `app/src/styles.css` and surfaced as CSS custom properties consumed through Tailwind 4's `@theme inline`. The values below describe the dark scheme (the resting state); the light scheme is a shipped peer built from the same OKLCH tokens (see *Theme modes* below).

### Theme modes (shipped, both clients)
hirsel ships **three theme modes — System / Light / Dark**, defaulting to **System** (follows the OS: `prefers-color-scheme` on web, `isSystemInDarkTheme()` on Android; no Material-You / dynamic color). The choice is persisted (web `localStorage` `hirsel.theme`; Android DataStore/prefs) and applied before first paint so there is no flash. Dark stays the brand's resting register; **light is a first-class, user-selectable peer**, not a parity afterthought, and both schemes are held to the same ≥90 bar on every surface.

The **light scheme** mirrors dark's structure: an off-white canvas `oklch(0.985 0.002 286)` (never pure white), white-paper cards `oklch(1 0 0)` layered on it, a `secondary` quiet fill a hair below canvas, and true hairline borders as dark ink at 10% alpha (`oklch(0.205 0.02 286 / 10%)`, mirroring dark's `white/10%`). The indigo primary and the status hues are darkened for WCAG-AA on the light canvas: `primary oklch(0.52 0.14 264)`, `status-active 0.52 0.12 221`, `status-success 0.52 0.17 149`, `status-attention 0.56 0.15 73`, `status-danger`/`destructive 0.55 0.22 27`, `status-idle 0.55 0 0`. These light values are canonical for **both** clients — the web `:root` and the Android `Theme.kt` light scheme render the same palette.

### Primary
- **Muted Indigo** (`oklch(0.55 0.13 264.05)`, `--primary`): The single brand and interaction accent. It carries the send button, the unread-Ping dot, the `border-l` stripe on a Ping that requires a response, the reply-quote rail in the composer, and link-style actions. Its restraint is the point: when this indigo appears, it means "this is interactive" or "this wants you." A brighter ring variant (`oklch(0.62 0.14 264.05)`, `--ring`) is used only for focus.

### Neutral
- **Base** (`oklch(0.141 0.005 285.823)`, `--background`): The app canvas. Near-black, faint cool cast.
- **Card** (`oklch(0.21 0.006 285.885)`, `--card`): Raised surfaces — Ping cards, Process rows, the composer bar, popovers. One step up from the canvas, no shadow required to separate.
- **Secondary / Muted** (`oklch(0.274 0.006 286.033)`, `--secondary` = `--muted`): Agent message bubbles, quiet chips, inset wells. The workhorse "quiet fill."
- **Foreground** (`oklch(0.985 0 0)`, `--foreground`): Primary text. Near-white; AA-plus on every surface.
- **Muted Foreground** (`oklch(0.705 0.015 286.067)`, `--muted-foreground`): Secondary text — timestamps, meta, labels, dimmed read Pings. Chosen to hold WCAG AA on the card surface; there is no gray-on-gray murk.
- **Hairline Border** (`oklch(1 0 0 / 10%)`, `--border`): The primary structural device. `white` at 10% opacity — dividers, card edges, the `foreground/10` ring on cards, rail borders. Inputs get a slightly stronger `white/15` (`--input`).

### Status (semantic; sparingly)
- **Active** (`oklch(0.72 0.115 221)`, `--status-active`): A calm cyan-blue for live work — the pulsing "agent working" dot, running tool spinners, a running Process chip, an in-progress Side Chat.
- **Success** (`oklch(0.72 0.185 150)`, `--status-success`): A muted green for confirmations only — a sent-reply check, a completed tool, "Done." Never a celebration.
- **Attention** (`oklch(0.79 0.16 82)`, `--status-attention`): A gold/amber for transient warnings — a queued message, "reconnecting…", an abandoned Process. Warns; does not alarm.
- **Danger** (`oklch(0.704 0.191 22.216)`, `--status-danger` = `--destructive`): A red reserved for the genuine "blocked on you" escalation (the one badge that carries an open requires-response Ping — the phone Tray shelf badge, and its desktop equivalent the **standing Pings rail's header badge**) and destructive confirms (Discard). It is the loudest thing in the system and appears least. Exactly one red source lives on each surface; the left nav rail carries only a *muted* Pings count (never red), so the interrupt is never doubled on desktop.
- **Idle** (`oklch(0.65 0 0)`, `--status-idle`): Pure neutral gray for dormant state.

### Named Rules
**The One Escalation Rule.** Exactly one color means "genuinely blocked on you": danger red, and only on the single Pings badge for an open requires-response Ping — the phone Tray shelf badge, or on desktop the standing Pings rail's header badge (the resting state of the right region). The left nav rail carries only a *muted* Pings count (never red), the nav's Processes badge uses a status-active *tint* chip (not a solid disc), and the nav's active item uses the muted fill — so nothing else competes with the one red. Indigo means "interactive / worth a look," amber means "transient hiccup," but red is the single interrupt. Do not multiply reds. If two things on screen are shouting, one of them is wrong.

**The Color-Is-Never-Alone Rule.** No state is conveyed by hue alone. A requires-response Ping also carries a persistent left border, bolder text, and an expanded reply input; a Done Ping also dims to 60% opacity and shows a labeled "Done" tag; a Process state also carries its literal word ("running", "failed"). Strip the color and the meaning must survive (WCAG AA, single-user-but-no-excuses).

## 3. Typography

**Body / UI Font:** Inter (Inter Variable → Inter → `ui-sans-serif`, `system-ui`), with character alternates on: `font-feature-settings: "cv02" "cv03" "cv04" "cv11"` for a cleaner single-story `a`/`g` and straighter terminals — the small tuning that makes Inter read as intentional rather than default.
**Mono Font:** `SFMono-Regular`, `Cascadia Code`, `Liberation Mono`, Menlo, monospace.

**Character:** One humanist sans doing all the human work at a tight, information-dense scale, plus a monospace that appears only where the content is literally machine text. There is no display face — hirsel has no hero, no marketing headline. The largest type on screen is a 16px app title. `text-rendering: optimizeLegibility` and `font-synthesis-weight: none` (no faux-bold; only real Inter weights render).

### Hierarchy
- **Title** (600, 1rem/16px, line-height 1.5, tracking 0.01em): The app wordmark ("hirsel"), Ping-card titles, dialog headings. The ceiling of the scale — hirsel never goes bigger.
- **Body** (400, 0.875rem/14px, line-height 1.625): Message bubbles, Ping content, Side Chat transcript, most prose. `leading-relaxed` gives dense text room to breathe.
- **Meta** (500, 0.75rem/12px): Timestamps, message footers, secondary chips, keyboard-hint emphasis.
- **Micro-label** (500, ~0.62–0.72rem, tracking 0.02–0.04em, frequently `uppercase`): Section eyebrows ("Prompt", "Probe", "Original question"), state chips, provenance chips. hirsel's densest, quietest tier — a whole vocabulary of ~0.62–0.72rem labels does the fine structural signposting.
- **Mono** (400, ~0.72rem): Tool names in the timeline, monitor commands, ids, keyboard keys. Rendered `text-foreground/90` on a faint `bg-muted` chip so machine text reads as machine text.

### Named Rules
**The Monospace-Earns-It Rule.** Monospace is for machine tokens only: tool names, commands, ids, `@name` handles, keyboard hints. Prose is never monospace, and a label is never monospace just for flavor. If a human wrote it as a sentence, it is Inter.

**The No-Display Rule.** There is no display type and no hero. 16px is the top of the scale. Confidence comes from spacing and restraint, never from a big headline. If a screen wants a 32px title to feel important, the screen is wrong.

## 4. Elevation

hirsel is **flat by default**. Depth is conveyed by tonal layering (canvas → card → secondary fill) and hairline `white/10` borders, not by drop shadows. A resting card announces itself with a one-step-lighter fill plus a `ring-1 ring-foreground/10` hairline and a barely-there `shadow-xs` — enough to lift it off the canvas without a 2014-app cast shadow. Shadows are reserved for surfaces that genuinely float above the plane: overlays, popovers, dialogs, and the floating scroll-to-end pill.

### Shadow Vocabulary
- **Resting** (`box-shadow: 0 1px 2px 0 rgb(0 0 0 / 0.05)` — Tailwind `shadow-xs`): Cards, buttons, inputs. Structural, almost subliminal; the hairline ring does most of the separating.
- **Floating** (`shadow-sm`): The floating "scroll to end" pill over the transcript.
- **Overlay** (`shadow-lg`): Genuinely-above-the-plane surfaces only — the expanded Tray panel, dropdown menus, the discard dialog. This is the one place a real shadow is allowed, because the element is literally over other content.

### Named Rules
**The Hairline-First Rule.** Reach for a `white/10` border before a shadow. Separation between siblings on the same plane (list rows, the composer against the transcript, a rail against Chat) is always a 1px hairline, never a shadow. A shadow means "this floats above everything," and most things do not float.

## 5. Components

### Message Bubbles (signature)
- **Shape:** Gently rounded (`rounded-xl`, 14px), `px-3 py-2`, `text-sm leading-relaxed`, capped at `max-w-[80%]`.
- **Owner:** Filled indigo (`--primary`) with near-white text, right-aligned (`align=end`).
- **Agent:** Muted secondary fill (`--muted`), left-aligned. The asymmetry (who is filled vs. quiet) does the speaker labeling — no avatars in the main thread.
- **Footer:** A `text-[0.68rem]` meta row: timestamp, a hover-revealed copy button, and status chips (queued/amber, failed/red, "sending…" italic, "worked out in a side chat"/muted provenance). A finished agent turn hangs a collapsed "turn details" chip beneath its bubble.
- **Do not** make these read as friendly SaaS chat bubbles — no tails, no drop shadows, no pastel. The rounding is calm, not cute.

### Turn Timeline (signature)
- A lash-CLI-style vertical list on a `border-l border-border/60` rail (`pl-3`): prose blocks (muted, so a live turn reads as provisional) interleaved with tool rows and collapsed reasoning, in exact seq order.
- **Tool row:** a leading state glyph (cyan spinner while running / green check / red cross), then the **tool name in mono** (`font-mono text-[0.72rem] text-foreground/90`), then a truncated host result summary in muted text.
- **Reasoning:** a thin, dim, collapsed "reasoning" row that expands to dim italic text — deliberately quieter than prose, never a headline.

### Ping Card
- **Shape / Surface:** `Card size="sm"` — `bg-card`, `rounded-xl`, `ring-1 ring-foreground/10`, `mx-3 px-3 py-3`.
- **Requires-response:** a `border-l-2 border-l-primary` indigo stripe; otherwise the left border is transparent (reserving the width so nothing shifts).
- **Unread:** an `oklch(…)` `bg-primary` dot (`size-2`) plus full-strength `font-medium` foreground text — the "bold email" look. Read cards drop to `text-muted-foreground`.
- **Done:** the whole card dims to `opacity-60` and shows a labeled "Done" tag with a green check. Non-destructive; the card stays findable.
- **Actions:** an effort ladder — Quick Reply buttons, an inline Reply input, then a "Discuss" / "in progress · resume" Side Chat link. Overflow lives behind a `⋯` menu; destructive-adjacent actions are labeled, never icon-only.

### Buttons
- **Shape:** `rounded-md` (8px) default; icon and send buttons are `rounded-full`. Sizes run `xs`/`sm`/`default`(h-9)/`lg` plus a matching icon set.
- **Primary:** filled indigo (`--primary`), hover `primary/80`. Presses nudge down 1px (`active:translate-y-px`) for a tactile-but-quiet click.
- **Ghost / Secondary / Outline:** ghost is transparent → `hover:bg-muted`; secondary is the `--secondary` fill; outline is a `--border` stroke over `input/30`. These carry almost all non-primary actions.
- **Link:** indigo text, underline on hover — the effort-ladder verbs ("Reply", "Discuss") and other low-commitment actions.
- **Focus:** `focus-visible` shows a 3px `ring-ring/50` plus a border shift. Every keyboard flow has a visible focus ring — non-negotiable.

### Inputs / Composer
- **Style:** transparent fill over a `--input` (`white/15`) hairline, `rounded-md`, `shadow-xs`, `text-base md:text-sm`. Textareas auto-grow to a `max-h-28` cap and never resize the page.
- **Focus:** border shifts to `--ring` with a 3px `ring-ring/50` glow; `aria-invalid` swaps to a destructive ring.
- **Composer chrome:** a `border-t bg-card` bar with a paperclip, the textarea, a round send button, and — on fine-pointer devices only — a `text-[0.66rem]` keyboard-hint row (`Enter send · Shift+Enter newline · Tab queue · Esc stop`). Phone keeps Enter as newline.

### State / Status Chips
- Small `rounded-full` pills, `text-[0.62rem]` `font-medium` with a tint-on-semantic pattern: `bg-status-active/15 text-status-active` for running (plus a pulsing dot), `bg-status-danger/15` for failed, `bg-status-attention/15` for abandoned, `bg-muted text-muted-foreground` for quiet/done. The tinted-fill-plus-colored-text pattern keeps them legible and un-loud.

### Tray Shelf & Overlay (signature)
- **Collapsed:** a slim ~40px `border-t bg-card` shelf pinned above the composer, showing the Ping icon, a count badge (`bg-status-danger` only when a requires-response Ping is open, else `bg-muted-foreground`), and a one-line preview. Hidden entirely when there is nothing to show — no standing empty-inbox chrome.
- **Expanded:** an absolutely-positioned `rounded-t-xl` panel (~58dvh, `shadow-lg`) that overlays the transcript rather than pushing it, dismissed by tapping the scrim or Esc.

### Side Chat (signature)
- One component tree, responsive: a full-screen `fixed` sheet below 900px; a `border-l` right rail (`clamp(340px,38vw,440px)`) beside a still-live Chat at ≥900px. Framed as "a fancy reply composer" — the same bubble/timeline/markdown surfaces as Chat, labeled by a header, a pinned collapsible seed card (`bg-muted/30`), and a "wrap-up" bar (`bg-muted/20`) above the composer — never recolored into a second app.

### Navigation
- **Phone / narrow:** the phone-first single column stays — Chat is the whole app. The only chrome is a thin `border-b` top bar carrying the **north-star at rest**: the wordmark plus the **full agent-status indicator** (priority width — it must never truncate to "Agen…"), then a bare connection dot (which expands to the full pill only when reconnecting/offline) and a single `⋯` overflow that folds the quick model variant, Canvas, Processes, and Settings behind one control (≤4 visual chunks). Shelves, sheets, and overlays do the rest. No nav rail.
- **Desktop (`rail`, ≥1100px):** a persistent 3-pane shell. A left **nav rail** (wordmark; **Pings** with a *muted* unread count, Processes with a status-active *tint* chip, and Settings — the three destinations of the single right region; a quiet `⌘K` command-palette hint beside the connection pill pinned at the foot above an always-present `border-t`) ∣ the center **chat pane** ∣ an always-present right **context pane**. The right context pane is owned by **one exclusive `rightRegion`** (`pings` · `sideChat` · `canvas` · `processes` · `settings`): it renders **exactly one** in-flow `<aside>` at one shared width token (`clamp(340px,38vw,440px)`), and the inactive panes **unmount** — no absolute inspector overlaying (and clipping) a still-mounted rail, no hidden pane holding focus. `pings` is the idle resting state (the standing Pings rail); Processes/Settings/Side Chat/Canvas each take the slot until dismissed, and closing any of them returns the region to `pings` ("last explicit user action wins"). A newly-arrived Canvas view auto-surfaces **only** from the idle `pings` state, never evicting an occupied pane — otherwise its availability shows as a reopen affordance. The three panes share one **top datum**: the nav-rail brand block, a slim center-pane header, and every pane title sit on the same `h-12` baseline with a single continuous `border-b` hairline running across all three. The center header's left carries a calm **agent-status indicator** (a status dot + label — pulsing status-active while the agent is thinking, quiet muted "Agent · idle" otherwise), giving the north-star question "what is my agent doing" a persistent desktop home; its right carries the quiet **ModelChip** (the main agent's current model + reasoning variant) and the **Canvas** reopen affordance. The center **chat pane** keeps the prose measure (centered in the pane) for the transcript, and the **composer bar** spans the pane full-width (rail hairline → context hairline) while its input row re-centers at that same measure — no naked canvas beside a lonely column. A sparse transcript bottom-anchors (grows up from the composer). The width is used: the frame fills to a ~1600px cap and only then centers. Each nav item is `aria-current` exactly when it owns `rightRegion`, and the active item uses the **muted fill** (never indigo), so the One-Escalation / indigo-is-rare rules still hold. On phone the same enum drives a single full-screen modal sheet (`role="dialog"`, focus-trapped, Esc returns to `pings`); on desktop the panes are in-flow, non-modal inspectors.

## 6. Do's and Don'ts

### Do:
- **Do** treat dark as the resting state. The canvas is near-black `oklch(0.141 …)`; raised surfaces step up one tone to `card`/`secondary`. Light is a shipped, user-selectable peer (System default) drawn from the same tokens — an off-white canvas (never pure white), white-paper cards, and black-ink hairlines mirroring dark's `white/10%` — and is held to the same bar, not an afterthought.
- **Do** separate siblings with a `white/10` hairline border first, and a `foreground/10` ring on cards — reach for a shadow only when an element genuinely floats (overlay, popover, dialog, floating pill).
- **Do** keep indigo (`--primary`) rare and meaningful: interaction and "attend to this," nothing decorative.
- **Do** reserve monospace for machine tokens — tool names, commands, ids, `@name` handles, keyboard keys — and keep all prose in Inter.
- **Do** make every state legible without color: a stripe, a dot, a weight change, or a literal label alongside the hue. Hold WCAG AA for body and secondary text on every surface.
- **Do** keep motion to state changes — a pulse, a shimmer, a 200ms sheet slide — and honor `prefers-reduced-motion`.
- **Do** keep 16px as the top of the type scale. Earn importance with spacing and weight, not size.
- **Do** keep touch targets thumb-friendly on phone even at high density, and give every keyboard flow a visible focus ring.

### Don't:
- **Don't** build **corporate SaaS chat** (Intercom / Zendesk): no chirpy rounded-friendly bubbles with tails, no marketing tone, no drop-shadowed pastel cards, no "How can I help you today!" energy.
- **Don't** build a **consumer assistant** (Siri / Alexa / ChatGPT mobile): no mascot personality, no over-explaining, no empty enthusiasm, no hand-holding onboarding. Assume total fluency.
- **Don't** build a **notification slot machine**: no badge spam, no red dots scattered around, no engagement-bait urgency. There is exactly one interrupt (danger red on the Tray badge for an open requires-response Ping) and silence otherwise.
- **Don't** introduce a second saturated hue or a gradient. The palette is neutral grays + one indigo + a tiny semantic status set. No neon, no glassmorphism, no purple-gradient dark mode.
- **Don't** multiply reds. If more than one thing is shouting danger, something is miscoded.
- **Don't** add a display face, a hero headline, or type above 16px. hirsel has no marketing surface.
- **Don't** cast a heavy drop shadow on a resting card. If it looks like a 2014 app, the shadow is too dark and the blur is too small — use the hairline ring instead.
- **Don't** set prose, labels, or decoration in monospace. Monospace with no machine content is costume, not signal.
