import { Show } from "solid-js";
import { state } from "../../store/store";

/** The desktop chat-pane header's left datum — now EXCEPTION-ONLY. In normal
 * operation (connected, whether the agent is idle or thinking) it renders
 * NOTHING: idle is the default state of the world, and a live turn is announced
 * exactly once — by the inline transcript marker, where the eye already is and
 * which carries the tool timeline. The header never doubles that, and it never
 * says "thinking" while the marker runs.
 *
 * The only thing worth a persistent header word is an exceptional connection
 * state, where the last-known agent reading can no longer be trusted: a quiet,
 * dimmed, pulse-free "offline" reading (never a "thinking" lie). On phone the
 * same exception is owned by the header's ConnectionPill, so this datum lives
 * only in the desktop chat header — nothing here is duplicated on either
 * surface. Purely presentational; no indigo, 16px-max.
 */
export function AgentStatus() {
  const connected = () => state.connection === "connected";
  return (
    <Show when={!connected()}>
      <div class="flex min-w-0 items-center gap-2 opacity-60">
        <span class="size-1.5 shrink-0 rounded-full bg-status-idle" aria-hidden="true" />
        <span class="truncate text-xs text-muted-foreground">Agent · offline</span>
      </div>
    </Show>
  );
}
