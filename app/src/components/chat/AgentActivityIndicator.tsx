import { LoaderCircle } from "lucide-solid";
import { Show } from "solid-js";
import { state } from "../../store/store";

/** Subtle "Agent is working…" indicator driven by agent_activity events.
 * Ephemeral by design: never persisted, never replayed (protocol.md). */
export function AgentActivityIndicator() {
  return (
    <Show when={state.agentActivity.state === "thinking"}>
      <div class="flex items-center gap-2 px-4 py-2 text-xs text-muted-foreground">
        <LoaderCircle class="size-3.5 shrink-0 animate-spin text-primary" aria-hidden="true" />
        <span class="overflow-hidden text-ellipsis whitespace-nowrap">
          {state.agentActivity.text ?? "Agent is working…"}
        </span>
      </div>
    </Show>
  );
}
