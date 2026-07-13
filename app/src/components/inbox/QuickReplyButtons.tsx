import { createMemo, createSignal, For, Show } from "solid-js";
import type { QuickReply } from "../../protocol";
import { Button } from "../ui/button";

interface Props {
  quickReplies: QuickReply[];
  onTap: (reply: QuickReply) => void;
}

/** How many quick replies show before the rest fold behind a "more" reveal.
 * An expanded requires-response card can ship an unbounded `quick_replies`
 * list; capping keeps the option budget legible (spec item 7) without ever
 * hiding a choice — the overflow is one keyboard-reachable button away. */
const VISIBLE_CAP = 4;

export function QuickReplyButtons(props: Props) {
  const [expanded, setExpanded] = createSignal(false);
  const overflowCount = createMemo(() => Math.max(0, props.quickReplies.length - VISIBLE_CAP));
  // Cap the visible set until the Owner reveals the rest; every option stays a
  // real focusable button either way, so keyboard access is preserved.
  const shown = createMemo(() =>
    expanded() ? props.quickReplies : props.quickReplies.slice(0, VISIBLE_CAP),
  );

  return (
    <Show when={props.quickReplies.length > 0}>
      <div class="mt-2 flex flex-wrap gap-2">
        <For each={shown()}>
          {(qr) => (
            <Button
              type="button"
              variant="secondary"
              size="sm"
              class="rounded-full"
              onClick={() => props.onTap(qr)}
            >
              {qr.label}
            </Button>
          )}
        </For>
        <Show when={overflowCount() > 0 && !expanded()}>
          <Button
            type="button"
            variant="ghost"
            size="sm"
            class="rounded-full text-muted-foreground"
            aria-expanded={false}
            aria-label={`Show ${overflowCount()} more quick ${
              overflowCount() === 1 ? "reply" : "replies"
            }`}
            onClick={() => setExpanded(true)}
          >
            +{overflowCount()} more
          </Button>
        </Show>
      </div>
    </Show>
  );
}
