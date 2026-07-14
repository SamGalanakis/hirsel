// The durable-snooze preset chooser (Wave-3) — the compact summoned surface both
// the card ⋯ "Snooze…" and the scroller's swipe-left / ArrowLeft (and ⌘K) open. A
// calm Kobalte dialog: three human presets ("This evening · Tomorrow morning ·
// Next week") each showing the concrete return time, plus a "Pick time…" entry
// that reveals a `datetime-local`. Picking any option calls `onPick(until, label)`
// and closes. Snooze is a quiet verb — nothing red, no destructive confirm.
import * as Dialog from "@kobalte/core/dialog";
import { Clock } from "lucide-solid";
import { createEffect, createSignal, For, Show } from "solid-js";
import { formatReturnTime } from "../../lib/format";
import { datetimeLocalToIso, snoozePresets } from "../../lib/snooze-presets";
import { cn } from "@/lib/utils";

/** A `Date` → the `datetime-local` input's local `YYYY-MM-DDTHH:mm` value. */
function toLocalInputValue(d: Date): string {
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

export function SnoozeChooser(props: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** Picked a return time — `until` is RFC3339, `label` a short human string. */
  onPick: (until: string, label: string) => void;
}) {
  const [picking, setPicking] = createSignal(false);
  const [pickValue, setPickValue] = createSignal("");
  // Default the free picker to tomorrow morning (9am), a sensible non-now start.
  const defaultPick = () => {
    const d = new Date();
    d.setDate(d.getDate() + 1);
    d.setHours(9, 0, 0, 0);
    return toLocalInputValue(d);
  };

  // Fresh chooser each time it is summoned: reset the pick sub-state on open.
  createEffect(() => {
    if (props.open) {
      setPicking(false);
      setPickValue("");
    }
  });

  const pickPreset = (until: string, label: string) => {
    props.onPick(until, label);
    props.onOpenChange(false);
  };
  const confirmPick = () => {
    const iso = datetimeLocalToIso(pickValue() || defaultPick());
    if (!iso) return; // invalid — keep the chooser open
    props.onPick(iso, formatReturnTime(iso));
    props.onOpenChange(false);
  };

  return (
    <Dialog.Root open={props.open} onOpenChange={props.onOpenChange} modal>
      <Dialog.Portal>
        <Dialog.Overlay class="fixed inset-0 z-50 bg-black/40 data-[expanded]:animate-in data-[closed]:animate-out data-[closed]:fade-out-0 data-[expanded]:fade-in-0" />
        <div class="fixed inset-0 z-50 flex items-center justify-center px-4">
          <Dialog.Content
            data-slot="snooze-chooser"
            class={cn(
              "flex w-full max-w-[300px] flex-col overflow-hidden rounded-xl border border-border bg-card p-2 shadow-lg outline-none",
              "data-[expanded]:animate-in data-[closed]:animate-out data-[closed]:fade-out-0 data-[expanded]:fade-in-0 data-[expanded]:zoom-in-95",
            )}
          >
            <Dialog.Title class="px-1.5 pb-1.5 pt-1 text-[0.62rem] font-semibold uppercase tracking-[0.05em] text-muted-foreground">
              Snooze until
            </Dialog.Title>
            <Show
              when={!picking()}
              fallback={
                <div class="flex flex-col gap-2 p-1">
                  <input
                    type="datetime-local"
                    aria-label="Snooze until a specific time"
                    class="w-full rounded-md border border-input bg-transparent px-2 py-1.5 text-xs text-foreground outline-none focus-visible:border-ring focus-visible:ring-2 focus-visible:ring-ring/40"
                    value={pickValue() || defaultPick()}
                    onInput={(e) => setPickValue(e.currentTarget.value)}
                  />
                  <div class="flex items-center justify-end gap-2">
                    <button
                      type="button"
                      class="rounded-sm px-1.5 py-1 text-xs font-medium text-muted-foreground transition-colors hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/50"
                      onClick={() => setPicking(false)}
                    >
                      Back
                    </button>
                    <button
                      type="button"
                      class="inline-flex h-7 items-center rounded-md bg-primary px-3 text-xs font-semibold text-primary-foreground transition-colors hover:bg-primary/90 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/50 active:translate-y-px"
                      onClick={confirmPick}
                    >
                      Snooze
                    </button>
                  </div>
                </div>
              }
            >
              <For each={snoozePresets()}>
                {(preset) => (
                  <button
                    type="button"
                    class="flex w-full items-center justify-between gap-3 rounded-md px-1.5 py-2 text-left text-sm text-foreground transition-colors hover:bg-muted focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/50"
                    onClick={() => pickPreset(preset.until, preset.label)}
                  >
                    <span>{preset.label}</span>
                    <span class="shrink-0 text-[0.68rem] tabular-nums text-muted-foreground/80">
                      {formatReturnTime(preset.until)}
                    </span>
                  </button>
                )}
              </For>
              <div class="-mx-1 my-1 h-px bg-border" />
              <button
                type="button"
                class="flex w-full items-center gap-2 rounded-md px-1.5 py-2 text-left text-sm text-foreground transition-colors hover:bg-muted focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/50"
                onClick={() => {
                  setPickValue(defaultPick());
                  setPicking(true);
                }}
              >
                <Clock class="size-4 shrink-0 text-muted-foreground" aria-hidden="true" />
                Pick time…
              </button>
            </Show>
          </Dialog.Content>
        </div>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
