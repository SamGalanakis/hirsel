import { Check, ChevronDown } from "lucide-solid";
import { createEffect, createSignal, For, Show } from "solid-js";
import type { AvailableModel } from "../../protocol";
import { state } from "../../store/store";
import { getClient } from "../../ws/client";
import { Popover, PopoverContent, PopoverTrigger } from "../ui/popover";

/** Compact main-model control for the chat header: a chip showing the current
 * model + variant (e.g. "GPT-5.6 Sol · medium") that opens a small popover to
 * switch the reasoning variant quickly. The full model config (and all
 * sub-agent config) lives in Settings — this is just the fast-path variant
 * switch for the main chat. Hidden entirely until a host reports a model
 * snapshot (older hosts omit it), so it never shows a stale/empty chip.
 *
 * Server-truthful: tapping a variant sends `set_model` and shows a brief pending
 * state on the tapped row; the `model_changed` broadcast settles the store (no
 * permanent optimistic divergence — the store is only written by the broadcast). */
export function ModelChip() {
  const current = () => state.model?.current;
  const selectedModel = (): AvailableModel | undefined =>
    state.model?.available.find((m) => m.id === current()?.id);
  const modelLabel = () => selectedModel()?.label ?? current()?.id ?? "";

  // The variant we've sent and are waiting for the broadcast to confirm; drives
  // the row's pending affordance and the chip's shown variant so the tap reads
  // as instant while the truth is still the store's.
  const [pendingVariant, setPendingVariant] = createSignal<string | null>(null);
  createEffect(() => {
    const p = pendingVariant();
    if (p !== null && current()?.variant === p) setPendingVariant(null);
  });

  const shownVariant = () => pendingVariant() ?? current()?.variant ?? "";

  function pick(variant: string) {
    const id = current()?.id;
    if (!id || variant === current()?.variant) return;
    setPendingVariant(variant);
    getClient()?.setModel(id, variant);
  }

  return (
    <Show when={current()}>
      <Popover placement="bottom-end" gutter={6}>
        {/* Configuration, not status: a quiet ghost control — no fill, no border,
            no indigo glyph — that brightens on hover/focus. It carries the same
            tap target and menu, just far less weight than the rest of the header. */}
        <PopoverTrigger
          class="flex min-w-0 shrink items-center gap-1 rounded-md px-2 py-1 text-xs text-muted-foreground outline-none transition-colors hover:bg-muted hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring data-[expanded]:bg-muted data-[expanded]:text-foreground"
          aria-label={`Model: ${modelLabel()} · ${shownVariant()}. Change variant`}
        >
          {/* Model name is the first thing dropped when the header is tight
              (phone); the variant — the thing this chip switches — always stays. */}
          <span class="hidden min-w-0 truncate min-[400px]:inline">{modelLabel()}</span>
          <span class="hidden text-muted-foreground/50 min-[400px]:inline" aria-hidden="true">
            ·
          </span>
          <span class="shrink-0 tabular-nums">{shownVariant()}</span>
          <ChevronDown class="size-3 shrink-0 opacity-50" aria-hidden="true" />
        </PopoverTrigger>
        <PopoverContent class="min-w-[11rem]">
          <div class="px-2 pt-1.5 pb-1 text-[0.68rem] font-medium uppercase tracking-[0.06em] text-muted-foreground">
            {modelLabel()}
          </div>
          <div role="radiogroup" aria-label="Reasoning variant">
            <For each={selectedModel()?.variants ?? []}>
              {(variant) => {
                const active = () => shownVariant() === variant;
                return (
                  <button
                    type="button"
                    role="radio"
                    aria-checked={active()}
                    onClick={() => pick(variant)}
                    class="flex w-full items-center justify-between gap-3 rounded-sm px-2 py-1.5 text-sm outline-none transition-colors hover:bg-accent hover:text-accent-foreground focus-visible:bg-accent focus-visible:text-accent-foreground"
                  >
                    <span class="capitalize">{variant}</span>
                    <Show when={active()}>
                      <Check class="size-3.5 shrink-0 text-primary" aria-hidden="true" />
                    </Show>
                  </button>
                );
              }}
            </For>
          </div>
        </PopoverContent>
      </Popover>
    </Show>
  );
}
