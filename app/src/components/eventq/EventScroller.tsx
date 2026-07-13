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
import { ArrowRight, Check, ChevronDown, CircleCheck, List, MessageSquare } from "lucide-solid";
import { createEffect, createMemo, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import type { EventItem } from "../../protocol";
import { cn } from "@/lib/utils";
import { decideEventWithUndo, undoDecide } from "../../lib/event-decide";
import { seedMockEvents } from "../../lib/mock-events";
import {
  eventTitle,
  eventUiNodes,
  isEventResolved,
  openJudgmentCount,
  orderedQueue,
} from "../../store/selectors";
import { dispatch, goToChatDrillIn, state } from "../../store/store";
import { EventCardRenderer } from "../../views/EventCardRenderer";
import { awarenessToAutoRead, nextOpenIndex } from "./queue";

/** How long the decided card lingers (confirmation + Undo reachable) before the
 * pager auto-advances to the next open event. Undo within the window cancels it
 * (the advance re-checks that the event is still decided at fire time). */
const ADVANCE_MS = 1150;

/** Horizontal drag past this (px) commits the swipe: → accepts the pick, ←
 * snoozes to the tail. Below it the card springs back. */
const SWIPE_THRESHOLD = 82;

const KIND_LABEL: Record<string, string> = { judgment: "Judgment", summary: "Summary", info: "Info" };

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

  const ordered = createMemo(() =>
    orderedQueue(state.events, state.eventDecideOverrides, snoozed()),
  );
  const total = () => ordered().length;
  const openCount = () => openJudgmentCount(state.events, state.eventDecideOverrides);
  const onClear = () => current() >= total();

  function pageHeight(): number {
    return scrollerRef?.clientHeight || 0;
  }

  function goTo(idx: number): void {
    const max = total(); // clear page = total
    const clamped = Math.max(0, Math.min(idx, max));
    const h = pageHeight();
    // jsdom has no layout (h === 0): fall back to just tracking the index.
    if (h > 0 && scrollerRef) {
      scrollerRef.scrollTo({ top: clamped * h, behavior: prefersReduced() ? "auto" : "smooth" });
    }
    setCurrent(clamped);
  }

  // Scroll → active-page tracking + awareness auto-read (scrolled PAST, never
  // while centred). rAF-throttled so it doesn't thrash on a fling.
  let scrollRAF = 0;
  function onScroll(): void {
    if (scrollRAF) return;
    scrollRAF = requestAnimationFrame(() => {
      scrollRAF = 0;
      const h = pageHeight();
      if (!scrollerRef || h === 0) return;
      const idx = Math.max(0, Math.min(Math.round(scrollerRef.scrollTop / h), total()));
      if (idx !== current()) setCurrent(idx);
    });
  }

  // Auto-read runs off the tracked index (works in tests that set current
  // directly too): any awareness event now behind the cursor and still unread
  // gets an optimistic read flip.
  createEffect(() => {
    for (const e of awarenessToAutoRead(ordered(), current())) {
      dispatch({ type: "event_read_local", eventId: e.id });
    }
  });

  function decide(ev: EventItem, action: string, data: unknown): void {
    if (isEventResolved(ev, state.eventDecideOverrides)) return;
    const label =
      data && typeof data === "object" && "label" in data && typeof data.label === "string"
        ? (data as { label: string }).label
        : undefined;
    decideEventWithUndo(ev.id, action, data, label);
    const from = ordered().findIndex((e) => e.id === ev.id);
    window.setTimeout(() => {
      // Undo (within the window) drops the override, so this re-check cancels a
      // still-owed advance for a card the Owner reclaimed.
      const live = state.events.find((e) => e.id === ev.id);
      if (live && isEventResolved(live, state.eventDecideOverrides)) {
        goTo(nextOpenIndex(ordered(), from, state.eventDecideOverrides));
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
  }

  function discuss(): void {
    // Chat is the drill-in (ADR-0012). Host wiring will open the judgment's Side
    // Chat directly; for now this lands in the chat shell.
    goToChatDrillIn();
  }

  // ---- keyboard mirror (scoped to the scroller; skips typing) ----
  function onKeyDown(e: KeyboardEvent): void {
    const tag = (e.target as HTMLElement | null)?.tagName ?? "";
    if (tag === "INPUT" || tag === "TEXTAREA") {
      if (e.key === "Escape") (e.target as HTMLElement).blur();
      return;
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
    <div
      ref={setRootRef}
      class="relative flex min-h-0 flex-1 flex-col outline-none"
      tabindex="-1"
      aria-label="Event queue"
      data-slot="event-scroller"
    >
      {/* Slim top pager — always over the current card. Tap to peek the whole
          queue. Carries position + the ONE red ("N need you") + a peek hint. */}
      <button
        type="button"
        class="absolute inset-x-0 top-0 z-20 block w-full border-b border-border bg-background/80 text-left backdrop-blur-md"
        aria-label="Open queue overview"
        onClick={() => setPeekOpen((v) => !v)}
      >
        <div class="flex items-center gap-2.5 px-4 py-2">
          <span class="text-xs font-semibold tabular-nums text-muted-foreground" data-slot="pager-pos">
            {onClear() ? "Queue clear" : `${current() + 1} of ${total()}`}
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
          <span class="inline-flex items-center gap-1 text-[0.62rem] font-semibold text-muted-foreground">
            peek
            <List class="size-3.5" aria-hidden="true" />
          </span>
        </div>
        <div class="h-0.5 bg-border">
          <div
            class="h-full rounded-r-sm bg-primary transition-[width] duration-300 ease-out"
            style={{ width: `${((current() + 1) / (total() + 1)) * 100}%` }}
          />
        </div>
      </button>

      {/* The scroll-snap pager. */}
      <div
        ref={setScrollerRef}
        class="absolute inset-0 snap-y snap-mandatory overflow-y-auto overflow-x-hidden scroll-smooth [scrollbar-width:none] motion-reduce:scroll-auto"
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
        <ClearPage decided={total() - openCount()} awarenessRead={ordered().filter((e) => e.kind !== "judgment" && e.read).length} />
      </div>

      <Show when={peekOpen()}>
        <PeekOverview
          ordered={ordered()}
          current={current()}
          openCount={openCount()}
          onJump={(idx) => {
            setPeekOpen(false);
            goTo(idx);
          }}
          onClose={() => setPeekOpen(false)}
        />
      </Show>
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
  onDiscuss: () => void;
  onAdvance: () => void;
}) {
  let cardRef: HTMLDivElement | undefined;
  const setCardRef = (el: HTMLDivElement) => (cardRef = el);
  const [dragging, setDragging] = createSignal(false);
  const [acceptHint, setAcceptHint] = createSignal(0);
  const [snoozeHint, setSnoozeHint] = createSignal(0);

  const decided = () => isEventResolved(props.ev, state.eventDecideOverrides);
  const isJudgment = () => props.ev.kind === "judgment";

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
    <div class="relative h-full snap-start snap-always">
      <div class="flex h-full flex-col justify-center overflow-y-auto px-3.5 pb-11 pt-14 [scrollbar-width:none]">
        <div
          class="relative mx-auto w-full max-w-[460px] touch-pan-y"
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
            )}
          >
            {/* The needs-you accent: a hairline-thin indigo edge (the one accent),
                as a strip rather than a heavy left border. */}
            <Show when={isJudgment() && !decided()}>
              <span class="absolute inset-y-0 left-0 w-0.5 bg-primary" aria-hidden="true" />
            </Show>
            {/* Header — minimal chrome: handle · source · kind. No wait/cost/turns. */}
            <div class="flex flex-wrap items-center gap-x-2 gap-y-1 px-3.5 pt-3">
              <span class="font-mono text-xs font-medium text-primary">{props.ev.name}</span>
              <span class="text-[0.68rem] font-medium text-muted-foreground/80">
                · <span class="font-semibold text-muted-foreground">{props.ev.source.ref ?? props.ev.source.kind}</span>
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

            <div class="px-3.5 pb-3.5 pt-2">
              <EventCardRenderer
                ui={props.ev.ui}
                disabled={decided()}
                onAction={(action, data) => props.onDecide(props.ev, action, data)}
              />
            </div>

            <Show when={decided()}>
              <DecidedStrip ev={props.ev} onUndo={props.onUndo} />
            </Show>
            <Show when={isJudgment() && !decided()}>
              <div class="flex items-center gap-2.5 border-t border-border bg-muted/40 px-3.5 py-2.5">
                <span class="inline-flex items-center gap-1.5 text-[0.62rem] font-medium text-muted-foreground/80">
                  <span class="inline-flex items-center gap-0.5 text-muted-foreground/60">
                    <ArrowRight class="size-3" aria-hidden="true" />
                  </span>
                  <span>
                    <b class="font-bold text-muted-foreground">swipe</b> → accept · ← snooze
                  </span>
                </span>
                <span class="flex-1" />
                <button
                  type="button"
                  class="inline-flex items-center gap-1 rounded-sm text-xs font-semibold text-primary transition-colors hover:text-primary/80"
                  onClick={props.onDiscuss}
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

/** The interaction-back confirmation: a green check + the posted payload + a
 * brief Undo, replacing the card body once decided. */
function DecidedStrip(props: { ev: EventItem; onUndo: (id: number) => void }) {
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

/** The inbox-zero end state — the peak-end reward. Calm, no confetti. */
function ClearPage(props: { decided: number; awarenessRead: number }) {
  return (
    <div class="h-full snap-start snap-always">
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
            0 waiting
          </span>
        </div>
      </div>
    </div>
  );
}

/** Peek: a top sheet overview of the whole queue; tap a row to jump. Keeps the
 * pager from being a tunnel. */
function PeekOverview(props: {
  ordered: EventItem[];
  current: number;
  openCount: number;
  onJump: (idx: number) => void;
  onClose: () => void;
}) {
  return (
    <div class="absolute inset-0 z-40" data-slot="event-peek">
      <button
        type="button"
        class="absolute inset-0 bg-black/35"
        aria-label="Close overview"
        onClick={props.onClose}
      />
      <div class="absolute inset-x-0 top-0 flex max-h-[78%] flex-col overflow-hidden rounded-b-xl border-b border-border bg-card shadow-xl">
        <div class="flex items-center gap-2 border-b border-border px-4 py-3">
          <div>
            <h3 class="text-sm font-semibold text-foreground">Queue</h3>
            <div class="text-[0.68rem] text-muted-foreground">
              {props.openCount > 0 ? `${props.openCount} still need you · tap to jump` : "all decided · tap to revisit"}
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
        <div class="overflow-y-auto p-1.5">
          <For each={props.ordered}>
            {(ev, i) => {
              const resolved = isEventResolved(ev, state.eventDecideOverrides);
              return (
                <button
                  type="button"
                  aria-label={`Jump to ${ev.name}: ${eventTitle(ev)}`}
                  class={cn(
                    "grid w-full grid-cols-[1fr_auto] items-center gap-3 rounded-md border border-transparent px-2 py-2 text-left transition-colors hover:bg-muted/60",
                    i() === props.current ? "border-border bg-muted/60" : "",
                  )}
                  onClick={() => props.onJump(i())}
                >
                  <span class="min-w-0">
                    <span class="block font-mono text-[0.68rem] font-medium text-primary">{ev.name}</span>
                    <span class="block truncate text-xs text-foreground">{eventTitle(ev)}</span>
                  </span>
                  <span class="text-[0.62rem] font-bold">
                    <Show
                      when={resolved}
                      fallback={
                        <Show
                          when={ev.kind === "judgment"}
                          fallback={
                            <span class={ev.read ? "text-muted-foreground/70" : "text-muted-foreground"}>
                              {ev.read ? "read" : "new"}
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
            }}
          </For>
        </div>
      </div>
    </div>
  );
}
