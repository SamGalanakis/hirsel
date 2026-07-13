import { Activity, MoreHorizontal, PanelRight, Settings as SettingsIcon, Sparkles } from "lucide-solid";
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

// The phone header's single overflow (§4 IA cleanup): brand + full agent status
// keep the header's width, and every secondary control — the quick model
// variant, Canvas, Processes, Settings — folds behind this one ⋯ menu so the
// north-star AgentStatus never truncates and the header stays at ≤4 chunks. The
// running-process count rides on the trigger as a status-active tint dot so
// "something is running" is still glanceable without opening the menu.

export function PhoneOverflowMenu() {
  const processCount = () => runningProcessCount(state.processes);
  const canvasAvailable = () =>
    canvasViews(state.views).length > 0 && state.rightRegion !== "canvas";
  const model = () => state.model?.current;

  return (
    <DropdownMenu placement="bottom-end" gutter={6}>
      <DropdownMenuTrigger
        class="relative flex items-center rounded-full p-1.5 text-muted-foreground outline-none transition-colors hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring"
        aria-label={
          processCount() > 0 ? `More actions, ${processCount()} running` : "More actions"
        }
      >
        <MoreHorizontal class="size-5" aria-hidden="true" />
        <Show when={processCount() > 0}>
          {/* Tint-chip awareness of running work, mirroring the desktop nav's
              Processes badge — never the interrupt red. The count is exposed to
              AT via the trigger's aria-label (spec item 6), so the visual chip
              stays decorative. */}
          <span
            class="absolute -right-0.5 -top-0.5 grid h-4 min-w-4 place-items-center rounded-full bg-status-active/20 px-1 text-[0.68rem] font-semibold text-status-active"
            aria-hidden="true"
          >
            {processCount() > 99 ? "99+" : processCount()}
          </span>
        </Show>
      </DropdownMenuTrigger>
      <DropdownMenuContent class="min-w-[11rem]">
        {/* "Model settings" (spec item 6): the honest label + destination — it
            opens Settings scrolled to the Models section, not Appearance, so the
            row's affordance matches where it lands. */}
        <Show when={model()}>
          <DropdownMenuItem class="justify-between" onSelect={() => openSettings("models")}>
            <span class="flex items-center gap-2">
              <Sparkles class="text-primary" aria-hidden="true" />
              Model settings
            </span>
            <span class="text-xs capitalize text-muted-foreground">{model()?.variant}</span>
          </DropdownMenuItem>
        </Show>
        <Show when={canvasAvailable()}>
          <DropdownMenuItem onSelect={showCanvas}>
            <PanelRight aria-hidden="true" />
            Canvas
          </DropdownMenuItem>
        </Show>
        <DropdownMenuItem class="justify-between" onSelect={openProcesses}>
          <span class="flex items-center gap-2">
            <Activity aria-hidden="true" />
            Processes
          </span>
          <Show when={processCount() > 0}>
            <span class="flex items-center gap-1 text-xs text-status-active">
              <span
                class="size-1.5 rounded-full bg-status-active motion-safe:animate-pulse"
                aria-hidden="true"
              />
              {processCount() > 99 ? "99+" : processCount()}
            </span>
          </Show>
        </DropdownMenuItem>
        <DropdownMenuSeparator />
        <DropdownMenuItem onSelect={openSettings}>
          <SettingsIcon aria-hidden="true" />
          Settings
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
