import { ChevronLeft, X } from "lucide-solid";
import { onMount, Show } from "solid-js";
import { createFocusTrap, createMediaFlag } from "../../lib/focus";
import { closeRightRegion, state } from "../../store/store";
import { ProcessesView } from "./ProcessesView";

// The Processes surface as one of the exclusive right-region panes (v2.3).
// Below `rail` it is a full-screen `fixed` sheet over the chat (a true modal —
// Tab is trapped, `aria-modal` honest); at/above `rail` it is an in-flow
// `<aside>` docked at the right edge of the frame, sharing the Pings-rail width
// token and h-12 header datum, never covering the chat. Only mounted while it
// owns the region, so it (and its focusables) UNMOUNT the moment another pane
// takes the region.
const RAIL_MQ = "(min-width: 1100px)";

function ProcessesPanel() {
  let panelRef: HTMLDivElement | undefined;
  const phone = createMediaFlag("(max-width: 1099.98px)");

  onMount(() => {
    createFocusTrap(() => panelRef, {
      onEscape: closeRightRegion,
      trapTab: () => !window.matchMedia(RAIL_MQ).matches,
    });
  });

  return (
    // Phone: a full-screen `fixed` modal sheet with a back affordance. Desktop
    // (`rail`): an in-flow right-edge aside (relative, shared width) — one slot,
    // the inactive panes unmounted, so nothing clips behind it and the obscured
    // pane can't hold focus.
    <div
      ref={panelRef}
      tabindex={-1}
      data-slot="processes-panel"
      role="dialog"
      aria-modal={phone() ? "true" : undefined}
      aria-labelledby="processes-panel-heading"
      class="fixed inset-0 z-40 flex flex-col bg-background outline-none pb-[env(safe-area-inset-bottom)]
        rail:relative rail:inset-auto rail:z-auto rail:min-h-0 rail:w-[clamp(340px,38vw,440px)] rail:shrink-0 rail:border-l rail:border-border rail:pb-0"
    >
      {/* h-12 on desktop (rail:) so the inspector header shares the top-bar
          datum with the center chat header + Pings rail; phone keeps its
          safe-area padding. */}
      <header class="flex flex-shrink-0 items-center gap-2 border-b border-border px-2 py-3 pt-[calc(env(safe-area-inset-top)+0.75rem)] rail:h-12 rail:py-0">
        <button
          type="button"
          class="flex items-center gap-0.5 rounded-md px-2 py-1 text-sm text-foreground transition-colors hover:bg-muted"
          onClick={closeRightRegion}
          aria-label="Close Processes"
        >
          <ChevronLeft class="size-5 rail:hidden" aria-hidden="true" />
          <X class="hidden size-4 rail:block" aria-hidden="true" />
          <span class="rail:hidden">Chat</span>
        </button>
        <h1
          id="processes-panel-heading"
          class="m-0 flex-1 text-center text-base font-semibold tracking-[0.01em] rail:text-left"
        >
          Processes
        </h1>
        {/* Balances the back button so the phone title stays visually centered;
            harmless at rail width where the title left-aligns. */}
        <span class="w-[3.25rem] rail:hidden" aria-hidden="true" />
      </header>
      <ProcessesView />
    </div>
  );
}

/** Processes surface (single-owner right region / desktop-shell): a full-screen
 * modal sheet with a back affordance on phone; an in-flow right-edge inspector
 * on desktop. Mounted inside ChatView's row so the desktop aside sits at the
 * right of the frame (ultrawide margins stay clear of it). */
export function ProcessesSheet() {
  return (
    <Show when={state.rightRegion === "processes"}>
      <ProcessesPanel />
    </Show>
  );
}
