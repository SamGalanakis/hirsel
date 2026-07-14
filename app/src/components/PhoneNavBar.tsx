import { Layers, MessageSquare } from "lucide-solid";
import { Show } from "solid-js";
import { openJudgmentCount, visibleEvents } from "../store/selectors";
import { goToChat, goToQueue, state } from "../store/store";

// The phone bottom navigation bar (two-way nav). A slim phone-only tab bar
// pinned at the foot of the column that lets the Owner move BOTH ways between
// the two phone surfaces — the event Feed (the queue home) and Chat — so he can
// always get back. It replaces the old dormant composer pill: the bar is
// NAVIGATION, never input. Chat's real composer already lives on the chat
// surface, so the bar never duplicates a text field.
//
// Composition — exactly two ghost icon buttons, evenly spread:
//  • Feed  — the queue home. A calm `Layers` glyph, matching the desktop
//    NavRail's Queue row. `goToQueue`.
//  • Chat  — the drill-in chat shell. The `MessageSquare` bubble. `goToChat`
//    (a plain navigation — no composer autofocus; the bar isn't a composer).
//
// The active destination gets the NavRail's active-row treatment — a MUTED fill
// + full-strength foreground (NOT indigo, NOT a red/filled pill), honoring the
// One-Escalation + indigo-is-rare rules — plus a truthful `aria-current`. A tiny
// micro-label under each glyph names the destination (the Owner framed these as
// "chat / feed" buttons); the labels stay calm (~0.66rem, muted → foreground on
// active).
//
// Cross-surface awareness: the Feed button carries the queue's needs-you count
// ONLY while we're away in Chat, in the MUTED count style (never the interrupt
// red — the single red stays the queue's own "N need you" pager pill). It is
// hidden on the Feed home itself, where it would merely duplicate that pager
// (mirrors the desktop NavRail Queue badge exactly).
//
// Phone only (`rail:hidden` — desktop has the NavRail); rendered by App on BOTH
// homes. On chat it sits BELOW the composer, the standard tab-bar placement. It
// is an in-flow flex sibling below the surface, so the scroller/chat layouts'
// own `clientHeight` already shrinks by the bar's height — no `svh` bookkeeping.
function clamp99(n: number): string {
  return n > 99 ? "99+" : String(n);
}

export function PhoneNavBar() {
  const onQueue = () => state.home === "queue";
  const onChat = () => state.home === "chat";
  // Counts run on the archive-filtered set (contract v1), matching every other
  // queue surface — an archived event never contributes to the badge.
  const feedNeedsYou = () =>
    openJudgmentCount(
      visibleEvents(state.events, state.eventArchiveOverrides),
      state.eventDecideOverrides,
    );

  return (
    <nav
      data-slot="phone-nav-bar"
      // "Main" (not "Primary"): the desktop NavRail already owns the "Primary"
      // navigation landmark, and the two never co-display — a distinct name keeps
      // the landmark list unambiguous for AT rather than doubling "Primary".
      aria-label="Main"
      class="flex flex-shrink-0 items-stretch gap-1 border-t border-border bg-card px-3 pt-1 [padding-bottom:calc(0.25rem+env(safe-area-inset-bottom))] rail:hidden"
    >
      {/* Feed — the queue home. Active on the queue surface. */}
      <button
        type="button"
        data-slot="phone-nav-feed"
        class="flex min-w-0 flex-1 flex-col items-center justify-center gap-0.5 rounded-md py-1.5 text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/50"
        classList={{ "bg-muted text-foreground": onQueue() }}
        aria-label="Feed"
        aria-current={onQueue() ? "page" : undefined}
        onClick={goToQueue}
      >
        <span class="relative">
          <Layers class="size-5 shrink-0" aria-hidden="true" />
          {/* Muted needs-you count — only while away in Chat, never red. */}
          <Show when={!onQueue() && feedNeedsYou() > 0}>
            <span
              data-slot="phone-nav-feed-badge"
              class="absolute -right-2.5 -top-1 grid h-4 min-w-4 place-items-center rounded-full bg-muted-foreground px-1 text-[0.6rem] font-bold text-primary-foreground"
              aria-hidden="true"
            >
              {clamp99(feedNeedsYou())}
            </span>
          </Show>
        </span>
        <span class="text-[0.66rem] font-medium leading-none tracking-[0.02em]">Feed</span>
      </button>

      {/* Chat — the drill-in shell. Active on the chat surface. */}
      <button
        type="button"
        data-slot="phone-nav-chat"
        class="flex min-w-0 flex-1 flex-col items-center justify-center gap-0.5 rounded-md py-1.5 text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/50"
        classList={{ "bg-muted text-foreground": onChat() }}
        aria-label="Chat"
        aria-current={onChat() ? "page" : undefined}
        onClick={() => goToChat()}
      >
        <MessageSquare class="size-5 shrink-0" aria-hidden="true" />
        <span class="text-[0.66rem] font-medium leading-none tracking-[0.02em]">Chat</span>
      </button>
    </nav>
  );
}
