import { X } from "lucide-solid";
import { For } from "solid-js";
import { cn } from "@/lib/utils";
import { dismissToast, toasts } from "../lib/toast";

/** Fixed bottom-center toast stack. Phone-first: sits above the safe area. */
export function Toaster() {
  return (
    <div class="pointer-events-none fixed inset-x-0 bottom-[calc(env(safe-area-inset-bottom)+1rem)] z-50 flex flex-col items-center gap-2 px-4">
      <For each={toasts()}>
        {(t) => (
          <div
            role="status"
            class={cn(
              "pointer-events-auto flex w-full max-w-[520px] items-start gap-2 rounded-lg border px-3 py-2 text-sm shadow-lg backdrop-blur",
              t.variant === "error"
                ? "border-destructive/40 bg-destructive/15 text-foreground"
                : "border-border bg-card text-card-foreground",
            )}
          >
            <span class="min-w-0 flex-1 wrap-break-word">{t.message}</span>
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
