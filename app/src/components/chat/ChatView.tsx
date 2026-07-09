import { ArrowDown, MessagesSquare, Upload } from "lucide-solid";
import { createEffect, createMemo, createSignal, For, Show } from "solid-js";
import type { Blob, SendMode } from "../../protocol";
import {
  clearComposerDraft,
  clearComposerPrefill,
  clearScrollTarget,
  state,
} from "../../store/store";
import type { DisplayMessage } from "../../store/types";
import { getClient } from "../../ws/client";
import { Empty, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle } from "../ui/empty";
import { Marker, MarkerContent } from "../ui/marker";
import {
  MessageScroller,
  MessageScrollerButton,
  MessageScrollerContent,
  MessageScrollerItem,
  MessageScrollerViewport,
  useMessageScrollerVisibility,
} from "../ui/message-scroller";
import { Composer } from "./Composer";
import { Lightbox } from "./Lightbox";
import { MessageBubble } from "./MessageBubble";
import { LiveToolCalls } from "./ToolCalls";
import { createComposerAttachments } from "./useAttachments";

const HIGHLIGHT_MS = 1600;

function dayKey(ts: string): string {
  const d = new Date(ts);
  return `${d.getFullYear()}-${d.getMonth()}-${d.getDate()}`;
}

function dayLabel(ts: string): string {
  const d = new Date(ts);
  const today = new Date();
  const yesterday = new Date();
  yesterday.setDate(today.getDate() - 1);
  if (dayKey(ts) === dayKey(today.toISOString())) return "Today";
  if (dayKey(ts) === dayKey(yesterday.toISOString())) return "Yesterday";
  return d.toLocaleDateString([], { weekday: "short", month: "short", day: "numeric" });
}

type Row =
  | { kind: "day"; key: string; label: string }
  | { kind: "msg"; key: string; message: DisplayMessage };

/** Scroll-to-latest pill with an unseen-below count, rendered inside the
 * scroller so it can read the visibility hook. */
function JumpToLatest() {
  const { unseenCount } = useMessageScrollerVisibility();
  return (
    <MessageScrollerButton size="sm" class="gap-1.5 px-3" aria-label="Scroll to latest">
      <ArrowDown class="size-4" />
      <Show when={unseenCount() > 0}>
        <span class="text-xs font-medium">{unseenCount()} new</span>
      </Show>
    </MessageScrollerButton>
  );
}

export function ChatView() {
  const [highlightedId, setHighlightedId] = createSignal<number | null>(null);
  const [lightbox, setLightbox] = createSignal<{ src: string; alt: string } | null>(null);
  const [dragging, setDragging] = createSignal(false);
  let dragDepth = 0;

  const attachments = createComposerAttachments();

  const messagesById = createMemo(() => {
    const map = new Map<number, DisplayMessage>();
    for (const m of state.messages) map.set(m.id, m);
    return map;
  });

  // Interleave day-break markers between messages when the calendar day changes.
  const rows = createMemo<Row[]>(() => {
    const out: Row[] = [];
    let lastDay: string | null = null;
    for (const m of state.messages) {
      const key = dayKey(m.ts);
      if (key !== lastDay) {
        out.push({ kind: "day", key: `day-${key}-${m.id}`, label: dayLabel(m.ts) });
        lastDay = key;
      }
      out.push({ kind: "msg", key: `msg-${m.id}`, message: m });
    }
    return out;
  });

  function scrollToId(id: number) {
    document.getElementById(`msg-${id}`)?.scrollIntoView({ behavior: "smooth", block: "center" });
    setHighlightedId(id);
    setTimeout(() => setHighlightedId((cur) => (cur === id ? null : cur)), HIGHLIGHT_MS);
  }

  // Consume a one-shot scroll-to request (from a quoted ref tap or a quick reply).
  createEffect(() => {
    const target = state.scrollToMessageId;
    if (target === null) return;
    scrollToId(target);
    clearScrollTarget();
  });

  const replyingTo = () =>
    state.composerDraft ? messagesById().get(state.composerDraft.ref) : null;

  const thinking = () => state.agentActivity.state === "thinking";

  // Ids of the trailing run of owner messages with no agent reply after them —
  // i.e. still-unanswered sends. A next_turn bubble only shows its cancellable
  // "queued" chip while it is in this run AND a turn is active; once the agent
  // replies (an agent message lands after it) it drops out and the chip clears.
  const unansweredOwnerIds = createMemo(() => {
    const ids = new Set<number>();
    for (let i = state.messages.length - 1; i >= 0; i--) {
      if (state.messages[i].author === "agent") break;
      ids.add(state.messages[i].id);
    }
    return ids;
  });

  function handleSend(body: string, ref: number | null, mode: SendMode, blobs: Blob[]) {
    getClient()?.sendMessage(body, ref, { mode, attachments: blobs });
  }

  function lastOwnerBody(): string | null {
    for (let i = state.messages.length - 1; i >= 0; i--) {
      if (state.messages[i].author === "owner") return state.messages[i].body;
    }
    return null;
  }

  // Drag-and-drop anywhere on the chat view. Depth counter avoids flicker as the
  // dragged item crosses child element boundaries.
  function onDragEnter(e: DragEvent) {
    if (!e.dataTransfer?.types.includes("Files")) return;
    dragDepth += 1;
    setDragging(true);
  }
  function onDragLeave() {
    dragDepth = Math.max(0, dragDepth - 1);
    if (dragDepth === 0) setDragging(false);
  }
  function onDragOver(e: DragEvent) {
    if (e.dataTransfer?.types.includes("Files")) e.preventDefault();
  }
  function onDrop(e: DragEvent) {
    dragDepth = 0;
    setDragging(false);
    if (!e.dataTransfer?.files.length) return;
    e.preventDefault();
    attachments.addFiles(e.dataTransfer.files);
  }

  return (
    <div class="flex min-h-0 flex-1 flex-col">
      <div
        class="relative flex min-h-0 flex-1 flex-col"
        onDragEnter={onDragEnter}
        onDragLeave={onDragLeave}
        onDragOver={onDragOver}
        onDrop={onDrop}
      >
        <Show when={state.messages.length === 0}>
          <Empty class="flex-1 border-none">
            <EmptyHeader>
              <EmptyMedia variant="icon">
                <MessagesSquare />
              </EmptyMedia>
              <EmptyTitle>No messages yet</EmptyTitle>
              <EmptyDescription>Say hello to the Agent to get started.</EmptyDescription>
            </EmptyHeader>
          </Empty>
        </Show>

        <Show when={state.messages.length > 0}>
          <MessageScroller class="flex-1">
            <MessageScrollerViewport class="py-3">
              <MessageScrollerContent class="gap-3">
                <For each={rows()}>
                  {(row) => (
                    <Show
                      when={row.kind === "msg"}
                      fallback={
                        <MessageScrollerItem class="px-3 py-1">
                          <Marker variant="separator" class="text-[0.7rem] uppercase tracking-wide">
                            <MarkerContent>{(row as { label: string }).label}</MarkerContent>
                          </Marker>
                        </MessageScrollerItem>
                      }
                    >
                      {(() => {
                        const m = (row as { message: DisplayMessage }).message;
                        return (
                          <MessageScrollerItem scrollAnchor={m.author === "owner"}>
                            <MessageBubble
                              message={m}
                              refTarget={m.ref !== null ? messagesById().get(m.ref) : undefined}
                              highlighted={highlightedId() === m.id}
                              queued={
                                m.mode === "next_turn" && thinking() && unansweredOwnerIds().has(m.id)
                              }
                              onTapQuote={scrollToId}
                              onOpenImage={(src, alt) => setLightbox({ src, alt })}
                              onRetry={(cid) => getClient()?.retrySend(cid)}
                              onCancelQueued={(cid) => getClient()?.cancelQueued(cid)}
                            />
                          </MessageScrollerItem>
                        );
                      })()}
                    </Show>
                  )}
                </For>

                {/* Live "Thinking…" status via a shimmering Marker (ephemeral),
                    with the running turn's live tool-call rows beneath it. */}
                <Show when={thinking() || state.liveToolCalls.length > 0}>
                  <MessageScrollerItem class="flex flex-col gap-1.5 px-4 py-1">
                    <Show when={thinking()}>
                      <Marker>
                        <MarkerContent class="shimmer text-sm">
                          {state.agentActivity.text ?? "Thinking…"}
                        </MarkerContent>
                      </Marker>
                    </Show>
                    <Show when={state.liveToolCalls.length > 0}>
                      <LiveToolCalls calls={state.liveToolCalls} />
                    </Show>
                  </MessageScrollerItem>
                </Show>
              </MessageScrollerContent>
            </MessageScrollerViewport>
            <JumpToLatest />
          </MessageScroller>
        </Show>

        {/* Drop overlay. */}
        <Show when={dragging()}>
          <div class="pointer-events-none absolute inset-2 z-40 flex flex-col items-center justify-center gap-2 rounded-xl border-2 border-dashed border-primary bg-background/80 backdrop-blur-sm">
            <Upload class="size-8 text-primary" />
            <span class="text-sm font-medium text-foreground">Drop files to attach</span>
          </div>
        </Show>
      </div>

      <Composer
        replyingTo={replyingTo()}
        attachments={attachments}
        thinking={thinking()}
        prefill={state.composerPrefill}
        onConsumePrefill={clearComposerPrefill}
        onCancelReply={clearComposerDraft}
        onSend={handleSend}
        onStop={() => getClient()?.cancelTurn()}
        getLastOwnerBody={lastOwnerBody}
      />

      <Lightbox
        src={lightbox()?.src ?? null}
        alt={lightbox()?.alt ?? ""}
        onClose={() => setLightbox(null)}
      />
    </div>
  );
}
