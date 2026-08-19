// The one way plugin UI reaches the screen. A view that owns a region drops a
// `<PluginSlot name="..."/>` where contributions belong; everything registered
// for that slot renders there, in manifest order.
//
// Each contribution gets its OWN `<ErrorBoundary>`, not one around the slot:
// a component that throws while rendering must cost only itself, never its
// neighbours and never the host view. The fallback is a quiet named notice —
// the Owner should be able to see which plugin broke without opening a console.
import { ErrorBoundary, For, type JSX } from "solid-js";
import { Dynamic } from "solid-js/web";
import { slotEntries } from "./registry";
import type { SlotCtx, SlotName } from "./types";

function failureDetail(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export function PluginSlot(props: { name: SlotName; ctx?: SlotCtx }): JSX.Element {
  return (
    <For each={slotEntries(props.name)}>
      {(entry) => (
        <ErrorBoundary
          fallback={(error: unknown) => (
            <div
              role="note"
              data-slot="plugin-error"
              data-plugin={entry.pluginId}
              class="rounded-lg border border-border bg-card px-3 py-2 text-xs leading-snug text-muted-foreground"
            >
              <span class="text-foreground">{entry.label}</span> couldn’t render:{" "}
              {failureDetail(error)}
            </div>
          )}
        >
          <div data-slot="plugin-contribution" data-plugin={entry.pluginId}>
            <Dynamic component={entry.component} ctx={props.ctx ?? {}} />
          </div>
        </ErrorBoundary>
      )}
    </For>
  );
}
