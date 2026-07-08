import { MessagesSquare } from "lucide-solid";
import { createEffect, createMemo, createSignal, For, Show } from "solid-js";
import { clearComposerDraft, clearScrollTarget, state } from "../../store/store";
import type { DisplayMessage } from "../../store/types";
import { getClient } from "../../ws/client";
import { Empty, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle } from "../ui/empty";
import { AgentActivityIndicator } from "./AgentActivityIndicator";
import { Composer } from "./Composer";
import { MessageBubble } from "./MessageBubble";

const HIGHLIGHT_MS = 1600;

export function ChatView() {
  let scrollRef: HTMLDivElement | undefined;
  const [highlightedId, setHighlightedId] = createSignal<number | null>(null);
  let prevLength = 0;

  const messagesById = createMemo(() => {
    const map = new Map<number, DisplayMessage>();
    for (const m of state.messages) map.set(m.id, m);
    return map;
  });

  function scrollToId(id: number) {
    document.getElementById(`msg-${id}`)?.scrollIntoView({ behavior: "smooth", block: "center" });
    setHighlightedId(id);
    setTimeout(() => setHighlightedId((cur) => (cur === id ? null : cur)), HIGHLIGHT_MS);
  }

  // Auto-scroll to the newest message as the thread grows, unless a specific
  // scroll target was requested (handled by the effect below).
  createEffect(() => {
    const len = state.messages.length;
    const target = state.scrollToMessageId;
    if (len > prevLength && target === null) {
      scrollRef?.scrollTo({ top: scrollRef.scrollHeight, behavior: "smooth" });
    }
    prevLength = len;
  });

  // Consume a one-shot scroll-to request (from a quoted ref tap or a quick reply).
  createEffect(() => {
    const target = state.scrollToMessageId;
    if (target === null) return;
    scrollToId(target);
    clearScrollTarget();
  });

  const replyingTo = () =>
    state.composerDraft ? messagesById().get(state.composerDraft.ref) : null;

  function handleSend(body: string, ref: number | null) {
    getClient()?.sendMessage(body, ref);
  }

  return (
    <div class="flex min-h-0 flex-1 flex-col">
      <div ref={scrollRef} class="thin-scrollbar flex flex-1 flex-col gap-3 overflow-y-auto py-3">
        <Show when={state.messages.length === 0}>
          <Empty class="border-none">
            <EmptyHeader>
              <EmptyMedia variant="icon">
                <MessagesSquare />
              </EmptyMedia>
              <EmptyTitle>No messages yet</EmptyTitle>
              <EmptyDescription>Say hello to the Agent to get started.</EmptyDescription>
            </EmptyHeader>
          </Empty>
        </Show>
        <For each={state.messages}>
          {(m) => (
            <MessageBubble
              message={m}
              refTarget={m.ref !== null ? messagesById().get(m.ref) : undefined}
              highlighted={highlightedId() === m.id}
              onTapQuote={scrollToId}
            />
          )}
        </For>
      </div>
      <AgentActivityIndicator />
      <Composer replyingTo={replyingTo()} onCancelReply={clearComposerDraft} onSend={handleSend} />
    </div>
  );
}
