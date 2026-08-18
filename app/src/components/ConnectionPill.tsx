import { Show } from "solid-js";
import type { ConnectionStatus } from "../store/types";
import { state } from "../store/store";
import { Badge } from "./ui/badge";

const LABEL: Record<ConnectionStatus, string> = {
  connecting: "connecting…",
  connected: "connected",
  reconnecting: "reconnecting…",
};

/** The connection status indicator. `compact` is always a bare semantic dot:
 * the composer already spells out offline/reconnecting, so the header never
 * grows or wraps when connection state changes. */
export function ConnectionPill(props: { compact?: boolean }) {
  const pending = () => state.connection !== "connected";

  return (
    <Show
      when={!props.compact}
      fallback={
        <span
          class="grid size-6 shrink-0 place-items-center"
          role="status"
          aria-live="polite"
          aria-label={LABEL[state.connection]}
        >
          <span
            class="size-2 rounded-full"
            classList={{
              "bg-status-success": !pending(),
              "bg-status-attention motion-safe:animate-pulse": pending(),
            }}
            aria-hidden="true"
          />
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
