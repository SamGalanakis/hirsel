import { ArrowDown, MessagesSquare, Upload } from "lucide-solid";
import { createEffect, createMemo, createSignal, For, Show } from "solid-js";
import type { Blob, SendMode } from "../../protocol";
import {
  clearComposerDraft,
  clearComposerPrefill,
  clearLastConclusion,
  clearPendingSideChatOpen,
  clearScrollTarget,
  setActiveSideChatSc,
  state,
} from "../../store/store";
import { sideChatForPing } from "../../store/selectors";
import { focusMainComposer } from "../../lib/focus";
import type { DisplayMessage } from "../../store/types";
import { getClient } from "../../ws/client";
import { PingsRail, TrayOverlay, TrayShelf } from "../inbox/Tray";
import { SideChatSheet } from "../inbox/SideChatSheet";
import { ProcessesSheet } from "../processes/ProcessesSheet";
import { SettingsSheet } from "../settings/SettingsSheet";
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
import { Timeline } from "./Timeline";
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

/** The slim desktop-only center-pane header's left datum: a calm agent-status
 * indicator (tint-chip vocabulary — a status dot + label), giving the
 * north-star question "what is my agent doing" a persistent desktop home.
 * Purely presentational; no indigo, 16px-max, quiet when idle. */
function AgentStatus() {
  const thinking = () => state.agentActivity.state === "thinking";
  return (
    <Show
      when={thinking()}
      fallback={
        <div class="flex min-w-0 items-center gap-2">
          <span class="size-1.5 shrink-0 rounded-full bg-status-idle" aria-hidden="true" />
          <span class="truncate text-xs text-muted-foreground">Agent · idle</span>
        </div>
      }
    >
      <div class="flex min-w-0 items-center gap-2">
        <span
          class="size-1.5 shrink-0 rounded-full bg-status-active motion-safe:animate-pulse"
          aria-hidden="true"
        />
        <span class="truncate text-xs text-status-active">
          {state.agentActivity.text ?? "Thinking…"}
        </span>
      </div>
    </Show>
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

  // The id of the message immediately preceding each message in the thread.
  // A reply whose ref *is* that neighbour is the ordinary adjacent
  // back-and-forth — the fill-vs-quiet bubble asymmetry already says who
  // answered whom, so no quoted-preview card is drawn. A quote is only surfaced
  // when the ref points somewhere non-contiguous (a quick-reply jump, an older
  // message, a side-chat conclusion), where it actually aids orientation.
  const prevIdOf = createMemo(() => {
    const map = new Map<number, number | null>();
    let prev: number | null = null;
    for (const m of state.messages) {
      map.set(m.id, prev);
      prev = m.id;
    }
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

  // v2.0 (ADR-0008): Discuss/Resume was tapped (see PingsView.handleDiscuss)
  // but the sheet couldn't open yet because the sc wasn't known. The moment a
  // sideChatRefs entry for that Ping appears — immediately for Resume (it
  // already existed), or once open_side_chat's response lands for a fresh
  // Discuss — open the sheet and consume the request.
  createEffect(() => {
    const pingId = state.pendingSideChatPingId;
    if (pingId === null) return;
    const ref = sideChatForPing(state.sideChatRefs, pingId);
    if (!ref) return;
    setActiveSideChatSc(ref.sc);
    clearPendingSideChatOpen();
  });

  // v2.0: a Side Chat conclusion just landed in main chat — close its sheet
  // (if that's the one on screen) and land-and-highlight the new owner bubble
  // with the same scroll+highlight machinery a quoted-ref tap uses (critique
  // P2, "peak-end positive": the work resolves into the main thread).
  createEffect(() => {
    const lastConclusion = state.lastConclusion;
    if (!lastConclusion) return;
    if (state.activeSideChatSc === lastConclusion.sc) {
      setActiveSideChatSc(null);
      // The side composer is gone; hand focus back to the main composer.
      focusMainComposer();
    }
    scrollToId(lastConclusion.messageId);
    clearLastConclusion();
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

  function handleSend(
    body: string,
    ref: number | null,
    mode: SendMode,
    blobs: Blob[],
    mentions: number[],
  ) {
    getClient()?.sendMessage(body, ref, { mode, attachments: blobs, mentions });
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
    // The desktop two-zone row (desktop-shell): a column on phone, a row at
    // `split`. The left zone is the chat pane (fills, `flex-1`); the right zone
    // is shared by precedence — a standing Pings rail by default, the Side Chat
    // panel when one is open, the Processes inspector docked over either. On
    // phone the side chat / processes are `fixed` out-of-flow sheets, so the row
    // degrades to the single chat column. `relative` anchors the Processes dock.
    <div class="relative flex min-h-0 flex-1 flex-col split:flex-row">
      <div class="flex min-h-0 min-w-0 flex-1 flex-col">
      {/* Desktop-only slim center-pane header (`hidden rail:flex`). Shares the
          h-12 top-bar height + border-b with the nav-rail brand block and the
          Pings/inspector pane headers, so one continuous hairline runs across
          all three panes and the "what is my agent doing" datum has a persistent
          home. Right side is reserved (kept clean). Phone is untouched. */}
      <header class="hidden h-12 flex-shrink-0 items-center justify-between gap-2 border-b border-border px-4 rail:flex">
        <AgentStatus />
        <span aria-hidden="true" />
      </header>
      {/* Measured inner: at `rail` width the chat pane fills the center of the
          3-pane shell but its content is capped to a reading measure (~680px)
          and centred in the pane (`rail:mx-auto`), so bubbles never stretch to
          hostile line lengths. The gutters here are now real structure — the pane
          sits between the nav rail and the context pane — not a lonely centered
          column beside a void. This wraps ONLY the transcript + tray shelf; the
          Composer bar bleeds full-width below and re-centers its own input at the
          same measure. Below `rail` this is a no-op and the phone/split widths
          are unchanged. */}
      <div class="flex min-h-0 w-full flex-1 flex-col rail:mx-auto rail:max-w-[680px]">
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
              {/* `justify-end` bottom-anchors a sparse transcript: it grows up
                  from the composer (void at top) like every desktop chat peer,
                  instead of top-anchoring with a big gap above the composer. The
                  content wrapper already carries `min-h-full h-max`, so once it
                  overflows there is no free space and the scroll/stick machinery
                  is unaffected. */}
              <MessageScrollerContent class="justify-end gap-3">
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
                              showQuote={m.ref !== null && m.ref !== prevIdOf().get(m.id)}
                              turnDetails={state.turnDetails[m.id]}
                              isConclusion={state.conclusionChips.includes(m.id)}
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
                    with the running turn's timeline (prose ↔ tools ↔ reasoning,
                    in seq order) beneath it. */}
                <Show when={thinking() || state.turnEvents.length > 0}>
                  <MessageScrollerItem class="flex flex-col gap-1.5 px-4 py-1">
                    <Show when={thinking()}>
                      <Marker>
                        <MarkerContent class="shimmer text-sm">
                          {state.agentActivity.text ?? "Thinking…"}
                        </MarkerContent>
                      </Marker>
                    </Show>
                    <Show when={state.turnEvents.length > 0}>
                      <Timeline events={state.turnEvents} />
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

        {/* Tray, expanded: an overlay over this message area, never a push —
            it must live inside this `relative` container so its absolute
            positioning resolves against the scroller, not the whole view. */}
        <TrayOverlay />
      </div>

      {/* Tray, collapsed: the shelf, pinned directly above the Composer. */}
      <TrayShelf />
      </div>

      {/* Composer bar bleeds full-width across the center pane (rail hairline →
          context hairline); its inner row re-centers at the prose measure so the
          input still aligns to the transcript. `rail:`-gated, so phone/split are
          pixel-identical. */}
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

      {/* Right region — precedence-ordered, one slot:
          • Side Chat panel when state.activeSideChatSc is set (fork-ui: a
            full-screen `fixed` sheet on phone, an in-flow right rail on wide
            viewports; never auto-opened — see the pendingSideChatPingId effect).
          • Otherwise the standing Pings rail at `rail` width (PingsRail hides
            itself while a side chat holds the region). */}
      <SideChatSheet />
      <PingsRail />

      {/* Processes / Settings: full-screen sheets on phone, right-docked
          inspectors over the right region on desktop — never covering the chat.
          Settings is a placeholder surface reachable only from the NavRail. */}
      <ProcessesSheet />
      <SettingsSheet />
    </div>
  );
}
