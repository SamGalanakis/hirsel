// The Settings row primitives — the small building blocks every section
// composes from (card surface, section/sub headings, the text column of a row,
// and the three controls: segmented, toggle, select) plus the copyable value
// field. Kept in one module so the sections stay about their own subject.
import { ChevronDown, Copy } from "lucide-solid";
import { For, type JSX, Show } from "solid-js";
import { cn } from "../../lib/utils";
import { copyText } from "./prefs";

/** Grouped card — a raised white-paper (light) / step-up (dark) surface with a
 * hairline border, mirroring the Android SettingsCard. */
export function Card(props: { children: JSX.Element; class?: string; id?: string }) {
  return (
    <div
      id={props.id}
      class={cn("overflow-hidden rounded-xl border border-border bg-card", props.class)}
    >
      {props.children}
    </div>
  );
}

export function SectionHeader(props: { children: JSX.Element; id?: string }) {
  return (
    <h2
      id={props.id}
      class="mt-6 mb-2 px-1 text-[0.68rem] font-medium uppercase tracking-[0.06em] text-muted-foreground first:mt-0 scroll-mt-4"
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
      class="flex items-center gap-1 rounded-lg border border-border bg-secondary p-1"
    >
      <For each={props.options}>
        {(opt) => {
          const selected = () => props.value === opt.value;
          return (
            <button
              type="button"
              role="radio"
              aria-checked={selected()}
              class="flex-1 rounded-md px-2 py-1.5 text-sm outline-none transition-colors focus-visible:ring-2 focus-visible:ring-ring"
              classList={{
                "bg-card font-medium text-foreground shadow-[0_1px_2px_0_rgb(0_0_0/0.06)]":
                  selected(),
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
      class="relative inline-flex h-5 w-9 shrink-0 items-center rounded-full border transition-colors outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:cursor-wait disabled:opacity-50"
      classList={{
        "border-primary bg-primary": props.checked,
        "border-input bg-secondary": !props.checked,
      }}
    >
      <span
        aria-hidden="true"
        class="pointer-events-none ml-0.5 size-3.5 rounded-full transition-transform"
        classList={{
          "translate-x-4 bg-primary-foreground": props.checked,
          "translate-x-0 bg-muted-foreground": !props.checked,
        }}
      />
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
 * the Models section). Quieter than a SectionHeader, louder than a Field. */
export function SubHeading(props: { children: JSX.Element }) {
  return (
    <h3 class="mt-4 mb-1.5 px-1 text-xs font-medium text-foreground first:mt-0">{props.children}</h3>
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
