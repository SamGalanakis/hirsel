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

/** Compact quoted preview of a referenced ChatMessage, WhatsApp-quote style.
 * Tapping it scrolls to (and briefly highlights) the original. */
export function QuotedRef(props: Props) {
  return (
    <Show
      when={props.message}
      fallback={
        <div class="mb-1 rounded-md border-l-2 border-current/40 bg-black/10 px-2 py-1">
          <span class="text-xs italic opacity-70">original message unavailable</span>
        </div>
      }
    >
      {(message) => (
        <button
          type="button"
          class="mb-1.5 block w-full rounded-md border-l-2 border-current/40 bg-black/10 px-2 py-1 text-left transition-colors hover:bg-black/20"
          onClick={props.onTap}
        >
          <div class="text-[0.62rem] font-medium uppercase tracking-[0.04em] opacity-70">
            {message().author === "owner" ? "You" : "Agent"}
          </div>
          <div class="overflow-hidden text-ellipsis whitespace-nowrap text-xs opacity-75">
            {snippet(message().body)}
          </div>
        </button>
      )}
    </Show>
  );
}
