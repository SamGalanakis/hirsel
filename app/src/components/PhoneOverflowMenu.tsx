import {
  MoreHorizontal,
  PanelRight,
  Settings as SettingsIcon,
} from "lucide-solid";
import { Show } from "solid-js";
import { canvasViews } from "../store/selectors";
import { openSettings, showCanvas, state } from "../store/store";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "./ui/dropdown-menu";

// The phone header's single overflow (§4 IA cleanup): brand + full agent status
// keep the header's width, and every secondary control — the quick model
// variant, Canvas, and Settings fold behind this one ⋯ menu. Processes is
// intentionally first-class in the header: live work should never require
// opening an unrelated menu just to inspect it.

export function PhoneOverflowMenu() {
  const canvasAvailable = () =>
    canvasViews(state.views).length > 0 && state.rightRegion !== "canvas";
  const model = () => state.model?.current;

  return (
    <DropdownMenu placement="bottom-end" gutter={6}>
      <DropdownMenuTrigger
        data-slot="phone-overflow-trigger"
        class="flex size-8 shrink-0 items-center justify-center rounded-full text-muted-foreground outline-none transition-colors hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring [@media(pointer:coarse)]:size-11"
        aria-label="More actions"
      >
        <MoreHorizontal class="size-5" aria-hidden="true" />
      </DropdownMenuTrigger>
      <DropdownMenuContent class="min-w-[11rem]">
        {/* Hirsel and Tasks are the standing world, not menu destinations.
            This overflow contains utilities only. */}
        {/* "Model settings" (spec item 6): the honest label + destination — it
            opens Settings scrolled to the Models section, not Appearance, so the
            row's affordance matches where it lands. */}
        <Show when={model()}>
          <DropdownMenuItem class="justify-between [@media(pointer:coarse)]:min-h-11" onSelect={() => openSettings("models")}>
            <span>Model settings</span>
            <span class="text-xs capitalize text-muted-foreground">{model()?.variant}</span>
          </DropdownMenuItem>
        </Show>
        <Show when={canvasAvailable()}>
          <DropdownMenuItem class="[@media(pointer:coarse)]:min-h-11" onSelect={showCanvas}>
            <PanelRight aria-hidden="true" />
            Canvas
          </DropdownMenuItem>
        </Show>
        <DropdownMenuSeparator />
        <DropdownMenuItem class="[@media(pointer:coarse)]:min-h-11" onSelect={openSettings}>
          <SettingsIcon aria-hidden="true" />
          Settings
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
