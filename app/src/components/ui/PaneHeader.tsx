import { X } from "lucide-solid";
import { type JSX, Show } from "solid-js";
import { cn } from "@/lib/utils";

// The one header the exclusive right region wears at EVERY width, so its panes
// read as ONE physical slot rather than several different surfaces. Processes
// and Settings used to carry a second, phone-only `<header>` with a `‹ Tasks`
// chevron — a navigation stack hirsel does not have. Utility panes are summoned
// and DISMISSED, never "gone back from" (DESIGN §4 Utilities: "Every utility
// appears as a temporary sheet or inspector; closing it returns to the same
// focus state"), and "Tasks" is not a place you travel to — it is the standing
// world underneath. So there is one header, one title, one trailing × labelled
// "Close", at both widths.
//
// The datum: `h-14`, matching the task-world header (TaskShell) so summoning a
// pane never jogs the content below it; sticky at the top of its pane with the
// shared top hairline; safe-area top padding so the phone sheet clears the
// notch; a 16px leading icon; one title token (`text-sm font-medium`); and a
// coarse-pointer size bump on the × so the thumb target clears 44px
// (PRODUCT: "phone targets at least 44px").

interface Props {
  /** 16px leading icon (`size-4`), decorative (`aria-hidden`). */
  icon: JSX.Element;
  /** The single pane title — one token across every pane, no second scale. */
  title: string;
  /** Ties the title to a pane's `aria-labelledby` when the pane needs it. */
  titleId?: string;
  /** Close the pane and return to the standing task world. */
  onClose?: () => void;
  /** Accessible label for the close control (e.g. "Close Settings"). */
  closeLabel?: string;
  /** A trailing accessory for a non-dismissible pane. */
  badge?: JSX.Element;
  class?: string;
  /** Overrides on the header's INNER row — the box the icon, title and × line
   * up in. A docked pane wants that row full-bleed (the default `px-3`); a
   * full-viewport pane wants it centred on the same reading column its content
   * holds, so the title and the × land on the content's own two edges instead
   * of on the far corners of a 1440px screen. The hairline and the background
   * stay full-bleed either way — it is one bar, not a floating card. */
  contentClass?: string;
}

export function PaneHeader(props: Props) {
  return (
    <div
      class={cn(
        // `box-content` so the safe-area inset stacks ON TOP of the h-14 datum
        // instead of eating into it — the bar is 56px of chrome everywhere,
        // plus whatever the notch demands.
        "sticky top-0 z-10 box-content flex h-14 flex-shrink-0 flex-col justify-center border-b border-border bg-background pt-[env(safe-area-inset-top)]",
        props.class,
      )}
    >
      <div class={cn("flex w-full items-center gap-2 px-3", props.contentClass)}>
        {props.icon}
        <span
          id={props.titleId}
          class="min-w-0 flex-1 truncate text-sm font-medium text-foreground"
        >
          {props.title}
        </span>
        <Show when={props.onClose} fallback={props.badge}>
          <button
            type="button"
            class="-mr-1 grid size-8 shrink-0 place-items-center rounded text-muted-foreground transition-colors hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/50 [@media(pointer:coarse)]:size-11"
            aria-label={props.closeLabel ?? "Close"}
            onClick={() => props.onClose?.()}
          >
            <X class="size-4" aria-hidden="true" />
          </button>
        </Show>
      </div>
    </div>
  );
}
