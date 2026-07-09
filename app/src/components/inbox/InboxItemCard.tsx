import { Check, Clock, MessageSquare, RotateCcw } from "lucide-solid";
import { createMemo, createSignal, Show } from "solid-js";
import type { InboxItem, QuickReply } from "../../protocol";
import { state } from "../../store/store";
import { latestReplyForAnchor } from "../../store/selectors";
import { Markdown } from "../Markdown";
import { Button } from "../ui/button";
import { Card } from "../ui/card";
import { QuickReplyButtons } from "./QuickReplyButtons";
import { ReplyInput } from "./ReplyInput";

interface Props {
  item: InboxItem;
  /** Send a reply anchored to this item, in place — no navigation. Shared by
   * the Quick Reply taps and the inline freeform input. */
  onSendReply: (item: InboxItem, body: string) => void;
  /** Secondary affordance: jump to the item's Anchor in Chat. */
  onJumpToChat: (item: InboxItem) => void;
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

function snippet(body: string): string {
  const oneLine = body.replace(/\s+/g, " ").trim();
  return oneLine.length > 80 ? `${oneLine.slice(0, 80)}…` : oneLine;
}

export function InboxItemCard(props: Props) {
  const isOpen = () => props.item.status === "open";
  // Reveal the inline input on non-requires_response cards via "Reply"; on
  // requires_response cards it is expanded by default.
  const [revealed, setRevealed] = createSignal(false);
  const showInput = () => isOpen() && (props.item.requires_response || revealed());

  // Replied state, derived from Chat (no persisted inbox reply state): the
  // latest owner message anchored to this item, if any.
  const reply = createMemo(() => latestReplyForAnchor(state.messages, props.item.anchor));

  return (
    <Card
      size="sm"
      class="mx-3 gap-2 border-l-2 px-3 py-3"
      classList={{
        "border-l-primary": props.item.requires_response,
        "border-l-transparent": !props.item.requires_response,
      }}
    >
      <div class="flex items-center justify-between gap-2">
        <span class="text-[0.7rem] text-muted-foreground">{formatTime(props.item.ts)}</span>
        <div class="flex items-center gap-1">
          <Show when={!isOpen()}>
            <span class="text-[0.68rem] uppercase tracking-[0.03em] text-muted-foreground">
              Archived
            </span>
          </Show>
          {/* Secondary "jump to chat" affordance (was the default card tap). */}
          <button
            type="button"
            class="-mr-1 rounded p-1 text-muted-foreground transition-colors hover:text-foreground"
            aria-label="Open in chat"
            title="Open in chat"
            onClick={() => props.onJumpToChat(props.item)}
          >
            <MessageSquare class="size-3.5" />
          </button>
        </div>
      </div>

      <Markdown>{props.item.content}</Markdown>

      {/* Replied state: small right-aligned quote under the content. */}
      <Show when={reply()}>
        {(r) => (
          <div class="flex justify-end">
            <span
              class="inline-flex max-w-full items-center gap-1 rounded-md bg-muted px-2 py-0.5 text-[0.72rem] text-muted-foreground"
              classList={{ "text-destructive": r().failed }}
            >
              <span class="font-medium">you:</span>
              <span class="min-w-0 truncate">{snippet(r().body)}</span>
              <Show
                when={!r().pending && !r().failed}
                fallback={
                  <Show
                    when={r().failed}
                    fallback={<Clock class="size-3 shrink-0" aria-label="Sending" />}
                  >
                    <RotateCcw class="size-3 shrink-0" aria-label="Failed to send" />
                  </Show>
                }
              >
                <Check class="size-3 shrink-0 text-status-success" aria-label="Sent" />
              </Show>
            </span>
          </div>
        )}
      </Show>

      <Show when={isOpen()}>
        <QuickReplyButtons
          quickReplies={props.item.quick_replies}
          onTap={(qr: QuickReply) => props.onSendReply(props.item, qr.value)}
        />
      </Show>

      <Show when={showInput()}>
        <ReplyInput
          autofocus={revealed()}
          placeholder="Reply…"
          onSend={(body) => props.onSendReply(props.item, body)}
        />
      </Show>

      <div class="-ml-2.5 mt-1 flex gap-1">
        <Show when={isOpen() && !showInput()}>
          <Button
            type="button"
            variant="link"
            size="sm"
            onClick={() => setRevealed(true)}
          >
            Reply
          </Button>
        </Show>
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
