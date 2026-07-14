import { Activity, MessageSquare, PanelRight } from "lucide-solid";
import { Show } from "solid-js";
import { canvasViews, runningProcessCount } from "../store/selectors";
import { goToChat, openProcesses, showCanvas, state } from "../store/store";

// The phone queue-home standing chat bar (ADR-0012 home). A slim bar pinned at
// the bottom of the phone column so "talk to the agent" is always one tap away
// instead of hiding behind the ⋯ overflow — WITHOUT re-promoting Chat to a
// labeled tab or standing up a full tab bar. Owner's shape: compact and
// icon-led, never a jumbo "Message the agent…" text banner.
//
// Composition:
//  • a dormant composer PILL — a leading 16px chat-bubble glyph + a muted 13px
//    "Message…". It is a BUTTON, not a live field: tapping drills into Chat with
//    the real composer focused (keyboard up, via `focusComposer`). Compact 36px,
//    so it reads as a quiet door, not a loud full-width input.
//  • at most TWO quiet ghost icon buttons on the right, each shown ONLY when it
//    carries live state — the Canvas reopen affordance when a canvas view is
//    waiting, and Processes when work is running (a status-active count tint,
//    never the interrupt red). When neither is live the bar is just the pill, so
//    it never sits as standing empty chrome or a badge slot machine.
//
// Phone only (`rail:hidden` — desktop has the NavRail's Chat row); rendered by
// App only while `home === "queue"`. It is an in-flow flex sibling below the
// scroller's column, so the scroller's own `clientHeight` (its `pageHeight()`
// snap math) already shrinks by the bar's height — cards and the pager never sit
// under it, no `svh` bookkeeping required.
export function PhoneChatBar() {
  const processCount = () => runningProcessCount(state.processes);
  // The Canvas reopen affordance: a canvas view exists and isn't already the
  // one on screen (mirrors the phone overflow's `canvasAvailable`).
  const canvasWaiting = () =>
    canvasViews(state.views).length > 0 && state.rightRegion !== "canvas";

  return (
    <div
      data-slot="phone-chat-bar"
      class="flex flex-shrink-0 items-center gap-2 border-t border-border bg-card px-3 py-2 [padding-bottom:calc(0.5rem+env(safe-area-inset-bottom))] rail:hidden"
    >
      {/* The dormant composer pill — the standing "talk to the agent" door. */}
      <button
        type="button"
        data-slot="phone-chat-open"
        class="flex h-9 min-w-0 flex-1 items-center gap-2 rounded-md border border-input bg-transparent px-3 text-left transition-colors hover:bg-muted/40 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/50"
        aria-label="Message the agent"
        onClick={() => goToChat({ focusComposer: true })}
      >
        <MessageSquare class="size-4 shrink-0 text-muted-foreground/80" aria-hidden="true" />
        <span class="truncate text-[0.8125rem] text-muted-foreground/70">Message…</span>
      </button>

      {/* Canvas reopen — quiet, only while a canvas view is waiting off-screen. */}
      <Show when={canvasWaiting()}>
        <button
          type="button"
          data-slot="phone-chat-canvas"
          class="grid size-9 shrink-0 place-items-center rounded-md text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/50"
          aria-label="Open canvas"
          onClick={showCanvas}
        >
          <PanelRight class="size-5" aria-hidden="true" />
        </button>
      </Show>

      {/* Processes — only while work is running; a status-active count tint,
          never the interrupt red (One-Escalation). The chip is decorative; the
          count is carried in the aria-label for AT. */}
      <Show when={processCount() > 0}>
        <button
          type="button"
          data-slot="phone-chat-processes"
          class="relative grid size-9 shrink-0 place-items-center rounded-md text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/50"
          aria-label={`Processes, ${processCount()} running`}
          onClick={openProcesses}
        >
          <Activity class="size-5" aria-hidden="true" />
          <span
            class="absolute -right-0.5 -top-0.5 grid h-4 min-w-4 place-items-center rounded-full bg-status-active/20 px-1 text-[0.62rem] font-semibold text-status-active"
            aria-hidden="true"
          >
            {processCount() > 99 ? "99+" : processCount()}
          </span>
        </button>
      </Show>
    </div>
  );
}
