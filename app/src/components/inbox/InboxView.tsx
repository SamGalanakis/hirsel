import { ChevronDown, ChevronRight, Inbox as InboxIcon } from "lucide-solid";
import { createMemo, createSignal, For, Show } from "solid-js";
import type { InboxItem } from "../../protocol";
import { dispatch, goToChat, requestSideChatOpen, state } from "../../store/store";
import { isResolvedStatus } from "../../store/selectors";
import { getClient } from "../../ws/client";
import { Empty, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle } from "../ui/empty";
import { InboxItemCard } from "./InboxItemCard";

const DONE_LIMIT = 20;

/** Content of the expanded Tray: the open list (this component's sole caller
 * is now `TrayOverlay`) plus the collapsed Done section (ADR-0009: was
 * "Deleted"). */
export function InboxView() {
  const [doneExpanded, setDoneExpanded] = createSignal(false);

  const partitioned = createMemo(() => {
    const sorted = [...state.inbox].sort((a, b) => b.id - a.id);
    const open = sorted.filter((i) => i.status === "open");
    // Tray (v1.6): requires_response items float to the top of the open list —
    // a triage affordance a chronological-only feed lacks, per the design
    // critique's [P1] fix. Each bucket stays newest-first internally so
    // arrival order is still legible within a bucket.
    const requiresResponse = open.filter((i) => i.requires_response);
    const rest = open.filter((i) => !i.requires_response);
    return {
      open: [...requiresResponse, ...rest],
      // ADR-0009: resolved items (done, or legacy archived) collect here.
      done: sorted.filter((i) => isResolvedStatus(i.status)).slice(0, DONE_LIMIT),
    };
  });

  // Answer in place: reuse the same send path an owner Composer send uses
  // (client_id, optimistic reconcile, offline queue). No tab switch, no
  // navigation — the card reconciles the reply inline and stays in the Inbox.
  function handleSendReply(item: InboxItem, body: string) {
    getClient()?.sendMessage(body, item.anchor);
  }

  // Secondary affordance: jump to the item's Anchor in Chat.
  function handleJumpToChat(item: InboxItem) {
    goToChat({ scrollToMessageId: item.anchor });
  }

  // Mark done = the wire `archive_item` (ADR-0009: the item's terminal state,
  // presented as "Done"); non-destructive, reached from a card's ⋯ menu.
  function handleDelete(item: InboxItem) {
    getClient()?.archiveItem(item.id);
  }

  // Auto-read / "Mark read": optimistic read flip + read_item on the wire.
  function handleRead(item: InboxItem) {
    getClient()?.readItem(item.id);
  }

  // "Mark unread": client-only override — there is no wire unread op.
  function handleMarkUnread(item: InboxItem) {
    dispatch({ type: "mark_unread_local", itemId: item.id });
  }

  // v2.0 (ADR-0008): "Discuss" (fresh) / "resume" (a live one already exists)
  // — open_side_chat is idempotent per item on the host either way. Recording
  // the pending item id here is what lets ChatView's effect open the sheet
  // the moment its `sc` (fresh) or transcript (resume) is ready, without the
  // Tray/InboxView needing to know about the sheet at all.
  function handleDiscuss(item: InboxItem) {
    getClient()?.openSideChat(item.id);
    requestSideChatOpen(item.id);
  }

  return (
    <Show
      when={partitioned().open.length > 0 || partitioned().done.length > 0}
      fallback={
        <div class="flex flex-1 flex-col p-3">
          <Empty class="border-none">
            <EmptyHeader>
              <EmptyMedia variant="icon">
                <InboxIcon />
              </EmptyMedia>
              <EmptyTitle>Inbox is empty</EmptyTitle>
              <EmptyDescription>Nothing needs your attention right now.</EmptyDescription>
            </EmptyHeader>
          </Empty>
        </div>
      }
    >
      <div class="thin-scrollbar flex flex-1 flex-col gap-3 overflow-y-auto py-3 pb-6">
        <div class="flex flex-col gap-3">
          <For each={partitioned().open}>
            {(item) => (
              <InboxItemCard
                item={item}
                onSendReply={handleSendReply}
                onJumpToChat={handleJumpToChat}
                onDelete={handleDelete}
                onRead={handleRead}
                onMarkUnread={handleMarkUnread}
                onDiscuss={handleDiscuss}
              />
            )}
          </For>
        </div>

        <Show when={partitioned().done.length > 0}>
          <div class="mt-2">
            <button
              type="button"
              class="mx-3 flex w-[calc(100%-1.5rem)] items-center gap-1 border-t border-border py-3 text-left text-sm text-muted-foreground transition-colors hover:text-foreground"
              onClick={() => setDoneExpanded((v) => !v)}
            >
              <Show when={doneExpanded()} fallback={<ChevronRight class="size-4" />}>
                <ChevronDown class="size-4" />
              </Show>
              Done ({partitioned().done.length})
            </button>
            <Show when={doneExpanded()}>
              <div class="flex flex-col gap-3">
                <For each={partitioned().done}>
                  {(item) => (
                    <InboxItemCard
                      item={item}
                      onSendReply={handleSendReply}
                      onJumpToChat={handleJumpToChat}
                      onDelete={handleDelete}
                      onRead={handleRead}
                      onMarkUnread={handleMarkUnread}
                      onDiscuss={handleDiscuss}
                    />
                  )}
                </For>
              </div>
            </Show>
          </div>
        </Show>
      </div>
    </Show>
  );
}
