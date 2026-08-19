import type { ConnectionStatus } from "../store/types";
import { state } from "../store/store";
import { Badge } from "./ui/badge";

const LABEL: Record<ConnectionStatus, string> = {
  connecting: "connecting…",
  connected: "connected",
  reconnecting: "reconnecting…",
};

/** The spoken/written name of a connection state. Shared so a caller that
 * renders the pill only sometimes can still announce every transition. */
export function connectionLabel(status: ConnectionStatus): string {
  return LABEL[status];
}

/** The connection status indicator: one labelled pill, always spelling the
 * state out. The home shell renders it only while the socket is abnormal (a
 * healthy connection is silence), so a bare dot variant no longer has a
 * caller — the only other home is the Settings connection row. */
export function ConnectionPill() {
  const pending = () => state.connection !== "connected";

  return (
    <Badge variant="outline" class="gap-1.5 text-muted-foreground" role="status" aria-live="polite">
      <span
        aria-hidden="true"
        class="size-1.5 rounded-full"
        classList={{
          "bg-status-success": !pending(),
          "bg-status-attention animate-pulse": pending(),
        }}
      />
      {LABEL[state.connection]}
    </Badge>
  );
}
