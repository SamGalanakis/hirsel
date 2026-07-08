import { ChevronDown, ChevronRight, Inbox as InboxIcon } from "lucide-solid";
import { createMemo, createSignal, For, Show } from "solid-js";
import type { InboxItem, QuickReply } from "../../protocol";
import { goToChat, state } from "../../store/store";
import { getClient } from "../../ws/client";
import { Empty, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle } from "../ui/empty";
import { InboxItemCard } from "./InboxItemCard";

const ARCHIVED_LIMIT = 20;

export function InboxView() {
  const [archivedExpanded, setArchivedExpanded] = createSignal(false);

  const partitioned = createMemo(() => {
    const sorted = [...state.inbox].sort((a, b) => b.id - a.id); // newest first
    return {
      open: sorted.filter((i) => i.status === "open"),
      archived: sorted.filter((i) => i.status === "archived").slice(0, ARCHIVED_LIMIT),
    };
  });

  function handleQuickReply(item: InboxItem, reply: QuickReply) {
    const localId = getClient()?.sendMessage(reply.value, item.anchor);
    goToChat({ scrollToMessageId: localId });
  }

  function handleReply(item: InboxItem) {
    goToChat({ composerDraft: { ref: item.anchor } });
  }

  function handleArchive(item: InboxItem) {
    getClient()?.archiveItem(item.id);
  }

  return (
    <Show
      when={partitioned().open.length > 0 || partitioned().archived.length > 0}
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
                onQuickReply={handleQuickReply}
                onReply={handleReply}
                onArchive={handleArchive}
              />
            )}
          </For>
        </div>

        <Show when={partitioned().archived.length > 0}>
          <div class="mt-2">
            <button
              type="button"
              class="mx-3 flex w-[calc(100%-1.5rem)] items-center gap-1 border-t border-border py-3 text-left text-sm text-muted-foreground transition-colors hover:text-foreground"
              onClick={() => setArchivedExpanded((v) => !v)}
            >
              <Show when={archivedExpanded()} fallback={<ChevronRight class="size-4" />}>
                <ChevronDown class="size-4" />
              </Show>
              Archived ({partitioned().archived.length})
            </button>
            <Show when={archivedExpanded()}>
              <div class="flex flex-col gap-3">
                <For each={partitioned().archived}>
                  {(item) => (
                    <InboxItemCard
                      item={item}
                      onQuickReply={handleQuickReply}
                      onReply={handleReply}
                      onArchive={handleArchive}
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
