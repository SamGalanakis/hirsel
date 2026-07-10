import { ChevronLeft, X } from "lucide-solid";
import { onCleanup, onMount, Show } from "solid-js";
import { setProcessesOpen, state } from "../../store/store";
import { ProcessesView } from "./ProcessesView";

function ProcessesPanel() {
  onMount(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") setProcessesOpen(false);
    };
    window.addEventListener("keydown", onKeyDown);
    onCleanup(() => window.removeEventListener("keydown", onKeyDown));
  });

  return (
    // Phone: a full-screen `fixed` sheet with a back affordance (unchanged).
    // Desktop (`rail`): a right-docked inspector `absolute` inside ChatView's
    // `relative` row — it overlays the right region (Pings rail / Side Chat)
    // only, never the chat measure on the left, and its bounded width keeps the
    // ProcessRow status pill next to its label instead of flung across the void.
    <div
      data-slot="processes-panel"
      class="fixed inset-0 z-40 flex flex-col bg-background pb-[env(safe-area-inset-bottom)]
        rail:absolute rail:left-auto rail:z-30 rail:w-[420px] rail:border-l rail:border-border rail:pb-0"
    >
      {/* h-12 on desktop (rail:) so the inspector header shares the top-bar
          datum with the center chat header + Pings rail; phone keeps its
          safe-area padding. */}
      <header class="flex flex-shrink-0 items-center gap-2 border-b border-border px-2 py-3 pt-[calc(env(safe-area-inset-top)+0.75rem)] rail:h-12 rail:py-0">
        <button
          type="button"
          class="flex items-center gap-0.5 rounded-md px-2 py-1 text-sm text-foreground transition-colors hover:bg-muted"
          onClick={() => setProcessesOpen(false)}
          aria-label="Close Processes"
        >
          <ChevronLeft class="size-5 rail:hidden" aria-hidden="true" />
          <X class="hidden size-4 rail:block" aria-hidden="true" />
          <span class="rail:hidden">Chat</span>
        </button>
        <h1 class="m-0 flex-1 text-center text-base font-semibold tracking-[0.01em] rail:text-left">
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

/** Processes surface (spec [P1] / desktop-shell): a full-screen sheet with a
 * back affordance on phone; a right-docked inspector over the right region on
 * desktop. Mounted inside ChatView's row so the desktop dock resolves against
 * the frame, not the viewport (ultrawide margins stay clear of it). */
export function ProcessesSheet() {
  return (
    <Show when={state.processesOpen}>
      <ProcessesPanel />
    </Show>
  );
}
