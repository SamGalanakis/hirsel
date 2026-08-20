import {
  Activity,
  MoreHorizontal,
  PanelRight,
  Settings as SettingsIcon,
} from "lucide-solid";
import { Show } from "solid-js";
import { canvasViews, runningProcessCount } from "../store/selectors";
import { openProcesses, openSettings, showCanvas, state } from "../store/store";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "./ui/dropdown-menu";

// The home shell's single overflow: with the header bar gone, this quiet ⋯ is
// the only standing chrome, and every utility — Processes, Canvas, Settings —
// is reachable from it. Processes leads the menu and carries its own running
// count, so live work still announces itself without a dedicated button in a
// bar that no longer exists.

export function PhoneOverflowMenu() {
  const canvasAvailable = () =>
    canvasViews(state.views).length > 0 && state.rightRegion !== "canvas";
  const processCount = () => runningProcessCount(state.processes);

  return (
    <DropdownMenu placement="bottom-end" gutter={6}>
      <DropdownMenuTrigger
        data-slot="phone-overflow-trigger"
        class="relative flex size-8 shrink-0 items-center justify-center rounded-full text-muted-foreground/70 outline-none transition-colors hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring [@media(pointer:coarse)]:size-11"
        aria-label={
          processCount() > 0
            ? `More actions, ${processCount()} processes running`
            : "More actions"
        }
      >
        <MoreHorizontal class="size-5" aria-hidden="true" />
        {/* Running work stays glanceable without a bar: one quiet dot, the
            count itself in the menu row below. */}
        <Show when={processCount() > 0}>
          <span
            aria-hidden="true"
            class="absolute right-1 top-1 size-1.5 rounded-full bg-status-active"
          />
        </Show>
      </DropdownMenuTrigger>
      <DropdownMenuContent class="min-w-[11rem]">
        {/* Hirsel and Tasks are the standing world, not menu destinations.
            This overflow contains utilities only. */}
        <DropdownMenuItem
          class="justify-between [@media(pointer:coarse)]:min-h-11"
          onSelect={() => openProcesses()}
        >
          <span class="flex items-center gap-2">
            <Activity aria-hidden="true" />
            Processes
          </span>
          <Show when={processCount() > 0}>
            <span class="text-xs font-medium text-status-active">
              {processCount() > 99 ? "99+" : processCount()}
            </span>
          </Show>
        </DropdownMenuItem>
        <Show when={canvasAvailable()}>
          <DropdownMenuItem class="[@media(pointer:coarse)]:min-h-11" onSelect={showCanvas}>
            <PanelRight aria-hidden="true" />
            Canvas
          </DropdownMenuItem>
        </Show>
        <DropdownMenuSeparator />
        {/* ONE Settings entry. The model choice is a tab inside it now, not a
            second row into the same sheet. */}
        <DropdownMenuItem
          class="[@media(pointer:coarse)]:min-h-11"
          onSelect={() => openSettings()}
        >
          <SettingsIcon aria-hidden="true" />
          Settings
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
