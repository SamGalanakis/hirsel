// The standing queue index (ADR-0012, desktop-shell §1). At `rail` widths the
// queue home is a two-column reader: this in-flow list of every queued event
// beside the reader-measure card column. It promotes the phone peek's content
// into a persistent left rail so the reader card is never a lonely centred
// column in a void — the list carries navigation (click a row to jump the
// pager), the reader carries the one focused card.
//
// The SAME `QueueRow` renders here and in the phone peek overlay, so the two
// stay in lockstep: kind glyph · @name · one-line title · state. State reads
// through weight + label, never hue alone: decided (green check), needs-you
// (the one indigo — never red; the surface's single red is the pager pill),
// snoozed (muted), and quiet new/read for the awareness tail.
import { Check, FileText, Info, type LucideProps, Scale } from "lucide-solid";
import { type Component, For, Show } from "solid-js";
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

/** One queue index row, shared by the standing list and the phone peek. Click
 * jumps the pager to the event; a snoozed row's jump also returns it to its
 * natural place (un-snooze), the standing "bring it back" path (§6). */
export function QueueRow(props: {
  ev: EventItem;
  active: boolean;
  snoozed: boolean;
  onJump: (ev: EventItem) => void;
}) {
  const resolved = () => isEventResolved(props.ev, state.eventDecideOverrides);
  const isJudgment = () => props.ev.kind === "judgment";
  const needsYou = () => isJudgment() && !resolved() && !props.snoozed;
  const label = () =>
    props.snoozed && !resolved()
      ? `Return ${props.ev.name} to the queue: ${eventTitle(props.ev)}`
      : `Jump to ${props.ev.name}: ${eventTitle(props.ev)}`;
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
              <Show when={props.snoozed} fallback={<span class="text-primary">needs you</span>}>
                <span class="text-muted-foreground/70">snoozed</span>
              </Show>
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

/** The desktop standing queue list — the left column of the two-column home.
 * CSS-gated to `rail`: hidden below it (the phone uses the peek overlay), an
 * in-flow bordered column at/above it. Shares the top datum with the nav rail
 * (an `h-12` header on the same baseline, one continuous top hairline). */
export function QueueList(props: {
  ordered: EventItem[];
  centeredId: number | undefined;
  snoozed: Set<number>;
  openCount: number;
  atClear: boolean;
  onJump: (ev: EventItem) => void;
  onJumpClear: () => void;
}) {
  return (
    <aside
      aria-label="Queue"
      data-slot="queue-list"
      class="hidden w-[19rem] shrink-0 flex-col border-r border-border bg-background rail:flex"
    >
      {/* Silent header: the pager's "N need you" pill in the reader beside this
          list is the surface's SINGLE count-bearing element — repeating the
          number here stated it three ways on one view. The rows' own state
          labels carry the per-item truth. */}
      <div class="flex h-12 shrink-0 items-center border-b border-border px-4">
        <h2 class="m-0 text-sm font-semibold tracking-[0.01em] text-foreground">Queue</h2>
      </div>
      <div class="min-h-0 flex-1 overflow-y-auto p-1.5">
        <For each={props.ordered}>
          {(ev) => (
            <QueueRow
              ev={ev}
              active={ev.id === props.centeredId}
              snoozed={props.snoozed.has(ev.id)}
              onJump={props.onJump}
            />
          )}
        </For>
        {/* The end page as the list's tail, so "you're at the bottom" reads in
            the index too and is one click away. Labelled by the same predicate
            the pager uses: it only says "clear" when nothing is open. */}
        <button
          type="button"
          aria-label="Jump to the end of the queue"
          class={cn(
            "mt-0.5 flex w-full items-center gap-2.5 rounded-md border border-transparent px-2 py-1.5 text-left text-xs transition-colors",
            "hover:bg-muted/60 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/50",
            props.atClear ? "border-border bg-muted/60" : "",
          )}
          onClick={props.onJumpClear}
        >
          <Check
            class={cn("size-3.5 shrink-0", props.openCount === 0 ? "text-status-success" : "text-muted-foreground/60")}
            aria-hidden="true"
          />
          <span class="text-muted-foreground">
            {props.openCount === 0 ? "Queue clear" : "End of the queue"}
          </span>
        </button>
      </div>
    </aside>
  );
}
