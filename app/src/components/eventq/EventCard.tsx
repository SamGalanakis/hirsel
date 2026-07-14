// Shared event-card pieces — the parts of a queue card that must look identical
// wherever a card is shown: the phone scroller's full-viewport pager (EventPage)
// and the desktop Feed column (FeedCard). Only the COMPOSITION around the card
// differs (a swipe wrapper + flick affordances on phone, a static scroll item on
// desktop); the card's own chrome — the needs-you accent, the @name·source·kind
// header, and the decided/undo strip — lives here so the two never drift.
//
// The card BODY is the constrained JSON UI (`EventCardRenderer`) and the decide
// path is `lib/event-decide` — both already shared. This module adds the last
// shared visual atoms so "desktop is a composition change, not a re-render of
// cards" holds literally.
import { ArrowRight, Check } from "lucide-solid";
import { Show } from "solid-js";
import type { EventItem } from "../../protocol";
import { isEventResolved } from "../../store/selectors";
import { state } from "../../store/store";

export const KIND_LABEL: Record<string, string> = {
  judgment: "Judgment",
  summary: "Summary",
  info: "Info",
};

/** The card's top matter: the one-accent needs-you edge (a hairline indigo strip
 * — the surface's single accent, never red) plus the minimal-chrome header row
 * (@name · source · kind, no wait/cost/turns). Placed as the first children of a
 * `relative overflow-hidden` card box in each composition. */
export function EventCardHeader(props: { ev: EventItem }) {
  const decided = () => isEventResolved(props.ev, state.eventDecideOverrides);
  const isJudgment = () => props.ev.kind === "judgment";
  return (
    <>
      <Show when={isJudgment() && !decided()}>
        <span class="absolute inset-y-0 left-0 w-0.5 bg-primary" aria-hidden="true" />
      </Show>
      <div class="flex flex-wrap items-center gap-x-2 gap-y-1 px-3.5 pt-3">
        <span class="font-mono text-xs font-medium text-primary">{props.ev.name}</span>
        <span class="text-[0.68rem] font-medium text-muted-foreground/80">
          ·{" "}
          <span class="font-semibold text-muted-foreground">
            {props.ev.source.ref ?? props.ev.source.kind}
          </span>
        </span>
        <span class="text-[0.56rem] font-bold uppercase tracking-[0.05em] text-muted-foreground/70">
          {KIND_LABEL[props.ev.kind] ?? props.ev.kind}
        </span>
        <span class="flex-1" />
        <Show when={props.ev.read && props.ev.kind !== "judgment" && !decided()}>
          <span class="inline-flex items-center gap-1 text-[0.56rem] font-bold uppercase tracking-[0.04em] text-status-success">
            <Check class="size-3" aria-hidden="true" /> read
          </span>
        </Show>
      </div>
    </>
  );
}

/** The interaction-back confirmation that replaces the card's action row once
 * decided: a green check + the posted payload + a brief Undo. Shared verbatim by
 * the phone pager and the desktop Feed column. */
export function DecidedStrip(props: { ev: EventItem; onUndo: (id: number) => void }) {
  return (
    <div class="border-t border-border bg-muted/40 px-3.5 py-3">
      <div class="flex items-center gap-2">
        <span class="grid size-[18px] shrink-0 place-items-center rounded-full bg-status-success/15 text-status-success">
          <Check class="size-3" aria-hidden="true" />
        </span>
        <span class="text-xs font-semibold text-foreground">
          {props.ev.name} → <span class="text-status-success">decided</span>
        </span>
      </div>
      <div class="mt-2 flex items-center gap-2 text-[0.68rem] text-muted-foreground">
        <ArrowRight class="size-3 text-primary" aria-hidden="true" />
        posted to {props.ev.source.ref ?? props.ev.source.kind}
        <span class="flex-1" />
        <button
          type="button"
          class="font-bold text-primary transition-colors hover:text-primary/80"
          onClick={() => props.onUndo(props.ev.id)}
        >
          Undo
        </button>
      </div>
    </div>
  );
}
