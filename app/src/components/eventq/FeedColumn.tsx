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
//
// Archive (contract v1): the resting column default-hides archived events —
// counts run on the same filtered set, so nothing ever disagrees. A calm filter
// row on top of the column (owner addendum) carries a search box + a segmented
// Active · Needs you · Archived(n) control (default Active every session);
// switching to Archived discloses the dense archived rows, each with Unarchive.
import { ArrowRight, CircleCheck } from "lucide-solid";
import { createEffect, createMemo, createSignal, For, onMount, Show } from "solid-js";
import type { EventItem } from "../../protocol";
import { cn } from "@/lib/utils";
import { archiveEventWithUndo, unarchiveEvent } from "../../lib/event-archive";
import { decideEventWithUndo, undoDecide } from "../../lib/event-decide";
import { snoozeEventWithUndo, unsnoozeEvent } from "../../lib/event-snooze";
import { clearFinishedEventsWithUndo } from "../../lib/event-sweep";
import { openEventFork } from "../../lib/event-fork";
import { formatEventAge } from "../../lib/format";
import { seedMockEvents } from "../../lib/mock-events";
import {
  archivedEvents,
  finishedEvents,
  isEventResolved,
  isOpenJudgment,
  openJudgmentCount,
  orderedQueue,
  snoozedEvents,
  visibleEvents,
} from "../../store/selectors";
import { state } from "../../store/store";
import { EventCardRenderer } from "../../views/EventCardRenderer";
import { ArchivedList } from "./ArchivedList";
import {
  ARCHIVE_EXIT_CLASS,
  CARD_ENTER_FROM,
  CARD_ENTER_TRANSITION,
  createArchiveExit,
  createCardEntrance,
  DecidedStrip,
  EventCardDoor,
  EventCardHeader,
} from "./EventCard";
import {
  matchesQuery,
  QueueFilterBar,
  queueFilterMode,
  queueSearch,
  setQueueFilterMode,
  setQueueSearch,
} from "./QueueFilter";
import { SnoozeChooser } from "./SnoozeChooser";
import { SnoozedList } from "./SnoozedList";

export function FeedColumn() {
  // DEV: seed the contract-shaped mock events so the Feed is real before the
  // host cutover (the phone scroller seeds the same set; only one is ever mounted
  // at a time, so this never double-seeds). Prod stays empty (inbox-zero).
  onMount(() => {
    if (import.meta.env.DEV && state.events.length === 0) seedMockEvents();
  });

  // THE default filter: the resting column and its counts both read the
  // archived-free set, so an archived event vanishes from the cards and the
  // needs-you pill in the same reactive beat.
  const visible = createMemo(() => visibleEvents(state.events, state.eventArchiveOverrides));
  const ordered = createMemo(() => orderedQueue(visible(), state.eventDecideOverrides));
  const openCount = () => openJudgmentCount(visible(), state.eventDecideOverrides);
  const archived = createMemo(() => archivedEvents(state.events, state.eventArchiveOverrides));
  // Wave-3: the parked (snoozed) set and the finished set the sweep would clear.
  const snoozed = createMemo(() => snoozedEvents(state.events, state.eventArchiveOverrides));
  const finished = createMemo(() =>
    finishedEvents(state.events, state.eventArchiveOverrides, state.eventDecideOverrides),
  );
  const sweep = () => clearFinishedEventsWithUndo(finished().map((e) => e.id));

  // The filter bar state lives in the shared signals (so ⌘K can drive it); the
  // standing desktop column reads them directly. Default `active` — archive /
  // snooze are storage, not standing second queues. A live search string narrows
  // whichever filter is showing.
  const mode = queueFilterMode;
  const query = queueSearch;

  // Any events at all (live, snoozed, or archived): drives the resting FeedEmpty
  // vs. a quiet in-filter empty line, and gates the filter bar.
  const hasAnyEvents = () => visible().length > 0 || snoozed().length > 0 || archived().length > 0;

  // The cards the current filter shows, narrowed by the search: `needs-you`
  // keeps only open judgments, `active` keeps the whole resting queue.
  const filteredCards = createMemo(() => {
    const base =
      mode() === "needs-you"
        ? ordered().filter((e) => isOpenJudgment(e, state.eventDecideOverrides))
        : ordered();
    return base.filter((e) => matchesQuery(e, query()));
  });
  const filteredArchived = createMemo(() =>
    archived().filter((e) => matchesQuery(e, query())),
  );
  const filteredSnoozed = createMemo(() =>
    snoozed().filter((e) => matchesQuery(e, query())),
  );

  // A disclosure filter can't stand once its set empties (the last Unarchive /
  // Unsnooze / return emptied it): fall back to the resting Active filter so the
  // column never strands on an empty view.
  createEffect(() => {
    if (mode() === "archived" && archived().length === 0) setQueueFilterMode("active");
    if (mode() === "snoozed" && snoozed().length === 0) setQueueFilterMode("active");
  });

  // The one quiet line for an empty live filter (search miss / nothing owed).
  const emptyCardsLine = () =>
    query().trim().length > 0
      ? `No events match “${query().trim()}”.`
      : mode() === "needs-you"
        ? "Nothing needs you right now."
        : "Nothing in the active queue.";

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

  function open(ev: EventItem): void {
    // Open (or resume) the event fork — a focused thread scoped to THIS card,
    // with the card itself pinned and still decidable at the top. Supersedes the
    // composer-prefill hack (a quoted reference dropped into the standing
    // composer): a fork keeps the decision framed and closes the loop, where a
    // prefilled quote just seeded a message. On desktop the fork docks the right
    // region beside Feed + Chat; on phone it is a full-screen sheet.
    openEventFork(ev);
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

      {/* The filter row — a hairline strip below the datum. Search narrows the
          list live; the segmented control switches Active · Needs you ·
          Archived(n). Deliberately quieter than the cards (no red, 13px). */}
      <Show when={hasAnyEvents()}>
        <div class="shrink-0 border-b border-border px-3 py-2">
          <QueueFilterBar
            query={query()}
            onQueryChange={setQueueSearch}
            mode={mode()}
            onModeChange={setQueueFilterMode}
            archivedCount={archived().length}
            snoozedCount={snoozed().length}
            finishedCount={finished().length}
            onClearFinished={sweep}
          />
        </div>
      </Show>

      <div class="min-h-0 flex-1 overflow-y-auto px-3 py-3">
        <Show when={hasAnyEvents()} fallback={<FeedEmpty />}>
          {/* Archived filter: the day-log — dense rows grouped by day, each with
              Unarchive (never full cards). */}
          <Show when={mode() === "archived"}>
            <div data-slot="feed-archived">
              <Show
                when={filteredArchived().length > 0}
                fallback={
                  <QueueEmptyLine>
                    {query().trim().length > 0
                      ? `No archived events match “${query().trim()}”.`
                      : "Nothing archived."}
                  </QueueEmptyLine>
                }
              >
                <ArchivedList
                  events={filteredArchived()}
                  onUnarchive={(ev) => unarchiveEvent(ev.id)}
                />
              </Show>
            </div>
          </Show>
          {/* Snoozed filter (Wave-3): dense rows with the return time + Unsnooze. */}
          <Show when={mode() === "snoozed"}>
            <div data-slot="feed-snoozed">
              <Show
                when={filteredSnoozed().length > 0}
                fallback={
                  <QueueEmptyLine>
                    {query().trim().length > 0
                      ? `No snoozed events match “${query().trim()}”.`
                      : "Nothing snoozed."}
                  </QueueEmptyLine>
                }
              >
                <SnoozedList
                  events={filteredSnoozed()}
                  onUnsnooze={(ev) => unsnoozeEvent(ev.id)}
                />
              </Show>
            </div>
          </Show>
          {/* Active / Needs you: the decidable cards. */}
          <Show when={mode() === "active" || mode() === "needs-you"}>
            <Show
              when={filteredCards().length > 0}
              fallback={<QueueEmptyLine>{emptyCardsLine()}</QueueEmptyLine>}
            >
              <div class="flex flex-col gap-3">
                <For each={filteredCards()}>
                  {(ev) => <FeedCard ev={ev} onDecide={decide} onOpen={open} />}
                </For>
                {/* An inbox-zero footer once nothing is owed (only in the
                    resting Active filter, no search) — the peak-end reward,
                    calm and inline, mirroring the phone clear page. */}
                <Show when={mode() === "active" && query().trim().length === 0 && openCount() === 0}>
                  <div class="flex items-center gap-2 rounded-xl border border-status-success/20 bg-status-success/[0.06] px-3.5 py-3 text-xs text-muted-foreground">
                    <CircleCheck class="size-4 shrink-0 text-status-success" aria-hidden="true" />
                    Queue clear — everything that needed you is decided.
                  </div>
                </Show>
              </div>
            </Show>
          </Show>
        </Show>
      </div>
    </aside>
  );
}

/** One decidable card in the Feed column. The card visual (accent, header, JSON
 * body, decided strip) is the shared vocabulary; the footer is the shared
 * `EventCardDoor` — Discuss / Ask / "discussion open · resume" — with no swipe
 * hint (there is no swiping in a column). */
function FeedCard(props: {
  ev: EventItem;
  onDecide: (ev: EventItem, action: string, data: unknown) => void;
  onOpen: (ev: EventItem) => void;
}) {
  const decided = () => isEventResolved(props.ev, state.eventDecideOverrides);
  const isJudgment = () => props.ev.kind === "judgment";
  const [snoozeOpen, setSnoozeOpen] = createSignal(false);
  // Motion-safe exit: the card fades/settles out, then the optimistic sweep
  // re-flows the column (the toast's Undo brings it straight back).
  const { leaving, archive } = createArchiveExit(() => archiveEventWithUndo(props.ev.id));
  // Motion-safe entrance (genuine arrivals only): the fade + ~6px settle that
  // mirrors the exit. Initial hydration and re-renders stay still.
  const { entering, atFrom } = createCardEntrance(props.ev);
  return (
    <div
      class={cn(
        "relative overflow-hidden rounded-xl border border-border bg-card shadow-sm",
        !isJudgment() ? "bg-muted/30 shadow-none" : "",
        decided() ? "opacity-80" : "",
        props.ev.read && !isJudgment() ? "opacity-60" : "",
        entering() ? CARD_ENTER_TRANSITION : "",
        atFrom() ? CARD_ENTER_FROM : "",
        leaving() ? ARCHIVE_EXIT_CLASS : "",
      )}
    >
      <EventCardHeader
        ev={props.ev}
        onArchive={archive}
        onRequestSnooze={() => setSnoozeOpen(true)}
      />
      <div class="px-3.5 pb-3.5 pt-2">
        <EventCardRenderer
          ui={props.ev.ui}
          disabled={decided()}
          eyebrowAge={
            props.ev.blocking && isJudgment() ? formatEventAge(props.ev.ts) : undefined
          }
          onAction={(action, data) => props.onDecide(props.ev, action, data)}
        />
      </div>
      <Show when={decided()}>
        <DecidedStrip ev={props.ev} onUndo={(id) => undoDecide(id)} onArchive={archive} />
      </Show>
      {/* The door slot: shown for every card except a decided judgment (which is
          leaving the queue). A judgment gets Discuss, a summary/info card gets
          Ask, and a live fork collapses either to the "discussion open · resume"
          chip — all `openEventFork`. */}
      <Show when={!(isJudgment() && decided())}>
        <EventCardDoor ev={props.ev} onOpen={props.onOpen} />
      </Show>
      {/* The durable-snooze preset chooser (Wave-3). */}
      <SnoozeChooser
        open={snoozeOpen()}
        onOpenChange={setSnoozeOpen}
        onPick={(until, label) => snoozeEventWithUndo(props.ev.id, until, label)}
      />
    </div>
  );
}

/** One quiet line for an empty filter/search result (a search miss, a
 * needs-you filter with nothing owed, an empty archive). Not the big FeedEmpty
 * peak-end state — just a muted sentence so the column is never blank. */
function QueueEmptyLine(props: { children: string }) {
  return (
    <div class="px-2 py-6 text-center text-xs text-muted-foreground/70" data-slot="queue-empty">
      {props.children}
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
