// The home: a vertical, one-event-per-viewport scroll-snap pager you flick
// through and clear (ADR-0012). Ports scratchpad/spike-scroller.html to SolidJS.
// The cards are the SAME constrained JSON UI as the Canvas tier (EventCardRenderer,
// ADR-0013) — this component is the presentation SHELL around them: a slim top
// pager, priority ordering (blocking judgments → needs-you → the awareness tail),
// buttons that carry the choice, swipes that accelerate (→ accept the pick, ←
// snooze to the tail, ↑ next), decide→confirm→auto-advance with undo, awareness
// that auto-marks-read as it scrolls past, a peek-to-overview, and an inbox-zero
// end state. Cards are MINIMAL CHROME — no wait/cost/turn/telemetry.
//
// The scroller is the phone home and the desktop center measure (a focused
// reader). Chat + the inspectors are drill-ins reached from a judgment's Discuss
// or the NavRail; nothing here can recolor a card — it only pages, decides, and
// advances.
import { ArrowRight, ArrowUp, ChevronDown, CircleCheck, List, MessageSquare } from "lucide-solid";
import { createEffect, createMemo, createSignal, For, onCleanup, onMount, Show, untrack } from "solid-js";
import type { EventItem } from "../../protocol";
import { cn } from "@/lib/utils";
import { archiveEventWithUndo, unarchiveEvent } from "../../lib/event-archive";
import {
  DECIDE_UNDO_WINDOW_MS,
  decideEventWithUndo,
  markEventRead,
  undoDecide,
} from "../../lib/event-decide";
import { createMediaFlag } from "../../lib/focus";
import { seedMockEvents } from "../../lib/mock-events";
import { toast } from "../../lib/toast";
import {
  archivedEvents,
  eventTitle,
  eventUiNodes,
  isEventArchived,
  isEventResolved,
  isOpenJudgment,
  openJudgmentCount,
  orderedQueue,
  visibleEvents,
} from "../../store/selectors";
import { goToChat, state } from "../../store/store";
import { EventCardRenderer } from "../../views/EventCardRenderer";
import { ArchivedList } from "./ArchivedList";
import {
  ARCHIVE_EXIT_CLASS,
  createArchiveExit,
  DecidedStrip,
  EventCardHeader,
} from "./EventCard";
import { matchesQuery, type QueueFilterMode, QueueFilterBar } from "./QueueFilter";
import { QueueRow } from "./QueueRow";
import { firstOpenIndex, nextOpenIndex, shouldMarkReadOnLeave } from "./queue";

/** How long the decided card lingers (confirmation + Undo reachable) before the
 * pager auto-advances to the next open event. Undo within the window cancels it
 * (the advance re-checks that the event is still decided at fire time). */
const ADVANCE_MS = 1150;

/** Horizontal drag past this (px) commits the swipe: → accepts the pick, ←
 * snoozes to the tail. Below it the card springs back. */
const SWIPE_THRESHOLD = 82;

function prefersReduced(): boolean {
  return typeof window !== "undefined" && window.matchMedia?.("(prefers-reduced-motion: reduce)").matches;
}

/** The recommended (else first) option of a judgment's optionList, for the
 * accept-the-pick gesture/key. Null when the card has no options. */
function recommendedChoice(ev: EventItem): { action: string; choice: string; label: string } | null {
  const list = eventUiNodes(ev.ui).find((n) => n.type === "optionList");
  if (!list) return null;
  const options = (Array.isArray(list.options) ? list.options : []) as Record<string, unknown>[];
  if (options.length === 0) return null;
  const rec = options.find((o) => o.recommended === true) ?? options[0];
  return {
    action: typeof list.action === "string" ? list.action : "choose",
    choice: String(rec.key ?? ""),
    label: String(rec.label ?? "").replace(/`/g, ""),
  };
}

export function EventScroller() {
  let scrollerRef: HTMLDivElement | undefined;
  const setScrollerRef = (el: HTMLDivElement) => (scrollerRef = el);
  let rootRef: HTMLDivElement | undefined;
  const setRootRef = (el: HTMLDivElement) => (rootRef = el);
  const [current, setCurrent] = createSignal(0);
  const [peekOpen, setPeekOpen] = createSignal(false);
  const [snoozed, setSnoozed] = createSignal<Set<number>>(new Set());

  // `rail` (desktop) stands the two-column list, so the phone peek overlay is
  // gated off there — the standing list already IS the whole-queue index.
  const atRail = createMediaFlag("(min-width: 1100px)");

  // DEV: seed the contract-shaped mock events so the home is real before the
  // host cutover. Prod stays empty until the host sends events (inbox-zero).
  onMount(() => {
    if (import.meta.env.DEV && state.events.length === 0) seedMockEvents();
    // The keyboard mirror is attached imperatively (not as a JSX handler) so the
    // scroll region stays a plain container for a11y — the keys are an
    // accelerator over the on-card buttons, which remain the primary controls.
    rootRef?.addEventListener("keydown", onKeyDown);
    onCleanup(() => rootRef?.removeEventListener("keydown", onKeyDown));
  });

  // Keyboard alive on load and on return (§5): focus the scroller root whenever
  // the queue is the home surface, so a keystroke pages immediately without a
  // hunt-and-click first. Depends only on `home`, so it fires on mount (home is
  // queue) and again each time a drill-in returns here — never mid-session.
  createEffect(() => {
    if (state.home === "queue") rootRef?.focus();
  });

  // THE default filter (archive contract v1): the pager's pages and every count
  // it shows read the archived-free set, so an archived event leaves the pages,
  // the "N of M" position, and the needs-you pill in the same reactive beat.
  const visible = createMemo(() => visibleEvents(state.events, state.eventArchiveOverrides));
  const ordered = createMemo(() =>
    orderedQueue(visible(), state.eventDecideOverrides, snoozed()),
  );
  const total = () => ordered().length;
  const openCount = () => openJudgmentCount(visible(), state.eventDecideOverrides);
  const archived = createMemo(() => archivedEvents(state.events, state.eventArchiveOverrides));
  // The end-page "decided" tally: DECIDED JUDGMENTS only — the exact complement
  // of `openCount` over the judgment set, so the two can never disagree (the
  // old `total - openCount` counted open awareness as "decided"). */
  const decidedJudgmentCount = () =>
    ordered().filter((e) => e.kind === "judgment" && isEventResolved(e, state.eventDecideOverrides))
      .length;
  const onClear = () => current() >= total();
  // The id of the event currently centred in the reader — highlights its row in
  // the standing list / peek. Undefined on the clear page.
  const centeredId = () => ordered()[current()]?.id;

  function pageHeight(): number {
    return scrollerRef?.clientHeight || 0;
  }

  /** The scroll offset of page `idx` — read from the page ELEMENTS, not
   * `idx × viewport`, because at `rail` the pages are card-anchored (variable
   * height) while phone pages stay full-viewport. One offset source for both. */
  function pageTop(idx: number): number {
    const el = scrollerRef?.children[idx] as HTMLElement | undefined;
    return el?.offsetTop ?? 0;
  }

  function goTo(idx: number, opts?: { instant?: boolean }): void {
    const max = total(); // clear page = total
    const clamped = Math.max(0, Math.min(idx, max));
    // The destination of an explicit navigation counts as VIEWED on arrival
    // (see the auto-read gating below) — record it before the scroll settles.
    navTargetIdx = clamped;
    const h = pageHeight();
    // jsdom has no layout (h === 0): fall back to just tracking the index.
    if (h > 0 && scrollerRef) {
      // `instant` (the store-driven re-anchor on a snapshot swap) must be the
      // literal "instant": "auto" DEFERS to the container's CSS `scroll-smooth`
      // (CSSOM View), which animated the re-anchor through pages the Owner
      // never chose to pass — the phantom-read vector.
      const behavior: ScrollBehavior = opts?.instant
        ? "instant"
        : prefersReduced()
          ? "auto"
          : "smooth";
      scrollerRef.scrollTo({ top: pageTop(clamped), behavior });
    }
    setCurrent(clamped);
  }

  // Scroll → active-page tracking: the page whose top offset is nearest the
  // scroll position (exact for uniform phone pages, correct for the
  // card-anchored variable-height rail pages). rAF-throttled.
  let scrollRAF = 0;
  function onScroll(): void {
    if (scrollRAF) return;
    scrollRAF = requestAnimationFrame(() => {
      scrollRAF = 0;
      if (!scrollerRef || pageHeight() === 0) return;
      const top = scrollerRef.scrollTop;
      const n = scrollerRef.children.length;
      let idx = 0;
      for (let i = 1; i < n; i++) {
        if (Math.abs(pageTop(i) - top) < Math.abs(pageTop(idx) - top)) idx = i;
      }
      idx = Math.min(idx, total());
      if (idx !== current()) setCurrent(idx);
    });
  }

  // ---- Awareness auto-read: dwell-or-destination gating ----------------------
  // A page converts into a read ONLY if it was genuinely VIEWED and then left.
  // "Viewed" is armed two ways: the page was the explicit DESTINATION of a
  // navigation (goTo — keys, swipe-advance, a list/peek jump, the snapshot
  // re-anchor), or it stayed centred for a dwell. Pass-through frames of a
  // smooth scroll and re-sorts/replacements under the cursor arm nothing, so
  // they can never mark (the live-reproduced phantom read).
  /** How long a page must stay centred, absent an explicit navigation, before
   * it counts as viewed. Longer than any smooth-scroll pass-through frame. */
  const VIEW_DWELL_MS = 450;

  let viewedId: number | null = null; // ARMED: this page, once left, may mark
  let dwellTimer: ReturnType<typeof setTimeout> | undefined;
  let navTargetIdx: number | null = null; // the pending goTo destination
  let prevIdx: number | null = null; // last settled page index
  let prevCenteredId: number | null = null; // id of the last centred page

  function cancelDwell(): void {
    if (dwellTimer !== undefined) {
      clearTimeout(dwellTimer);
      dwellTimer = undefined;
    }
  }
  onCleanup(cancelDwell);

  /** The Owner is now on page `idx` (event `id`). Arm it as viewed immediately
   * when it is the explicit navigation destination (consumed — a stale target
   * must never arm a later fly-by), else only after it dwells. */
  function arriveAt(idx: number, id: number | null): void {
    cancelDwell();
    if (navTargetIdx === idx) {
      navTargetIdx = null;
      viewedId = id;
      return;
    }
    viewedId = null;
    if (id === null) return;
    dwellTimer = setTimeout(() => {
      dwellTimer = undefined;
      // Arm only if this page is STILL the centred one when the dwell elapses.
      const stillIdx = untrack(current);
      const stillId = untrack(() => ordered()[stillIdx]?.id ?? null);
      if (stillIdx === idx && stillId === id) viewedId = id;
    }, VIEW_DWELL_MS);
  }

  // Whether the queue currently has any events — a memo so the reposition
  // effect below re-runs only when this FLIPS (first data arriving), not on
  // every incremental change to the set.
  const hasEvents = createMemo(() => ordered().length > 0);

  // Re-anchor on snapshot replacement (every `hello_ok`) and on first data.
  // The pager mounts against an empty store (only the clear page exists), and
  // the browser's snap-target tracking / preserved scroll offset then follows
  // that surviving clear-page node as real pages are inserted above it — which
  // is how the live stack landed on "Queue clear" with open events. The event
  // set swap makes the old offset meaningless, so position is re-derived
  // explicitly: follow the CENTRED card if it survived (a same-set resync keeps
  // the Owner's place), else land on the first needs-you card. Session
  // bookkeeping (snoozed ids, view tracking) is pruned to the surviving set so
  // nothing from a previous set (e.g. the DEV mocks) leaks into tallies.
  createEffect(() => {
    void state.eventsSnapshotSeq; // track: bumps on every hello_ok
    if (!hasEvents()) {
      // No events (pre-data / emptied set): the clear page is the only page.
      cancelDwell();
      viewedId = null;
      prevCenteredId = null;
      return;
    }
    untrack(() => {
      setSnoozed((prev) => {
        const surviving = new Set([...prev].filter((id) => state.events.some((e) => e.id === id)));
        return surviving.size === prev.size ? prev : surviving;
      });
      const list = ordered();
      const kept = prevCenteredId !== null ? list.findIndex((e) => e.id === prevCenteredId) : -1;
      const target = kept !== -1 ? kept : firstOpenIndex(list, state.eventDecideOverrides);
      goTo(target, { instant: true });
      // Settle the view bookkeeping HERE: this jump is store-driven, never a
      // "leave" — the auto-read effect below dedupes against these.
      prevIdx = target;
      prevCenteredId = list[target]?.id ?? null;
      arriveAt(target, prevCenteredId);
    });
  });

  // The one marking site. Tracks the page index AND the id centred under it:
  //  • index changed → the Owner NAVIGATED: the page being left marks read iff
  //    it was armed (viewed) and is unread awareness — round-tripped on the
  //    wire so a resync or a second device agrees (markEventRead);
  //  • same index, different id → the set SHIFTED under the cursor (an
  //    event_upsert sorting in above, a snapshot swap): never a leave — the
  //    view bookkeeping re-seeds to what is actually centred now, and whatever
  //    was armed before is discarded unmarked.
  createEffect(() => {
    const idx = current();
    const curId = ordered()[idx]?.id ?? null;
    untrack(() => {
      if (idx === prevIdx && curId === prevCenteredId) return; // already settled
      if (idx !== prevIdx) {
        const leaving = viewedId;
        if (leaving !== null && leaving !== curId) {
          const prev = state.events.find((e) => e.id === leaving);
          if (shouldMarkReadOnLeave(prev)) markEventRead(prev.id);
        }
      }
      arriveAt(idx, curId);
      prevIdx = idx;
      prevCenteredId = curId;
    });
  });

  function decide(ev: EventItem, action: string, data: unknown): void {
    if (isEventResolved(ev, state.eventDecideOverrides)) return;
    const label =
      data && typeof data === "object" && "label" in data && typeof data.label === "string"
        ? (data as { label: string }).label
        : undefined;
    // Silent decide (§6): the decided card carries its own on-screen "Undo"
    // strip, so no toast fires while it is on screen — otherwise the two Undos
    // stack. The toast is handed off below, once the card has scrolled away.
    decideEventWithUndo(ev.id, action, data, label, { silent: true });
    const from = ordered().findIndex((e) => e.id === ev.id);
    window.setTimeout(() => {
      // Undo (within the window) drops the override, so this re-check cancels a
      // still-owed advance for a card the Owner reclaimed. An archive within the
      // window also stands the advance down — the archived card's page is
      // already gone (that re-flow IS the advance) and the Archived toast owns
      // recovery, so firing here would double both the jump and the Undo.
      const live = state.events.find((e) => e.id === ev.id);
      if (
        live &&
        isEventResolved(live, state.eventDecideOverrides) &&
        !isEventArchived(live, state.eventArchiveOverrides)
      ) {
        goTo(nextOpenIndex(ordered(), from, state.eventDecideOverrides));
        // The decided card is off screen now; keep recovery reachable for the
        // rest of the undo window as a toast — exactly one Undo at any moment.
        toast(label ? `Decided · ${label}` : "Decided", {
          durationMs: Math.max(0, DECIDE_UNDO_WINDOW_MS - ADVANCE_MS),
          action: { label: "Undo", onClick: () => undoDecide(ev.id) },
        });
      }
    }, ADVANCE_MS);
  }

  function acceptRec(ev: EventItem): void {
    const pick = recommendedChoice(ev);
    if (pick) decide(ev, pick.action, { choice: pick.choice, label: pick.label });
  }

  function snooze(ev: EventItem): void {
    if (isEventResolved(ev, state.eventDecideOverrides)) return;
    setSnoozed((prev) => new Set(prev).add(ev.id));
    // Feedback + the immediate un-snooze path (§6); the standing list / peek row
    // is the durable one (jumping a snoozed row returns it too).
    toast("Snoozed to the end", {
      action: { label: "Undo", onClick: () => unsnooze(ev.id) },
    });
  }

  function unsnooze(id: number): void {
    setSnoozed((prev) => {
      if (!prev.has(id)) return prev;
      const next = new Set(prev);
      next.delete(id);
      return next;
    });
  }

  /** Jump the pager to an event by id (the standing list / peek target). A
   * snoozed row's jump also un-snoozes it — clicking a parked item in the index
   * means "bring it back" — then lands on it in its restored place. Keyboard
   * stays alive by returning focus to the reader. */
  function jumpToEvent(ev: EventItem): void {
    if (snoozed().has(ev.id)) unsnooze(ev.id);
    const idx = ordered().findIndex((e) => e.id === ev.id);
    if (idx >= 0) goTo(idx);
    rootRef?.focus();
  }

  function discuss(ev: EventItem): void {
    // Preserve the judgment across the drill-in (§4): seed the chat composer with
    // a quoted reference to it — the @name + the fork heading line — so the
    // context is never dropped on the way to Chat. `>`-quote style matches the
    // reply-quote vocabulary; the Owner types their message below it.
    // TODO(host): open scoped side chat seeded with the event card
    const prefill = `> ${ev.name} — ${eventTitle(ev)}\n\n`;
    goToChat({ composerPrefill: prefill });
  }

  // ---- keyboard mirror (scoped to the scroller; skips typing) ----
  function onKeyDown(e: KeyboardEvent): void {
    const tag = (e.target as HTMLElement | null)?.tagName ?? "";
    if (tag === "INPUT" || tag === "TEXTAREA") {
      if (e.key === "Escape") (e.target as HTMLElement).blur();
      return;
    }
    // WAI-ARIA button activation (§5): when focus is on a button / role=button /
    // link (an option, Discuss, the chevron, a list row), Space and Enter must
    // ACTIVATE it, not page the scroller — so never intercept them there.
    if (e.key === " " || e.key === "Enter" || e.key === "Spacebar") {
      const active = document.activeElement as HTMLElement | null;
      const role = active?.getAttribute("role");
      if (
        active &&
        (active.tagName === "BUTTON" ||
          active.tagName === "A" ||
          role === "button" ||
          role === "link")
      ) {
        return;
      }
    }
    if (peekOpen() && e.key === "Escape") {
      setPeekOpen(false);
      return;
    }
    if (e.key === "ArrowDown" || e.key === "j" || e.key === " ") {
      e.preventDefault();
      goTo(current() + 1);
      return;
    }
    if (e.key === "ArrowUp" || e.key === "k") {
      e.preventDefault();
      goTo(current() - 1);
      return;
    }
    if (e.key === "p") {
      setPeekOpen((v) => !v);
      return;
    }
    const ev = ordered()[current()];
    if (!ev || ev.kind !== "judgment" || isEventResolved(ev, state.eventDecideOverrides)) return;
    if (e.key === "ArrowRight") {
      e.preventDefault();
      acceptRec(ev);
      return;
    }
    if (e.key === "ArrowLeft") {
      e.preventDefault();
      snooze(ev);
      return;
    }
    const list = eventUiNodes(ev.ui).find((n) => n.type === "optionList");
    const options = (list && Array.isArray(list.options) ? list.options : []) as Record<string, unknown>[];
    const opt = options.find((o) => String(o.key ?? "").toLowerCase() === e.key.toLowerCase());
    if (opt) {
      e.preventDefault();
      decide(ev, typeof list?.action === "string" ? list.action : "choose", {
        choice: String(opt.key),
        label: String(opt.label ?? "").replace(/`/g, ""),
      });
    }
  }

  return (
    // The queue home. Phone: a single full-viewport pager column. Desktop
    // (`rail`): a two-column reader — the standing queue index (left) beside the
    // reader-measure card column (right), so the width is used by real structure
    // and the card is never a lonely centred column in a void (§1).
    <div class="flex min-h-0 flex-1 flex-col rail:flex-row">
      {/* The reader column. At `rail` it is centred and capped to a reader
          measure (~688px); the pager and the card both hug that measure, not
          the full frame. Below `rail` it is the full-width phone column. */}
      <div
        ref={setRootRef}
        class="relative flex min-h-0 flex-1 flex-col outline-none rail:items-center"
        tabindex="-1"
        aria-label="Event queue"
        data-slot="event-scroller"
      >
        <div class="relative flex min-h-0 w-full flex-1 flex-col rail:max-w-[43rem]">
          {/* Slim top pager — hugs the reader measure. Carries position + the
              surface's ONE red ("N need you") + (phone) a peek hint. On desktop
              the standing list is the whole-queue index, so the peek hint is
              hidden there. */}
          <button
            type="button"
            class="absolute inset-x-0 top-0 z-20 block w-full border-b border-border bg-background/80 text-left backdrop-blur-md rail:cursor-default"
            aria-label={atRail() ? "Queue position" : "Open queue overview"}
            tabindex={atRail() ? -1 : undefined}
            onClick={() => {
              // Phone only: the pager is the peek trigger. On desktop the standing
              // list is the always-on index, so the pager is purely informational
              // (position + the single red) and not an action.
              if (!atRail()) setPeekOpen((v) => !v);
            }}
          >
            <div class="flex items-center gap-2.5 px-4 py-2">
              <span class="text-xs font-semibold tabular-nums text-muted-foreground" data-slot="pager-pos">
                {/* The end page only claims "clear" when the same predicate the
                    red pill uses agrees — with anything still open it is just
                    the end of the queue, never a contradiction. */}
                {onClear()
                  ? openCount() > 0
                    ? "End of the queue"
                    : "Queue clear"
                  : `${current() + 1} of ${total()}`}
              </span>
              <span class="flex-1" />
              <span
                data-slot="pager-need"
                class={cn(
                  "rounded-full px-2 py-0.5 text-[0.68rem] font-bold",
                  openCount() > 0
                    ? "bg-status-danger/12 text-status-danger"
                    : "bg-status-success/12 text-status-success",
                )}
              >
                {openCount() > 0 ? `${openCount()} need you` : "all clear"}
              </span>
              {/* Phone: a lone list glyph is the peek affordance — the bar's
                  aria-label ("Open queue overview") names it; no "peek" word. */}
              <List class="size-3.5 text-muted-foreground rail:hidden" aria-hidden="true" />
            </div>
            <div class="h-0.5 bg-border">
              <div
                class="h-full rounded-r-sm bg-primary transition-[width] duration-300 ease-out"
                style={{ width: `${((current() + 1) / (total() + 1)) * 100}%` }}
              />
            </div>
          </button>

          {/* The scroll-snap pager. `overflow-anchor: none`: position is OWNED
              by the pager (goTo / the snapshot re-anchor effect) — the browser
              must never adjust the offset itself when the event set changes
              under the viewport (scroll anchoring + mandatory snap otherwise
              glue the view to the surviving clear page on a hello_ok swap). */}
          <div
            ref={setScrollerRef}
            class="absolute inset-0 snap-y snap-mandatory overflow-y-auto overflow-x-hidden scroll-smooth [overflow-anchor:none] [scrollbar-width:none] motion-reduce:scroll-auto"
            onScroll={onScroll}
          >
            <For each={ordered()}>
              {(ev) => (
                <EventPage
                  ev={ev}
                  onDecide={decide}
                  onAccept={acceptRec}
                  onSnooze={snooze}
                  onUndo={(id) => undoDecide(id)}
                  onDiscuss={discuss}
                  onAdvance={() => goTo(current() + 1)}
                />
              )}
            </For>
            <ClearPage
              decided={decidedJudgmentCount()}
              awarenessRead={ordered().filter((e) => e.kind !== "judgment" && e.read).length}
              openCount={openCount()}
              onJumpBack={() => {
                goTo(firstOpenIndex(ordered(), state.eventDecideOverrides));
                rootRef?.focus();
              }}
            />
          </div>

          {/* The phone peek overlay — the whole-queue index on phone (the
              desktop standing list replaces it, so it is gated off at `rail`). */}
          <Show when={peekOpen() && !atRail()}>
            <PeekOverview
              ordered={ordered()}
              archived={archived()}
              centeredId={centeredId()}
              snoozed={snoozed()}
              openCount={openCount()}
              onJump={(ev) => {
                setPeekOpen(false);
                jumpToEvent(ev);
              }}
              onClose={() => setPeekOpen(false)}
            />
          </Show>
        </div>
      </div>
    </div>
  );
}

/** One full-viewport page: a centred event card in a swipe wrapper, plus a
 * down-chevron advance affordance. Judgments accept a horizontal swipe. */
function EventPage(props: {
  ev: EventItem;
  onDecide: (ev: EventItem, action: string, data: unknown) => void;
  onAccept: (ev: EventItem) => void;
  onSnooze: (ev: EventItem) => void;
  onUndo: (id: number) => void;
  onDiscuss: (ev: EventItem) => void;
  onAdvance: () => void;
}) {
  let cardRef: HTMLDivElement | undefined;
  const setCardRef = (el: HTMLDivElement) => (cardRef = el);
  const [dragging, setDragging] = createSignal(false);
  const [acceptHint, setAcceptHint] = createSignal(0);
  const [snoozeHint, setSnoozeHint] = createSignal(0);

  const decided = () => isEventResolved(props.ev, state.eventDecideOverrides);
  const isJudgment = () => props.ev.kind === "judgment";
  // Motion-safe archive exit: the card fades/settles out, then the optimistic
  // sweep drops its page — the pages below shift up into the slot, which IS the
  // advance (the pager's set-shift tracking re-seeds without a phantom read).
  const { leaving, archive } = createArchiveExit(() => archiveEventWithUndo(props.ev.id));

  // Pointer-driven swipe (the accelerator layer). Vertical intent releases to
  // the pager; horizontal past threshold commits accept/snooze.
  let drag: { x0: number; y0: number; dx: number; active: boolean; id: number } | null = null;
  function onPointerDown(e: PointerEvent): void {
    if (decided() || !isJudgment()) return;
    if (e.pointerType === "mouse" && e.button !== 0) return;
    drag = { x0: e.clientX, y0: e.clientY, dx: 0, active: false, id: e.pointerId };
  }
  function onPointerMove(e: PointerEvent): void {
    if (!drag || e.pointerId !== drag.id) return;
    const dx = e.clientX - drag.x0;
    const dy = e.clientY - drag.y0;
    if (!drag.active) {
      if (Math.abs(dx) > 10 && Math.abs(dx) > Math.abs(dy) + 2) {
        drag.active = true;
        setDragging(true);
      } else if (Math.abs(dy) > 10) {
        drag = null;
        return;
      } else return;
    }
    e.preventDefault();
    drag.dx = dx;
    if (cardRef) cardRef.style.transform = `translateX(${dx}px) rotate(${dx * 0.018}deg)`;
    const t = Math.min(1, Math.abs(dx) / SWIPE_THRESHOLD);
    setAcceptHint(dx > 0 ? t : 0);
    setSnoozeHint(dx < 0 ? t : 0);
  }
  function onPointerUp(e: PointerEvent): void {
    if (!drag || e.pointerId !== drag.id) return;
    const d = drag;
    drag = null;
    setDragging(false);
    if (cardRef) cardRef.style.transform = "";
    setAcceptHint(0);
    setSnoozeHint(0);
    if (!d.active) return;
    if (d.dx > SWIPE_THRESHOLD) props.onAccept(props.ev);
    else if (d.dx < -SWIPE_THRESHOLD) props.onSnooze(props.ev);
  }

  return (
    <div class="relative h-full snap-start snap-always rail:h-auto">
      {/* Phone: a full-viewport page, card vertically centred. Desktop (`rail`):
          the page is CARD-ANCHORED — its height is the card plus a comfortable
          margin, so a short card never strands a viewport of void below it and
          the next card's top edge peeks in as the forward affordance. The
          snap math reads real element offsets (pageTop), so both models share
          one pager. */}
      <div class="flex h-full flex-col justify-center overflow-y-auto px-3.5 pb-11 pt-14 [scrollbar-width:none] rail:h-auto rail:justify-start rail:overflow-visible rail:pb-14 rail:pt-16">
        <div
          class="relative mx-auto w-full max-w-[460px] touch-pan-y rail:max-w-[35rem]"
          onPointerDown={onPointerDown}
          onPointerMove={onPointerMove}
          onPointerUp={onPointerUp}
          onPointerCancel={onPointerUp}
        >
          {/* Swipe reveal panels behind the card. */}
          <div
            class="pointer-events-none absolute inset-y-0 left-0 flex w-[46%] items-center rounded-xl bg-primary/[0.1] px-4 text-xs font-bold text-primary"
            style={{ opacity: acceptHint() }}
            aria-hidden="true"
          >
            <span class="inline-flex items-center gap-1.5">
              <ArrowRight class="size-4" /> Accept pick
            </span>
          </div>
          <div
            class="pointer-events-none absolute inset-y-0 right-0 flex w-[46%] items-center justify-end rounded-xl bg-status-attention/[0.12] px-4 text-xs font-bold text-status-attention"
            style={{ opacity: snoozeHint() }}
            aria-hidden="true"
          >
            Snooze
          </div>

          <div
            ref={setCardRef}
            class={cn(
              "relative z-[1] overflow-hidden rounded-xl border border-border bg-card shadow-sm",
              props.ev.kind !== "judgment" ? "bg-muted/30 shadow-none" : "",
              decided() ? "opacity-80" : "",
              props.ev.read && props.ev.kind !== "judgment" ? "opacity-60" : "",
              dragging() ? "shadow-lg" : "transition-transform duration-300 motion-reduce:transition-none",
              leaving() ? ARCHIVE_EXIT_CLASS : "",
            )}
          >
            {/* The needs-you accent + minimal-chrome header (handle · source ·
                kind), shared verbatim with the desktop Feed card. */}
            <EventCardHeader ev={props.ev} onArchive={archive} />

            <div class="px-3.5 pb-3.5 pt-2">
              <EventCardRenderer
                ui={props.ev.ui}
                disabled={decided()}
                onAction={(action, data) => props.onDecide(props.ev, action, data)}
              />
            </div>

            <Show when={decided()}>
              <DecidedStrip ev={props.ev} onUndo={props.onUndo} onArchive={archive} />
            </Show>
            <Show when={isJudgment() && !decided()}>
              {/* No resting gesture/keys hint — the swipe reveals its own
                  "Accept pick"/"Snooze" panels while dragging, and the keys live
                  in the `?` sheet (→ accept · ← snooze · A–Z choose). Just the
                  low-commitment Discuss escape hatch, right-aligned. */}
              <div class="flex items-center justify-end border-t border-border bg-muted/40 px-3.5 py-2.5">
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
        </div>
      </div>

      {/* Down-chevron advance affordance. */}
      <button
        type="button"
        class="absolute bottom-3 left-1/2 grid size-8 -translate-x-1/2 place-items-center rounded-full border border-border bg-card/80 text-muted-foreground backdrop-blur-sm transition-colors hover:text-foreground"
        aria-label="Next event"
        onClick={props.onAdvance}
      >
        <ChevronDown class="size-4" aria-hidden="true" />
      </button>
    </div>
  );
}

/** The end of the queue. Truly clear (nothing open) → the inbox-zero peak-end
 * reward, calm, no confetti. Anything still open → an honest "end, not clear"
 * state that AGREES with the pager's red pill (same `openCount` predicate) and
 * offers the way back up — the page never contradicts the chrome above it. */
function ClearPage(props: {
  decided: number;
  awarenessRead: number;
  openCount: number;
  onJumpBack: () => void;
}) {
  return (
    <div class="h-full snap-start snap-always" data-slot="queue-end">
      <Show
        when={props.openCount === 0}
        fallback={
          <div class="flex h-full flex-col items-center justify-center px-6 pb-11 pt-14 text-center">
            <span class="mb-4 grid size-14 place-items-center rounded-full border border-border bg-muted/60 text-muted-foreground">
              <ArrowUp class="size-6" aria-hidden="true" />
            </span>
            <div class="text-base font-semibold tracking-[-0.005em] text-foreground">
              End of the queue
            </div>
            <p class="mx-auto mt-2 max-w-[16rem] text-xs leading-relaxed text-muted-foreground">
              {props.openCount === 1
                ? "1 judgment above still needs you."
                : `${props.openCount} judgments above still need you.`}
            </p>
            <button
              type="button"
              class="mt-4 inline-flex h-8 items-center gap-1.5 rounded-md border border-border bg-card px-3.5 text-xs font-semibold text-foreground transition-colors hover:bg-muted focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/50"
              onClick={props.onJumpBack}
            >
              <ArrowUp class="size-3.5" aria-hidden="true" /> Back to the first
            </button>
          </div>
        }
      >
        <div class="flex h-full flex-col items-center justify-center px-6 pb-11 pt-14 text-center">
          <span class="mb-4 grid size-14 place-items-center rounded-full border border-status-success/25 bg-status-success/12 text-status-success">
            <CircleCheck class="size-7" aria-hidden="true" />
          </span>
          <div class="text-base font-semibold tracking-[-0.005em] text-foreground">Queue clear</div>
          <p class="mx-auto mt-2 max-w-[16rem] text-xs leading-relaxed text-muted-foreground">
            Everything that needed you is decided and posted back. The fleet has what it was waiting on.
          </p>
          <div class="mt-4 flex flex-wrap justify-center gap-2">
            <Show when={props.decided > 0}>
              <span class="rounded-full bg-status-success/12 px-2 py-0.5 text-[0.68rem] font-semibold text-status-success">
                {props.decided} decided
              </span>
            </Show>
            <Show when={props.awarenessRead > 0}>
              <span class="rounded-full bg-muted px-2 py-0.5 text-[0.68rem] font-semibold text-muted-foreground">
                {props.awarenessRead} read
              </span>
            </Show>
            <span class="rounded-full bg-primary/[0.12] px-2 py-0.5 text-[0.68rem] font-semibold text-primary">
              {props.openCount} waiting
            </span>
          </div>
          {/* The natural "what next" moment (§3): a quiet door to the agent —
              demoted (muted, no fill), never a destination-promotion. */}
          <button
            type="button"
            class="mt-6 inline-flex items-center gap-1.5 rounded-sm text-xs font-medium text-muted-foreground transition-colors hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/50"
            onClick={() => goToChat()}
          >
            Talk to the agent <ArrowRight class="size-3.5" aria-hidden="true" />
          </button>
        </div>
      </Show>
    </div>
  );
}

/** Peek: the phone's whole-queue index — a top sheet you flick down; tap a row
 * to jump. Keeps the phone pager from being a tunnel (the desktop standing list
 * is the always-on equivalent). It also hosts the queue's filter surface on
 * phone (owner addendum): a calm search + Active · Needs you · Archived(n)
 * control, so the pager itself stays a minimal decide flow and the browse /
 * filter / search lives here. Rows are the SAME `QueueRow` the standing list
 * uses; the archived filter swaps them for dense Unarchive rows. */
function PeekOverview(props: {
  ordered: EventItem[];
  archived: EventItem[];
  centeredId: number | undefined;
  snoozed: Set<number>;
  openCount: number;
  onJump: (ev: EventItem) => void;
  onClose: () => void;
}) {
  // Filter state — default `active` on every open (the sheet unmounts on close,
  // so this resets by construction; never persisted). Search narrows live.
  const [query, setQuery] = createSignal("");
  const [mode, setMode] = createSignal<QueueFilterMode>("active");

  // The rows the current live filter shows, search-narrowed (`needs-you` keeps
  // only open judgments; `active` keeps the whole visible queue).
  const rows = createMemo(() => {
    const base =
      mode() === "needs-you"
        ? props.ordered.filter((e) => isOpenJudgment(e, state.eventDecideOverrides))
        : props.ordered;
    return base.filter((e) => matchesQuery(e, query()));
  });
  const filteredArchived = createMemo(() =>
    props.archived.filter((e) => matchesQuery(e, query())),
  );

  // The Archived filter can't stand once nothing is archived — fall back so the
  // sheet never strands on an empty archived view.
  createEffect(() => {
    if (mode() === "archived" && props.archived.length === 0) setMode("active");
  });

  return (
    <div class="absolute inset-0 z-40" data-slot="event-peek">
      <button
        type="button"
        class="absolute inset-0 bg-black/35"
        aria-label="Close overview"
        onClick={props.onClose}
      />
      <div class="absolute inset-x-0 top-0 flex max-h-[82%] flex-col overflow-hidden rounded-b-xl border-b border-border bg-card shadow-xl">
        <div class="flex items-center gap-2 border-b border-border px-4 py-3">
          <div>
            <h3 class="text-sm font-semibold text-foreground">Queue</h3>
            <div class="text-[0.68rem] text-muted-foreground">
              {props.openCount > 0 ? `${props.openCount} still need you` : "all decided"}
            </div>
          </div>
          <span class="flex-1" />
          <button
            type="button"
            class="rounded-full border border-border bg-muted px-2.5 py-1 text-[0.68rem] font-semibold text-muted-foreground transition-colors hover:text-foreground"
            onClick={props.onClose}
          >
            Close
          </button>
        </div>
        {/* The filter row — the phone's home for search + Active/Needs you/
            Archived; the pager below the peek stays a minimal decide flow. */}
        <div class="border-b border-border px-3 py-2">
          <QueueFilterBar
            query={query()}
            onQueryChange={setQuery}
            mode={mode()}
            onModeChange={setMode}
            archivedCount={props.archived.length}
          />
        </div>
        <div class="overflow-y-auto p-1.5">
          <Show
            when={mode() === "archived"}
            fallback={
              <Show
                when={rows().length > 0}
                fallback={<PeekEmptyLine mode={mode()} query={query()} />}
              >
                <For each={rows()}>
                  {(ev) => (
                    <QueueRow
                      ev={ev}
                      active={ev.id === props.centeredId}
                      snoozed={props.snoozed.has(ev.id)}
                      onJump={props.onJump}
                    />
                  )}
                </For>
              </Show>
            }
          >
            <div data-slot="peek-archived">
              <Show
                when={filteredArchived().length > 0}
                fallback={
                  <div class="px-2 py-6 text-center text-xs text-muted-foreground/70">
                    {query().trim().length > 0
                      ? `No archived events match “${query().trim()}”.`
                      : "Nothing archived."}
                  </div>
                }
              >
                <ArchivedList
                  events={filteredArchived()}
                  onUnarchive={(ev) => unarchiveEvent(ev.id)}
                />
              </Show>
            </div>
          </Show>
        </div>
      </div>
    </div>
  );
}

/** One quiet line for an empty live filter in the peek (search miss / nothing
 * owed). */
function PeekEmptyLine(props: { mode: QueueFilterMode; query: string }) {
  const line = () =>
    props.query.trim().length > 0
      ? `No events match “${props.query.trim()}”.`
      : props.mode === "needs-you"
        ? "Nothing needs you right now."
        : "The queue is empty.";
  return (
    <div class="px-2 py-6 text-center text-xs text-muted-foreground/70">{line()}</div>
  );
}
