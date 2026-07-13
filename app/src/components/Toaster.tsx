import { X } from "lucide-solid";
import { For, Show } from "solid-js";
import { cn } from "@/lib/utils";
import { dismissToast, pauseToast, resumeToast, toasts } from "../lib/toast";

/** Fixed bottom-center toast stack. Phone-first: sits above the safe area. */
export function Toaster() {
  return (
    <div class="pointer-events-none fixed inset-x-0 bottom-[calc(env(safe-area-inset-bottom)+1rem)] z-50 flex flex-col items-center gap-2 px-4">
      <For each={toasts()}>
        {(t) => (
          // Pause-on-hover/focus lives on the toast surface itself (a live
          // region), not an interactive child — the whole card is the hover
          // target. role="status" is the correct AT semantics here.
          // eslint-disable-next-line jsx-a11y/no-noninteractive-element-interactions
          <div
            role="status"
            class={cn(
              "pointer-events-auto flex w-full max-w-[520px] items-start gap-2 rounded-lg border px-3 py-2 text-sm shadow-lg backdrop-blur",
              t.variant === "error"
                ? "border-destructive/40 bg-destructive/15 text-foreground"
                : "border-border bg-card text-card-foreground",
            )}
            // Pause the auto-dismiss while the toast is hovered or holds keyboard
            // focus, and resume on leave (spec item 7) — reaching for "Undo"
            // never runs the clock out from under the pointer/focus.
            onMouseEnter={() => pauseToast(t.id)}
            onMouseLeave={() => resumeToast(t.id)}
            onFocusIn={() => pauseToast(t.id)}
            onFocusOut={() => resumeToast(t.id)}
          >
            <span class="min-w-0 flex-1 wrap-break-word">{t.message}</span>
            <Show when={t.action}>
              {(action) => (
                <button
                  type="button"
                  class="shrink-0 rounded-sm px-1.5 py-0.5 text-sm font-medium text-primary transition-colors hover:text-primary/80 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                  onClick={() => action().onClick()}
                >
                  {action().label}
                </button>
              )}
            </Show>
            <button
              type="button"
              class="shrink-0 text-muted-foreground transition-colors hover:text-foreground"
              aria-label="Dismiss"
              onClick={() => dismissToast(t.id)}
            >
              <X class="size-4" />
            </button>
          </div>
        )}
      </For>
    </div>
  );
}
