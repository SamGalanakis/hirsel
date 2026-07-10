import { ChevronLeft, X } from "lucide-solid";
import { onCleanup, onMount, Show } from "solid-js";
import { setSettingsOpen, state } from "../../store/store";

function SettingsPanel() {
  onMount(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") setSettingsOpen(false);
    };
    window.addEventListener("keydown", onKeyDown);
    onCleanup(() => window.removeEventListener("keydown", onKeyDown));
  });

  return (
    // Same responsive docking as ProcessesPanel — phone: a full-screen `fixed`
    // sheet with a back affordance; desktop (`rail`): a right-docked inspector
    // `absolute` inside ChatView's `relative` row, overlaying the right region
    // (Pings rail / Side Chat) only, never the chat measure on the left. Settings
    // is only reachable from the desktop NavRail, but the phone form is kept for
    // parity with the Processes surface.
    <div
      data-slot="settings-panel"
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
          onClick={() => setSettingsOpen(false)}
          aria-label="Close Settings"
        >
          <ChevronLeft class="size-5 rail:hidden" aria-hidden="true" />
          <X class="hidden size-4 rail:block" aria-hidden="true" />
          <span class="rail:hidden">Chat</span>
        </button>
        <h1 class="m-0 flex-1 text-center text-base font-semibold tracking-[0.01em] rail:text-left">
          Settings
        </h1>
        {/* Balances the back button so the phone title stays visually centered;
            harmless at rail width where the title left-aligns. */}
        <span class="w-[3.25rem] rail:hidden" aria-hidden="true" />
      </header>

      {/* Clearly-marked placeholder for a later pass — left-aligned and top-
          anchored (no hero medallion, no centered void): one calm sentence, then
          the disabled scaffold rows stacked directly beneath. Reads as an
          intentional placeholder, not an empty-state template. */}
      <div class="thin-scrollbar flex flex-1 flex-col gap-3 overflow-y-auto p-4">
        <p class="text-sm text-muted-foreground">
          Appearance, account, and theme settings arrive in a later pass.
        </p>

        <div class="flex flex-col gap-2">
          <div
            class="flex items-center justify-between rounded-md border border-border px-3 py-2.5 opacity-60"
            aria-disabled="true"
          >
            <div class="flex flex-col">
              <span class="text-sm text-foreground">Appearance — Theme</span>
              <span class="text-xs text-muted-foreground">Light mode coming</span>
            </div>
            <span class="shrink-0 rounded-full bg-muted px-2 py-0.5 text-[0.65rem] font-medium text-muted-foreground">
              Soon
            </span>
          </div>
          <div
            class="flex items-center justify-between rounded-md border border-border px-3 py-2.5 opacity-60"
            aria-disabled="true"
          >
            <div class="flex flex-col">
              <span class="text-sm text-foreground">About</span>
              <span class="text-xs text-muted-foreground">Version and account details</span>
            </div>
            <span class="shrink-0 rounded-full bg-muted px-2 py-0.5 text-[0.65rem] font-medium text-muted-foreground">
              Soon
            </span>
          </div>
        </div>
      </div>
    </div>
  );
}

/** Settings surface (desktop-shell): a placeholder inspector reachable from the
 * NavRail gear. Mirrors ProcessesSheet's responsive docking — a right-docked
 * inspector over the right region on desktop (a full-screen sheet on phone).
 * Mounted inside ChatView's row so the desktop dock resolves against the frame,
 * not the viewport. */
export function SettingsSheet() {
  return (
    <Show when={state.settingsOpen}>
      <SettingsPanel />
    </Show>
  );
}
