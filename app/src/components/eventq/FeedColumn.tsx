// The desktop Feed column (desktop-unified shell). On the phone the event queue
// is a full-viewport, one-card-at-a-time scroll-snap PAGER you flick through —
// that is a thumb gesture. On desktop, where Chat now stands permanently beside
// the queue, one-card-at-a-time is the wrong form: the honest "expanded mobile"
// shape is a scrollable COLUMN of the same decidable cards, glanceable and
// decided inline without ever paging. This column replaces BOTH the old desktop
// pieces at once — the full-viewport pager AND the standing index+reader split
// (the index existed only to keep a lone card from floating in a void; Chat fills
// that void now).
//
// It reuses the shared card machinery verbatim (`EventCardHeader` / `DecidedStrip`
// + `EventCardRenderer` + `lib/event-decide`), so a card looks and decides
// identically to the phone — desktop is a composition change, not a re-render of
// cards. The column owns the surface's ONE red: the needs-you count in its
// header (the phone pager's red pill, promoted to the column header). Deciding is
// inline (tap an option); "Discuss" drops a quoted reference into the standing
// composer beside it — no navigation at all.
import { ArrowRight, CircleCheck, MessageSquare } from "lucide-solid";
import { createMemo, For, onMount, Show } from "solid-js";
import type { EventItem } from "../../protocol";
import { cn } from "@/lib/utils";
import { decideEventWithUndo, undoDecide } from "../../lib/event-decide";
import { focusMainComposer } from "../../lib/focus";
import { seedMockEvents } from "../../lib/mock-events";
import {
  eventTitle,
  isEventResolved,
  openJudgmentCount,
  orderedQueue,
} from "../../store/selectors";
import { prefillComposer, state } from "../../store/store";
import { EventCardRenderer } from "../../views/EventCardRenderer";
import { DecidedStrip, EventCardHeader } from "./EventCard";

export function FeedColumn() {
  // DEV: seed the contract-shaped mock events so the Feed is real before the
  // host cutover (the phone scroller seeds the same set; only one is ever mounted
  // at a time, so this never double-seeds). Prod stays empty (inbox-zero).
  onMount(() => {
    if (import.meta.env.DEV && state.events.length === 0) seedMockEvents();
  });

  const ordered = createMemo(() => orderedQueue(state.events, state.eventDecideOverrides));
  const openCount = () => openJudgmentCount(state.events, state.eventDecideOverrides);

  function decide(ev: EventItem, action: string, data: unknown): void {
    if (isEventResolved(ev, state.eventDecideOverrides)) return;
    const label =
      data && typeof data === "object" && "label" in data && typeof data.label === "string"
        ? (data as { label: string }).label
        : undefined;
    // Silent decide (§6, "one Undo"): the card flips to its on-screen DecidedStrip
    // in place (no pager to auto-advance, so it simply stays), and that strip
    // carries the Undo — a toast on top would stack a second Undo.
    decideEventWithUndo(ev.id, action, data, label, { silent: true });
  }

  function discuss(ev: EventItem): void {
    // No navigation: Chat is already on screen. Drop a quoted reference to the
    // judgment into the standing composer (the `>`-quote vocabulary) and land the
    // caret there — the demand for "a standing place to type on desktop", finally
    // paid off (the composer is always mounted beside the Feed).
    prefillComposer(`> ${ev.name} — ${eventTitle(ev)}\n\n`);
    focusMainComposer();
  }

  return (
    <aside
      data-slot="feed-column"
      data-feed-column
      tabindex="-1"
      aria-label="Feed"
      class="hidden w-[clamp(360px,30vw,428px)] shrink-0 flex-col border-r border-border bg-background outline-none rail:flex"
    >
      {/* Header on the shared h-12 top datum (one continuous hairline across the
          icon rail brand, this header, and the chat header). Carries the
          surface's ONE red — the needs-you count — as the calm pill vocabulary
          the phone pager uses (danger tint when something is owed, success tint
          when clear). Nothing else on this surface is ever red. */}
      <div class="flex h-12 shrink-0 items-center gap-2.5 border-b border-border px-4">
        <h2 class="m-0 text-sm font-semibold tracking-[0.01em] text-foreground">Feed</h2>
        <span class="flex-1" />
        <span
          data-slot="feed-need"
          class={cn(
            "rounded-full px-2 py-0.5 text-[0.68rem] font-bold",
            openCount() > 0
              ? "bg-status-danger/12 text-status-danger"
              : "bg-status-success/12 text-status-success",
          )}
        >
          {openCount() > 0 ? `${openCount()} need you` : "all clear"}
        </span>
      </div>

      <div class="min-h-0 flex-1 overflow-y-auto px-3 py-3">
        <Show when={ordered().length > 0} fallback={<FeedEmpty />}>
          <div class="flex flex-col gap-3">
            <For each={ordered()}>
              {(ev) => <FeedCard ev={ev} onDecide={decide} onDiscuss={discuss} />}
            </For>
            {/* An inbox-zero footer once nothing is owed — the peak-end reward,
                calm and inline (no confetti), mirroring the phone clear page. */}
            <Show when={openCount() === 0}>
              <div class="flex items-center gap-2 rounded-xl border border-status-success/20 bg-status-success/[0.06] px-3.5 py-3 text-xs text-muted-foreground">
                <CircleCheck class="size-4 shrink-0 text-status-success" aria-hidden="true" />
                Queue clear — everything that needed you is decided.
              </div>
            </Show>
          </div>
        </Show>
      </div>
    </aside>
  );
}

/** One decidable card in the Feed column. The card visual (accent, header, JSON
 * body, decided strip) is the shared vocabulary; the footer is the desktop's —
 * a quiet Discuss link, no swipe hint (there is no swiping in a column). */
function FeedCard(props: {
  ev: EventItem;
  onDecide: (ev: EventItem, action: string, data: unknown) => void;
  onDiscuss: (ev: EventItem) => void;
}) {
  const decided = () => isEventResolved(props.ev, state.eventDecideOverrides);
  const isJudgment = () => props.ev.kind === "judgment";
  return (
    <div
      class={cn(
        "relative overflow-hidden rounded-xl border border-border bg-card shadow-sm",
        !isJudgment() ? "bg-muted/30 shadow-none" : "",
        decided() ? "opacity-80" : "",
        props.ev.read && !isJudgment() ? "opacity-60" : "",
      )}
    >
      <EventCardHeader ev={props.ev} />
      <div class="px-3.5 pb-3.5 pt-2">
        <EventCardRenderer
          ui={props.ev.ui}
          disabled={decided()}
          onAction={(action, data) => props.onDecide(props.ev, action, data)}
        />
      </div>
      <Show when={decided()}>
        <DecidedStrip ev={props.ev} onUndo={(id) => undoDecide(id)} />
      </Show>
      <Show when={isJudgment() && !decided()}>
        <div class="flex items-center gap-2.5 border-t border-border bg-muted/40 px-3.5 py-2.5">
          <span class="min-w-0 truncate text-[0.62rem] font-medium text-muted-foreground/70">
            Choose an option to decide
          </span>
          <span class="flex-1" />
          <button
            type="button"
            class="inline-flex shrink-0 items-center gap-1 rounded-sm text-xs font-semibold text-primary transition-colors hover:text-primary/80 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/50"
            onClick={() => props.onDiscuss(props.ev)}
          >
            <MessageSquare class="size-3.5" aria-hidden="true" /> Discuss
          </button>
        </div>
      </Show>
    </div>
  );
}

/** Inbox-zero: a genuinely empty Feed (pre-data or an emptied set). Calm, no
 * standing chrome — the quiet resting face of the needs-you surface. */
function FeedEmpty() {
  return (
    <div class="flex h-full flex-col items-center justify-center gap-3 px-6 text-center">
      <span class="grid size-12 place-items-center rounded-full border border-border bg-muted/50 text-muted-foreground">
        <CircleCheck class="size-6" aria-hidden="true" />
      </span>
      <div class="text-sm font-semibold text-foreground">Nothing needs you</div>
      <p class="max-w-[15rem] text-xs leading-relaxed text-muted-foreground">
        New judgments and digests land here. Talk to the agent anytime — the composer is right there.
      </p>
      <ArrowRight class="size-4 text-muted-foreground/60" aria-hidden="true" />
    </div>
  );
}
