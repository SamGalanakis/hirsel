import { X } from "lucide-solid";
import { type JSX, Show } from "solid-js";
import { cn } from "@/lib/utils";

// The one header the exclusive right region wears, so its panes read as ONE
// physical slot rather than four different surfaces (spec item 1). Before this,
// older inspectors diverged in title size and close placement; this component
// right-aligned ×, and Processes/Settings wore a `text-base font-semibold` h1
// with a LEFT-aligned × — four treatments in the same `h-12` slot. PaneHeader
// fixes the datum for the desktop presentations of Canvas / Processes /
// Settings: one `h-12` bar on the shared top hairline, a 16px leading icon, one
// title token (`text-sm font-medium`), and — for every DISMISSIBLE pane — a
// single trailing × with a consistent focus-visible ring (a stable
// muscle-memory close target).

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
}

export function PaneHeader(props: Props) {
  return (
    <div
      class={cn(
        "flex h-12 flex-shrink-0 items-center gap-2 border-b border-border px-3",
        props.class,
      )}
    >
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
  );
}
