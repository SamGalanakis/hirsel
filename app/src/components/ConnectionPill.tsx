import { Show } from "solid-js";
import type { ConnectionStatus } from "../store/types";
import { state } from "../store/store";
import { Badge } from "./ui/badge";

const LABEL: Record<ConnectionStatus, string> = {
  connecting: "connecting…",
  connected: "connected",
  reconnecting: "reconnecting…",
};

/** The connection status indicator. `compact` (phone header) collapses to a
 * bare dot while connected — the calm resting state costs no width — and expands
 * to the full pill only when reconnecting/offline, where the word matters. The
 * full pill everywhere else (nav footer, Settings). */
export function ConnectionPill(props: { compact?: boolean }) {
  const pending = () => state.connection !== "connected";

  return (
    <Show
      when={!props.compact || pending()}
      fallback={
        <span
          class="grid size-6 shrink-0 place-items-center"
          role="status"
          aria-live="polite"
          aria-label="Connected"
        >
          <span class="size-2 rounded-full bg-status-success" aria-hidden="true" />
        </span>
      }
    >
      <Badge variant="outline" class="gap-1.5 text-muted-foreground" role="status" aria-live="polite">
        <span
          aria-hidden="true"
          class="size-1.5 rounded-full"
          classList={{
            "bg-status-success": state.connection === "connected",
            "bg-status-attention animate-pulse": pending(),
          }}
        />
        {LABEL[state.connection]}
      </Badge>
    </Show>
  );
}
