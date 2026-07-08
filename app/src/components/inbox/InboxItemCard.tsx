import { Show } from "solid-js";
import type { InboxItem, QuickReply } from "../../protocol";
import { Markdown } from "../Markdown";
import { Button } from "../ui/button";
import { Card } from "../ui/card";
import { QuickReplyButtons } from "./QuickReplyButtons";

interface Props {
  item: InboxItem;
  onQuickReply: (item: InboxItem, reply: QuickReply) => void;
  onReply: (item: InboxItem) => void;
  onArchive: (item: InboxItem) => void;
}

function formatTime(ts: string): string {
  try {
    return new Date(ts).toLocaleString([], {
      month: "short",
      day: "numeric",
      hour: "2-digit",
      minute: "2-digit",
    });
  } catch {
    return "";
  }
}

export function InboxItemCard(props: Props) {
  const isOpen = () => props.item.status === "open";
  return (
    <Card
      size="sm"
      class="mx-3 gap-2 border-l-2 px-3 py-3"
      classList={{
        "border-l-primary": props.item.requires_response,
        "border-l-transparent": !props.item.requires_response,
      }}
    >
      <div class="flex items-center justify-between">
        <span class="text-[0.7rem] text-muted-foreground">{formatTime(props.item.ts)}</span>
        <Show when={!isOpen()}>
          <span class="text-[0.68rem] uppercase tracking-[0.03em] text-muted-foreground">
            Archived
          </span>
        </Show>
      </div>
      <Markdown>{props.item.content}</Markdown>
      <Show when={isOpen()}>
        <QuickReplyButtons
          quickReplies={props.item.quick_replies}
          onTap={(reply) => props.onQuickReply(props.item, reply)}
        />
      </Show>
      <div class="-ml-2.5 mt-1 flex gap-1">
        <Button type="button" variant="link" size="sm" onClick={() => props.onReply(props.item)}>
          Reply
        </Button>
        <Show when={isOpen()}>
          <Button
            type="button"
            variant="ghost"
            size="sm"
            class="text-muted-foreground"
            onClick={() => props.onArchive(props.item)}
          >
            Archive
          </Button>
        </Show>
      </div>
    </Card>
  );
}
