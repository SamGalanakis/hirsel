// The Settings row primitives — the small building blocks every section
// composes from (card surface, section/sub headings, the text column of a row,
// and the three controls: segmented, toggle, select) plus the copyable value
// field. Kept in one module so the sections stay about their own subject.
import { ChevronDown, Copy } from "lucide-solid";
import { For, type JSX, Show } from "solid-js";
import { cn } from "../../lib/utils";
import { copyText } from "./prefs";

/** A settings group: flat rows on the canvas, separated by hairlines, with the
 * region formed by the whitespace above its heading — NOT a rounded frame.
 * Settings used to stack seven bordered `bg-card` rectangles down one narrow
 * pane, which DESIGN forbids twice over (§2 "Regions are formed by whitespace
 * and tonal shifts, not boxes"; §6 "no repeated rectangular frames"). Rows keep
 * their own padding and the pane's own `px-4` gives the measure, so content
 * lines up with the section heading instead of being inset inside a box. */
export function Group(props: { children: JSX.Element; class?: string; id?: string }) {
  return (
    <div id={props.id} class={cn("scroll-mt-4", props.class)}>
      {props.children}
    </div>
  );
}

/** Heading for a group a tab holds more than one of (Settings' side tab names
 * the panel, so a lone group under it needs no heading of its own). Sentence
 * case, at the meta size — DESIGN §3 rules out all-caps ("Avoid oversized bold
 * section headings, all-caps navigation"), so the heading earns its rank from
 * weight and the whitespace above it. */
export function SectionHeader(props: { children: JSX.Element; id?: string }) {
  return (
    <h2
      id={props.id}
      class="mt-8 mb-2 text-sm font-medium text-foreground first:mt-0 scroll-mt-4"
    >
      {props.children}
    </h2>
  );
}

/** A title + optional one-line subtitle, the row's text column. */
export function Field(props: { title: string; subtitle?: string }) {
  return (
    <div class="flex min-w-0 flex-col">
      <span class="text-sm text-foreground">{props.title}</span>
      <Show when={props.subtitle}>
        <span class="mt-0.5 text-xs leading-snug text-muted-foreground">{props.subtitle}</span>
      </Show>
    </div>
  );
}

/** Neutral segmented control (System / Light / Dark, notify scope). Selection
 * is a raised neutral pill, never indigo — per DESIGN, indigo stays reserved
 * for "attend to this," so a chosen segment reads as a lift, not an alert. */
export function SegmentedControl<T extends string>(props: {
  options: { value: T; label: string }[];
  value: T;
  onChange: (value: T) => void;
  ariaLabel: string;
}) {
  return (
    <div
      role="radiogroup"
      aria-label={props.ariaLabel}
      // A control sized by its own choices, not by the column it sits in. Full
      // width where a thumb uses it; at `rail`, where Settings became a
      // full-viewport overlay reading at the whole measure, a stretched
      // three-segment bar 672px wide would be a control shouting about itself
      // (DESIGN §4: "A capsule taller than the text it holds is a control
      // advertising itself" — the same rule on the other axis).
      class="flex w-full items-center gap-1 rounded-lg border border-border bg-secondary p-1 rail:w-fit"
    >
      <For each={props.options}>
        {(opt) => {
          const selected = () => props.value === opt.value;
          return (
            <button
              type="button"
              role="radio"
              aria-checked={selected()}
              class="flex-1 rounded-md px-2 py-1.5 text-sm outline-none transition-colors focus-visible:ring-2 focus-visible:ring-ring rail:flex-none rail:px-6 [@media(pointer:coarse)]:min-h-11"
              classList={{
                // `shadow-raised` — the theme-adaptive lift token. The literal
                // `rgb(0 0 0/0.06)` this replaces was invisible on the dark
                // canvas, so a chosen segment lost its lift in the resting theme.
                "bg-card font-medium text-foreground shadow-raised": selected(),
                "text-muted-foreground hover:text-foreground": !selected(),
              }}
              onClick={() => props.onChange(opt.value)}
            >
              {opt.label}
            </button>
          );
        }}
      </For>
    </div>
  );
}

/** Accessible on/off switch. On-state track is the indigo primary (an
 * interactive control state, matching the Android switch), thumb near-white so
 * it reads in both themes. */
export function Toggle(props: {
  checked: boolean;
  onChange: (v: boolean) => void;
  ariaLabel: string;
  disabled?: boolean;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={props.checked}
      aria-label={props.ariaLabel}
      disabled={props.disabled}
      onClick={() => props.onChange(!props.checked)}
      // The 20×36px switch is the VISUAL, not the target: on a coarse pointer
      // the button grows to a 44px square hit area around it (PRODUCT: "phone
      // targets at least 44px") while the track keeps its quiet proportions.
      class="group inline-flex shrink-0 items-center justify-end outline-none disabled:cursor-wait disabled:opacity-50 [@media(pointer:coarse)]:min-h-11 [@media(pointer:coarse)]:min-w-11"
    >
      {/* Ring on the track, not the grown hit box, so keyboard focus outlines
          the switch the eye sees. */}
      <span
        aria-hidden="true"
        class="relative flex h-5 w-9 shrink-0 items-center rounded-full border transition-colors group-focus-visible:ring-2 group-focus-visible:ring-ring"
        classList={{
          "border-primary bg-primary": props.checked,
          "border-input bg-secondary": !props.checked,
        }}
      >
        <span
          class="pointer-events-none ml-0.5 size-3.5 rounded-full transition-transform"
          classList={{
            "translate-x-4 bg-primary-foreground": props.checked,
            "translate-x-0 bg-muted-foreground": !props.checked,
          }}
        />
      </span>
    </button>
  );
}

/** Compact styled native select — the app's form-select building block (used by
 * the Models section). Native for honest keyboard/AT behaviour and platform
 * option lists; the chevron is decorative (appearance stripped). */
export function Select<T extends string>(props: {
  value: T;
  options: { value: T; label: string }[];
  onChange: (value: T) => void;
  ariaLabel: string;
  disabled?: boolean;
  class?: string;
}) {
  return (
    <div class={cn("relative inline-flex", props.class)}>
      <select
        aria-label={props.ariaLabel}
        disabled={props.disabled}
        value={props.value}
        onChange={(e) => props.onChange(e.currentTarget.value as T)}
        class="h-9 w-full appearance-none rounded-lg border border-border bg-surface pl-2.5 pr-8 text-sm text-foreground outline-none transition-colors hover:border-input focus-visible:ring-2 focus-visible:ring-ring disabled:cursor-not-allowed disabled:opacity-50"
      >
        <For each={props.options}>{(o) => <option value={o.value}>{o.label}</option>}</For>
      </select>
      <ChevronDown
        class="pointer-events-none absolute right-2.5 top-1/2 size-3.5 -translate-y-1/2 text-muted-foreground"
        aria-hidden="true"
      />
    </div>
  );
}

/** A sub-heading within a section (e.g. "Main agent" / a provider name inside
 * the Agents tab). Quieter than a SectionHeader — it must not outrank the
 * heading it lives under. */
export function SubHeading(props: { children: JSX.Element }) {
  return (
    <h3 class="mt-5 mb-1 text-xs font-medium text-muted-foreground first:mt-0">{props.children}</h3>
  );
}

/** A copyable value in a raised inset field (endpoint, identity fingerprint). */
export function CopyRow(props: { value: string; label: string; mono?: boolean }) {
  return (
    <button
      type="button"
      onClick={() => copyText(props.value, props.label)}
      class="flex w-full items-center gap-2 rounded-lg border border-border bg-surface px-3 py-2.5 text-left outline-none transition-colors hover:border-input focus-visible:ring-2 focus-visible:ring-ring"
      aria-label={`Copy ${props.label}`}
    >
      <span
        class="min-w-0 flex-1 truncate text-sm text-foreground"
        classList={{ "font-mono": props.mono }}
      >
        {props.value}
      </span>
      <Copy class="size-3.5 shrink-0 text-muted-foreground" aria-hidden="true" />
    </button>
  );
}
