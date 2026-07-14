// One queue index row (ADR-0012), shared by the phone scroller's peek overview
// (the whole-queue index you flick down to jump from). State reads through
// weight + label, never hue alone: kind glyph · @name · one-line title · state —
// decided (green check), needs-you (the one indigo — never red; the surface's
// single red is the pager's needs-you pill), snoozed (muted), and quiet new/read
// for the awareness tail.
//
// The desktop Feed column is a column of full decidable CARDS, not compact index
// rows, so it does NOT use this — the standing two-column index the pre-unified
// desktop stood beside a lonely reader is retired (Chat now sits beside the Feed,
// so there is no void to fill). QueueRow lives on for the phone peek.
import { Check, FileText, Info, type LucideProps, Scale } from "lucide-solid";
import { type Component, Show } from "solid-js";
import { Dynamic } from "solid-js/web";
import type { EventItem } from "../../protocol";
import { cn } from "@/lib/utils";
import { eventTitle, isEventResolved } from "../../store/selectors";
import { state } from "../../store/store";

const KIND_GLYPH: Record<string, Component<LucideProps>> = {
  judgment: Scale,
  summary: FileText,
  info: Info,
};

/** One queue index row (phone peek). Click jumps the pager to the event.
 * (Wave-3: durable snooze removes a parked event from the active queue entirely,
 * so a resting row is never "snoozed" — the parked set lives in the Snoozed
 * filter's dense rows instead.) */
export function QueueRow(props: {
  ev: EventItem;
  active: boolean;
  onJump: (ev: EventItem) => void;
}) {
  const resolved = () => isEventResolved(props.ev, state.eventDecideOverrides);
  const isJudgment = () => props.ev.kind === "judgment";
  const needsYou = () => isJudgment() && !resolved();
  const label = () => `Jump to ${props.ev.name}: ${eventTitle(props.ev)}`;
  return (
    <button
      type="button"
      aria-label={label()}
      class={cn(
        "grid w-full grid-cols-[auto_1fr_auto] items-center gap-2.5 rounded-md border border-transparent px-2 py-1.5 text-left transition-colors",
        "hover:bg-muted/60 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/50",
        props.active ? "border-border bg-muted/60" : "",
      )}
      onClick={() => props.onJump(props.ev)}
    >
      <Dynamic
        component={KIND_GLYPH[props.ev.kind] ?? Info}
        class={cn(
          "size-3.5 shrink-0",
          needsYou() ? "text-primary" : "text-muted-foreground/70",
        )}
        aria-hidden="true"
      />
      <span class="min-w-0">
        <span class="block truncate font-mono text-[0.68rem] font-medium text-primary">{props.ev.name}</span>
        <span
          class={cn(
            "block truncate text-xs",
            !isJudgment() && props.ev.read ? "text-muted-foreground/70" : "text-foreground",
          )}
        >
          {eventTitle(props.ev)}
        </span>
      </span>
      <span class="shrink-0 text-[0.62rem] font-bold">
        <Show
          when={resolved()}
          fallback={
            <Show
              when={isJudgment()}
              fallback={
                <span class={props.ev.read ? "text-muted-foreground/60" : "text-muted-foreground"}>
                  {props.ev.read ? "read" : "new"}
                </span>
              }
            >
              <span class="text-primary">needs you</span>
            </Show>
          }
        >
          <span class="inline-flex items-center gap-1 text-status-success">
            <Check class="size-3" aria-hidden="true" /> decided
          </span>
        </Show>
      </span>
    </button>
  );
}
