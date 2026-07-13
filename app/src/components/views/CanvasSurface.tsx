import { ChevronLeft, PanelRight, X } from "lucide-solid";
import { For, onMount, Show } from "solid-js";
import { createFocusTrap, createMediaFlag } from "../../lib/focus";
import { canvasViews } from "../../store/selectors";
import { closeRightRegion, showCanvas, state } from "../../store/store";
import { ViewRenderer } from "../../views/ViewRenderer";

// Canvas: the shared right-context surface for `canvas`-placed generative
// views. On desktop (`rail`) it is an in-flow right column that takes the slot
// otherwise held by the Pings rail (precedence handled in ChatView); on phone
// it is a full-screen `fixed` sheet over the chat. The newest canvas view leads
// (auto-surfaced — ChatView opens the surface when a new one arrives); older
// canvas views stack beneath so nothing an agent drew is lost.

const RAIL_MQ = "(min-width: 1100px)";

/** True when the Canvas holds the exclusive right region and has something to
 * show. The single-owner enum makes the Side-Chat/Processes/Settings precedence
 * automatic — Canvas shows only while it owns the region. Same predicate drives
 * the desktop in-flow rail and the phone full-screen sheet. */
export function canvasActive(): boolean {
  return state.rightRegion === "canvas" && canvasViews(state.views).length > 0;
}

/** Newest-first canvas views. */
function orderedCanvasViews() {
  return canvasViews(state.views).slice().reverse();
}

function CanvasBody() {
  return (
    <div class="thin-scrollbar flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto p-3">
      <For each={orderedCanvasViews()}>
        {(v) => (
          <ViewRenderer spec={v.spec} instanceId={v.instance_id} placement={v.placement} />
        )}
      </For>
    </div>
  );
}

/** The desktop in-flow Canvas column. Mirrors the Pings rail's frame (width,
 * left hairline, h-12 header) so the shared right region reads as one slot. */
export function CanvasRail() {
  return (
    <Show when={canvasActive()}>
      <aside
        data-slot="canvas-rail"
        class="hidden min-h-0 w-[clamp(340px,38vw,440px)] shrink-0 flex-col border-l border-border bg-background rail:flex"
        aria-label="Canvas"
      >
        <div class="flex h-12 flex-shrink-0 items-center gap-2 border-b border-border px-3">
          <PanelRight class="size-4 shrink-0 text-muted-foreground" aria-hidden="true" />
          <span class="min-w-0 flex-1 truncate text-xs font-medium text-foreground">Canvas</span>
          <button
            type="button"
            class="-mr-1 grid size-8 place-items-center rounded text-muted-foreground transition-colors hover:text-foreground"
            aria-label="Close Canvas"
            onClick={closeRightRegion}
          >
            <X class="size-4" aria-hidden="true" />
          </button>
        </div>
        <CanvasBody />
      </aside>
    </Show>
  );
}

function CanvasPhonePanel() {
  let panelRef: HTMLDivElement | undefined;
  // Full-screen on phone (a true modal dialog: trap Tab, aria-modal); the
  // desktop presentation is the in-flow CanvasRail above, so this phone panel is
  // `rail:hidden`. `phone()` gates the modal semantics to that width.
  const phone = createMediaFlag("(max-width: 1099.98px)");
  onMount(() => {
    createFocusTrap(() => panelRef, {
      onEscape: closeRightRegion,
      trapTab: () => !window.matchMedia(RAIL_MQ).matches,
    });
  });
  return (
    <div
      ref={panelRef}
      tabindex={-1}
      data-slot="canvas-sheet"
      role="dialog"
      aria-modal={phone() ? "true" : undefined}
      aria-labelledby="canvas-sheet-heading"
      class="fixed inset-0 z-40 flex flex-col bg-background outline-none pb-[env(safe-area-inset-bottom)] rail:hidden"
    >
      <header class="flex flex-shrink-0 items-center gap-2 border-b border-border px-2 py-3 pt-[calc(env(safe-area-inset-top)+0.75rem)]">
        <button
          type="button"
          class="flex items-center gap-0.5 rounded-md px-2 py-1 text-sm text-foreground transition-colors hover:bg-muted"
          onClick={closeRightRegion}
          aria-label="Close Canvas"
        >
          <ChevronLeft class="size-5" aria-hidden="true" />
          <span>Chat</span>
        </button>
        <h1
          id="canvas-sheet-heading"
          class="m-0 flex-1 text-center text-base font-semibold tracking-[0.01em]"
        >
          Canvas
        </h1>
        <span class="w-[3.25rem]" aria-hidden="true" />
      </header>
      <CanvasBody />
    </div>
  );
}

/** The phone Canvas sheet (full-screen, `rail:hidden`). */
export function CanvasSheet() {
  return (
    <Show when={canvasActive()}>
      <CanvasPhonePanel />
    </Show>
  );
}

/** Reopen affordance: appears whenever canvas views exist but the Canvas does
 * NOT own the right region — so a closed Canvas (or one evicted because the user
 * opened another pane) stays reachable without the auto-surface ever stealing an
 * occupied slot. Used in both the desktop center-pane header and the phone
 * overflow. */
export function CanvasButton() {
  return (
    <Show when={canvasViews(state.views).length > 0 && state.rightRegion !== "canvas"}>
      <button
        type="button"
        class="flex items-center gap-1.5 rounded-full px-2 py-1 text-xs font-medium text-muted-foreground transition-colors hover:text-foreground"
        onClick={showCanvas}
        aria-label="Open Canvas"
      >
        <PanelRight class="size-4" aria-hidden="true" />
        <span>Canvas</span>
      </button>
    </Show>
  );
}
