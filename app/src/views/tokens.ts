// Generative-UI tier: maps the catalog's semantic tones/states onto the
// existing calm-terminal status design tokens (see src/styles.css `--status-*`
// and the `@theme inline --color-status-*` registrations). The catalog forbids
// hard-coded colors — every tone resolves to a token here so light/dark themes
// and the single-accent restraint hold by construction.
//
// Every branch returns a COMPLETE literal class string: Tailwind's JIT scans
// source for whole class names, so these must never be assembled from fragments.

/** Semantic tones shared by text / keyValue / list / badge (`callout` uses a
 * narrower set without `muted`). */
export type Tone = "default" | "muted" | "success" | "warning" | "danger";

/** `status` component lifecycle states. */
export type StatusState = "neutral" | "running" | "success" | "warning" | "danger";

/** Foreground text color for a tone. `default` inherits the surrounding
 * foreground (empty class); `warning` maps to the amber `status-attention`
 * token (the design system has no separate "warning", attention IS it). */
export function toneTextClass(tone: string | undefined): string {
  switch (tone) {
    case "muted":
      return "text-muted-foreground";
    case "success":
      return "text-status-success";
    case "warning":
      return "text-status-attention";
    case "danger":
      return "text-status-danger";
    default:
      return "";
  }
}

/** Tinted pill for a `badge` tone: a low-alpha fill of the tone token plus its
 * text color, so a badge reads as a quiet chip rather than a loud block. */
export function toneBadgeClass(tone: string | undefined): string {
  switch (tone) {
    case "success":
      return "bg-status-success/12 text-status-success";
    case "warning":
      return "bg-status-attention/12 text-status-attention";
    case "danger":
      return "bg-status-danger/12 text-status-danger";
    case "muted":
      return "bg-muted text-muted-foreground";
    default:
      return "bg-muted text-foreground";
  }
}

/** The dot color for a `status` state. `running` also pulses (see the
 * component); the class here is only the fill. */
export function statusDotClass(state: string | undefined): string {
  switch (state) {
    case "running":
      return "bg-status-active";
    case "success":
      return "bg-status-success";
    case "warning":
      return "bg-status-attention";
    case "danger":
      return "bg-status-danger";
    default:
      return "bg-status-idle";
  }
}

/** Left-accent + tint for a `callout` tone (`default|success|warning|danger`).
 * `default` uses the neutral card border/surface; the others tint. */
export function calloutToneClass(tone: string | undefined): string {
  switch (tone) {
    case "success":
      return "border-status-success/40 bg-status-success/8";
    case "warning":
      return "border-status-attention/40 bg-status-attention/8";
    case "danger":
      return "border-status-danger/40 bg-status-danger/8";
    default:
      return "border-border bg-muted/40";
  }
}

/** Progress-bar fill color. Progress carries no tone in the catalog, so it uses
 * the single accent (`primary`) — the one sanctioned emphasis color. */
export const PROGRESS_FILL = "bg-primary";

// ---- Event-card vocabulary (ADR-0013) — the interactive sibling maps ----
// The event queue's `eyebrow` carries a wider tone set than `text` (an `accent`
// tone → the one indigo, for a taste-boundary label). Kept here beside the other
// tone maps so the palette authority stays in one file.

/** Foreground color for an `eyebrow` tone. `accent` is the one indigo (a
 * taste-boundary / attend-to label); `danger` the one red; default is muted
 * (a quiet section label). Every branch is a COMPLETE literal class (JIT). */
export function eyebrowToneClass(tone: string | undefined): string {
  switch (tone) {
    case "accent":
      return "text-primary";
    case "danger":
      return "text-status-danger";
    case "success":
      return "text-status-success";
    case "warning":
      return "text-status-attention";
    default:
      return "text-muted-foreground";
  }
}
