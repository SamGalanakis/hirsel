import { Match, Switch } from "solid-js";
import { state } from "../../store/store";

/** The slim desktop-only center-pane header's left datum: a calm agent-status
 * indicator (tint-chip vocabulary — a status dot + label), giving the
 * north-star question "what is my agent doing" a persistent desktop home.
 * Purely presentational; no indigo, 16px-max, quiet when idle.
 *
 * Extracted out of ChatView (v-transcript) so a second surface can render the
 * same reading. Three states, never a lie: while the socket is down we do NOT
 * assert "idle" (we can't know) — the reading dims to a stale/offline register
 * instead; a live "thinking" pulse only shows when we are actually connected.
 */
export function AgentStatus() {
  const connected = () => state.connection === "connected";
  const thinking = () => connected() && state.agentActivity.state === "thinking";
  return (
    <Switch>
      <Match when={!connected()}>
        {/* Stale reading: the socket is down, so the last-known activity can no
            longer be trusted. Dimmed, no pulse, no "idle" assertion. */}
        <div class="flex min-w-0 items-center gap-2 opacity-60">
          <span class="size-1.5 shrink-0 rounded-full bg-status-idle" aria-hidden="true" />
          <span class="truncate text-xs text-muted-foreground">Agent · offline</span>
        </div>
      </Match>
      <Match when={thinking()}>
        <div class="flex min-w-0 items-center gap-2">
          <span
            class="size-1.5 shrink-0 rounded-full bg-status-active motion-safe:animate-pulse"
            aria-hidden="true"
          />
          <span class="truncate text-xs text-status-active">
            {state.agentActivity.text ?? "Thinking…"}
          </span>
        </div>
      </Match>
      <Match when={connected()}>
        <div class="flex min-w-0 items-center gap-2">
          <span class="size-1.5 shrink-0 rounded-full bg-status-idle" aria-hidden="true" />
          <span class="truncate text-xs text-muted-foreground">Agent · idle</span>
        </div>
      </Match>
    </Switch>
  );
}
