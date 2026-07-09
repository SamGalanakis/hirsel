import { ChevronLeft } from "lucide-solid";
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
    <div class="fixed inset-0 z-40 flex flex-col bg-background pb-[env(safe-area-inset-bottom)]">
      <header class="flex flex-shrink-0 items-center gap-2 border-b border-border px-2 py-3 pt-[calc(env(safe-area-inset-top)+0.75rem)]">
        <button
          type="button"
          class="flex items-center gap-0.5 rounded-md px-2 py-1 text-sm text-foreground transition-colors hover:bg-muted"
          onClick={() => setProcessesOpen(false)}
          aria-label="Back to Chat"
        >
          <ChevronLeft class="size-5" aria-hidden="true" />
          Chat
        </button>
        <h1 class="m-0 flex-1 text-center text-base font-semibold tracking-[0.01em]">
          Processes
        </h1>
        {/* Balances the back button so the title stays visually centered. */}
        <span class="w-[3.25rem]" aria-hidden="true" />
      </header>
      <ProcessesView />
    </div>
  );
}

/** Full-screen panel replacing the old Processes tab (spec [P1]: "full-screen
 * panel/sheet with a back affordance — keep it simple"). Covers the whole app
 * (header included) rather than sharing it, since there is no longer a tab
 * strip to hold a persistent title — the panel supplies its own. */
export function ProcessesSheet() {
  return (
    <Show when={state.processesOpen}>
      <ProcessesPanel />
    </Show>
  );
}
