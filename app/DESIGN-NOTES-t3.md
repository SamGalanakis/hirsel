# Conversation visuals: t3code compared, and what hirsel took

Reference: `pingdotgg/t3code`, web app at `apps/web/src` (React + Tailwind v4),
phone app at `apps/mobile/src/features/threads`. Read September 2026 against
hirsel's `app/src` (SolidJS + the zaidan/vega kit).

This page exists because the finished run card had drifted: a full-height header
row with `2m 20s` set in body type beside a green check, body prose at the same
size as its own header, Thread citations rendered as dotted-underlined pills that
out-measured the sentence carrying them, and inline code painted as a solid slab.
t3code solves all four with the same idea — **work metadata is caption type on a
flat row; only prose gets reading type** — so hirsel copied its conventions rather
than inventing a fifth scale.

---

## 1. Typography scale

| role | t3code | hirsel now |
|---|---|---|
| message prose | `text-sm` + `leading-relaxed` → 14px / 22.75px, foreground at 80% | `text-sm leading-relaxed` (unchanged), full foreground |
| meta / caption | `text-xs` → 12px / 16px, `text-muted-foreground`, `tabular-nums`, **sans not mono** | `text-meta` → 11.5px, `text-muted-foreground`, `tabular-nums` |
| work-row label | `text-sm` at `text-secondary-label` | `text-meta`, tool name in `font-mono` |
| headings in prose | 20 / 18 / 16 / 14px, weight 600, `line-height 1.3` | 16.8 / 15.2 / 14px, weight 500 (unchanged — already restrained) |

t3code defines no custom `--text-*` on web; every size is a stock Tailwind step
or a literal (`text-[11px]`, `text-[.7rem]`). hirsel keeps its one named token,
`--text-meta` (0.72rem), which sits inside DESIGN.md's 11–12px meta band. The
token was never the problem.

**The bug the comparison found.** `--text-meta` was applied at ~59 call sites and
silently ignored on every `<button>`. `app/src/styles.css` carried an *unlayered*

```css
button, input, select, textarea { font: inherit; }
```

and in Tailwind v4 unlayered CSS outranks every `@layer utilities` rule, so the
reset beat `text-meta` / `text-xs` and every meta line inside a control rendered
at the 16px body size — the "huge numbers" in the run card, verbatim. The reset
now sits in `@layer base`, where it still beats the UA sheet and loses to the
utility a call site actually wrote.

## 2. Duration, elapsed, timestamp

t3code has one formatter (`packages/shared/src/orchestrationTiming.ts`):
sub-second in ms (`842ms`), one decimal under ten seconds (`8.0s`), whole seconds
under a minute (`42s`), then space-joined `1h 4m 12s` with zero parts dropped.
Timestamps are `Intl.DateTimeFormat` wall-clock, day-aware (`3:42 PM`,
`yesterday at 3:42 PM`, `8/13 3:42 PM`).

Placement: the turn fold reads **`Worked for 8.0s`, label first, chevron after**;
message timestamps sit on their own trailing right-aligned row, revealed on
hover. Everything numeric is sans with `tabular-nums` — mono appears only on the
subagent status line and diff stats.

hirsel had two disagreeing formats in one card (`2m20s` on a step row above
`2m 20s` on the header). Both now call `app/src/lib/duration.ts`, which is
t3code's ladder; a run that lands on the minute reads `2m`, not `2m 0s`. The
header keeps hirsel's `chevron · origin · duration · mark` order rather than
t3code's leading label, because hirsel's origin line ("Report from #12", "Woken
by a process") is the identity and has to come first.

## 3. Folding work and reasoning

t3code's collapsed rows carry **no card, no border, no fill** — flat rows with a
hover tint only:

- row: `rounded-md px-0.5 py-0.5`, ~28px tall, `hover:bg-accent/20`
- 24px icon well, 16px icon at `stroke-[1.8]`
- label truncates; the answer preview takes the remaining width
- 12px `ChevronRight` rotated `90deg` when open, in a 16px well that stays
  `invisible` rather than unmounting, so labels never shift
- **1px** between rows (`space-y-px`)
- the expanded body indents `ms-7` onto a `bg-muted/40` block

Only two rules exist in the whole stream: `border-b border-border/60 pb-2 pt-1`
under the turn fold and under the live "Working for" row, so live and settled
look identical. Reasoning is not a fold at all — it is a one-line live activity
row with a shimmer.

hirsel's steps were bordered capsules (`h-6 rounded-full border px-2`) laid out
`flex-wrap`, so a long run reflowed into a paragraph of pills whose order was
recoverable only by reading. They are now flat rows stacked `gap-px` in one
column, hover-tinted, with the timing pinned right — the names read down one
edge and the durations down the other. hirsel keeps its status glyph per row
(t3code tints the tool icon instead); the glyph is the same 12px.

## 4. Inline references and inline code

**Code.** t3code (`index.css`): `1px solid var(--contrast-border)`, radius
`0.375rem`, `background: var(--muted)`, padding `0.1rem 0.35rem`, font-size
`0.75rem` absolute. A hairline outline over a quiet fill — the literal is marked,
not highlighted. hirsel's `bg-muted/70 px-1 py-0.5` painted every path and
command as a solid slab; it is now the t3code recipe in em units
(`0.86em` lands on 12px inside 14px prose and still scales inside meta lines).

**References.** t3code citations are chips, but *small* ones: 12px, height
`1.41em`, radius `0.5em`, padding `0.5em`, icon `1.17em`, tinted by kind at a
single OKLCH lightness (citation hue `oklch(0.62 0.16 259)`), and — importantly —
**no underline anywhere**. Markdown links carry no decoration at rest and gain a
faint dotted underline only on hover.

hirsel's `RichLink` did the opposite: a permanent dotted underline at
`underline-offset-4`, a 16px list avatar beside 14px prose, and a 24px dropdown
chevron. It is now inline text: em-sized icon (`1.15em`), em-sized chevron, and a
1px underline at 25% that firms up on hover. hirsel keeps *an* underline where
t3code drops it, because hirsel's link colour is `currentColor` and colour alone
is not an affordance.

## 5. Message spacing and borders

t3code gives turns no container at all. Rhythm is bottom padding per row kind:
16px after a message, 8px after a work or thinking row, 6px after a fold, 1px
between tool rows; content is capped at 768px with 12–20px gutters. Only the user
message is a bubble (`rounded-2xl`, `p-3`, `bg-accent`, `max-w-[80%]`), and author
names are `sr-only`.

hirsel deliberately keeps **both** sides in bubbles: alignment plus fill is how
the column reads left against right without spending an avatar gutter (see the
comment in `ThreadMessages.tsx`). That divergence is intentional and stays. What
hirsel took is the *interior* rhythm: the trace column, the flat rows, the 1px
row gap, and meta type for everything that is not prose.

## 6. Success and failure marks

t3code has **no success mark** — a completed tool call gets the same neutral icon
well as any other. Failure is a 12px `X` at 40% opacity in the error token, and
the severity ladder is `warning → destructive → tool-error-icon/40 → icon-muted`,
with warning and destructive also bumping the label to `font-medium`.

hirsel keeps its outcome mark (a run card that says nothing else needs one glyph
to say how it ended, and the header's accessible name depends on it), but every
mark dropped from 14px to t3code's 12px and the header itself no longer reserves
a 32px row for it.

## Port cheat-sheet

```
prose              14px / 22.75px               (text-sm leading-relaxed)
meta               11.5px, muted, tabular-nums  (text-meta, sans — never mono)
work row           flat, rounded-md px-0.5 py-0.5, hover:bg-accent/40
row gap            1px (gap-px), one step per line
chevron            12px ChevronRight, rotate-90 when open
outcome mark       12px
inline code        0.86em mono, 1px border, radius 6px, pad 0.1em/0.35em, bg muted/40
inline reference   em-sized icon and chevron, 1px underline at 25%, no dotted rule
duration           ms / 8.0s / 42s / 1h 4m 12s, zero parts dropped
```
