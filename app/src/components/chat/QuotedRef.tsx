import { CornerUpLeft } from "lucide-solid";
import { Show } from "solid-js";
import type { DisplayMessage } from "../../store/types";

interface Props {
  message: DisplayMessage | undefined;
  onTap: () => void;
}

function snippet(body: string): string {
  const oneLine = body.replace(/\s+/g, " ").trim();
  return oneLine.length > 80 ? `${oneLine.slice(0, 80)}…` : oneLine;
}

/** A quiet one-line citation of the referenced message — drawn only when the
 * reply is non-contiguous (see ChatView), so it reads as a light "in reply to"
 * marker rather than a nested card that competes with the message itself.
 * Tapping it scrolls to (and briefly highlights) the original. */
export function QuotedRef(props: Props) {
  return (
    <Show
      when={props.message}
      fallback={
        <div class="mb-1 flex items-center gap-1 border-l-2 border-current/30 pl-1.5 text-[0.7rem] italic opacity-55">
          <CornerUpLeft class="size-3 shrink-0" aria-hidden="true" />
          <span>original message unavailable</span>
        </div>
      }
    >
      {(message) => (
        <button
          type="button"
          class="mb-1 flex w-full items-center gap-1 border-l-2 border-current/30 pl-1.5 text-left text-[0.7rem] opacity-65 transition-opacity hover:opacity-90"
          onClick={props.onTap}
        >
          <CornerUpLeft class="size-3 shrink-0" aria-hidden="true" />
          <span class="shrink-0 font-medium">
            {message().author === "owner" ? "You" : "Agent"}
          </span>
          <span class="min-w-0 truncate opacity-85">{snippet(message().body)}</span>
        </button>
      )}
    </Show>
  );
}
