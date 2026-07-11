import type { ConnectionStatus } from "../store/types";
import { state } from "../store/store";
import { Badge } from "./ui/badge";

const LABEL: Record<ConnectionStatus, string> = {
  connecting: "connecting…",
  connected: "connected",
  reconnecting: "reconnecting…",
};

export function ConnectionPill() {
  const pending = () => state.connection !== "connected";
  return (
    <Badge
      variant="outline"
      class="gap-1.5 text-muted-foreground"
      role="status"
      aria-live="polite"
    >
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
  );
}
