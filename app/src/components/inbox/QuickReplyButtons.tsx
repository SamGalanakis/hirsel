import { For, Show } from "solid-js";
import type { QuickReply } from "../../protocol";
import { Button } from "../ui/button";

interface Props {
  quickReplies: QuickReply[];
  onTap: (reply: QuickReply) => void;
}

export function QuickReplyButtons(props: Props) {
  return (
    <Show when={props.quickReplies.length > 0}>
      <div class="mt-2 flex flex-wrap gap-2">
        <For each={props.quickReplies}>
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
      </div>
    </Show>
  );
}
