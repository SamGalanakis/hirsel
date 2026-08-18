import { Activity, ChevronLeft } from "lucide-solid";
import { onMount, Show } from "solid-js";
import {
  createFocusTrap,
  createMediaFlag,
  processesRestoreTarget,
} from "../../lib/focus";
import { closeRightRegion, state } from "../../store/store";
import { PaneHeader } from "../ui/PaneHeader";
import { ProcessesView } from "./ProcessesView";

// The Processes surface as one of the exclusive right-region panes (v2.3).
// Below `rail` it is a full-screen `fixed` sheet over the task world (a true modal —
// Tab is trapped, `aria-modal` honest); at/above `rail` it is an in-flow
// `<aside>` docked at the right edge of the frame, sharing the utility width
// token and h-12 header datum, never covering the standing conversation. Only mounted while it
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
      // Resolve this again on teardown: Processes may have opened from a
      // shortcut as a desktop inspector and become a phone sheet while open.
      restoreTo: () => phone() ? processesRestoreTarget() : undefined,
    });
  });

  return (
    // Phone: a full-screen `fixed` modal sheet with a back affordance. Desktop
    // (`rail`): an in-flow right-edge aside (relative, shared width) — one slot,
    // the inactive panes unmounted, so nothing clips behind it and the obscured
    // pane can't hold focus.
    // A11y (spec item 1): at `rail` (desktop, in-flow) this is a NON-modal
    // inspector, so it is `role="complementary"` with Tab free; only under the
    // phone media flag (a full-screen sheet) is it a true `role="dialog"` +
    // `aria-modal` with Tab trapped. The heading it is labelled by follows suit
    // (the visible one for each width). Motion (spec item 3): the phone sheet
    // slides up 200ms; the desktop pane-swap does a subtler right-slide + fade
    // 150ms so a same-width pane change reads as a transition, not a flash.
    <div
      ref={(node) => { panelRef = node; }}
      tabindex={-1}
      data-slot="processes-panel"
      role={phone() ? "dialog" : "complementary"}
      aria-modal={phone() ? "true" : undefined}
      aria-labelledby={phone() ? "processes-panel-heading" : "processes-pane-title"}
      class="fixed inset-0 z-40 flex flex-col bg-background outline-none pb-[env(safe-area-inset-bottom)]
        rail:relative rail:inset-auto rail:z-auto rail:min-h-0 rail:w-[clamp(340px,38vw,440px)] rail:shrink-0 rail:border-l rail:border-border rail:pb-0
        motion-safe:animate-in motion-safe:fade-in motion-safe:slide-in-from-bottom motion-safe:duration-200
        motion-safe:rail:slide-in-from-bottom-0 motion-safe:rail:slide-in-from-right-2 motion-safe:rail:duration-150"
    >
      {/* Phone header (rail:hidden): a back affordance to Tasks, safe-area padded,
          centered title. */}
      <header class="flex flex-shrink-0 items-center gap-2 border-b border-border px-2 py-3 pt-[calc(env(safe-area-inset-top)+0.75rem)] rail:hidden">
        <button
          type="button"
          class="flex min-h-11 items-center gap-0.5 rounded-md px-2 text-sm text-foreground transition-colors hover:bg-muted"
          onClick={closeRightRegion}
          aria-label="Close Processes"
        >
          <ChevronLeft class="size-5" aria-hidden="true" />
          <span>Tasks</span>
        </button>
        <h1
          id="processes-panel-heading"
          class="m-0 flex-1 text-center text-base font-semibold tracking-[0.01em]"
        >
          Processes
        </h1>
        {/* Balances the back button so the phone title stays visually centered. */}
        <span class="w-[3.25rem]" aria-hidden="true" />
      </header>
      {/* Desktop header (hidden rail:flex): the shared PaneHeader — one datum,
          trailing × close with the sibling focus-visible ring. */}
      <PaneHeader
        class="hidden rail:flex"
        icon={<Activity class="size-4 shrink-0 text-muted-foreground" aria-hidden="true" />}
        title="Processes"
        titleId="processes-pane-title"
        onClose={closeRightRegion}
        closeLabel="Close Processes"
      />
      <ProcessesView />
    </div>
  );
}

/** Processes is a modal sheet on phone and an in-flow inspector on desktop. */
export function ProcessesSheet() {
  return (
    <Show when={state.rightRegion === "processes"}>
      <ProcessesPanel />
    </Show>
  );
}
