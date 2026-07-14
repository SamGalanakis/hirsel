import { ArrowDown, ChevronUp, CircleAlert, LoaderCircle, MessagesSquare, Upload, X } from "lucide-solid";
import { createEffect, createMemo, createSignal, For, onCleanup, onMount, Show, untrack } from "solid-js";
import type { Blob, SendMode } from "../../protocol";
import {
  clearComposerAutofocus,
  clearComposerDraft,
  clearComposerPrefill,
  clearLastConclusion,
  clearPendingSideChatOpen,
  clearProtocolError,
  clearScrollTarget,
  closeRightRegion,
  openSideChat,
  setActiveSideChatSc,
  setComposerReplyTarget,
  showCanvas,
  state,
} from "../../store/store";
import { canvasViews, chatViews, sideChatForPing } from "../../store/selectors";
import { CanvasButton, CanvasRail, CanvasSheet } from "../views/CanvasSurface";
import { ViewRenderer } from "../../views/ViewRenderer";
import { focusMainComposer } from "../../lib/focus";
import { AgentStatus } from "./AgentStatus";
import { ModelChip } from "./ModelChip";
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
  useMessageScroller,
  useMessageScrollerScrollable,
  useMessageScrollerVisibility,
} from "../ui/message-scroller";
import { Composer } from "./Composer";
import { Lightbox } from "./Lightbox";
import { MessageBubble } from "./MessageBubble";
import { Timeline } from "./Timeline";
import { createComposerAttachments } from "./useAttachments";

const HIGHLIGHT_MS = 1600;

/** Rendered chat window (D10): only the most-recent N rows are in the DOM at
 * once; "load older" reveals `RENDER_STEP` more from the in-memory buffer (which
 * the reducer caps at MESSAGES_CAP). Keeps the always-open PWA's render + unseen
 * scan bounded no matter how long the session runs. */
const RENDER_WINDOW = 200;
const RENDER_STEP = 100;

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

type MsgRow = {
  kind: "msg";
  key: string;
  message: DisplayMessage;
  /** This message clusters under a same-author neighbour within CLUSTER_GAP_MS
   * (tight top gap). */
  clustered: boolean;
  /** This message is the last of its cluster, so it carries the shared
   * timestamp/meta footer. */
  showMeta: boolean;
};
type Row = { kind: "day"; key: string; label: string } | MsgRow;

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

/** Same restraint as the per-tool duration: whole seconds while a turn runs,
 * rolling to m/s past a minute. Feeds the running-turn elapsed indicator. */
function formatElapsed(ms: number): string {
  const s = Math.floor(ms / 1000);
  if (s < 60) return `${s}s`;
  return `${Math.floor(s / 60)}m${s % 60}s`;
}

/** How close in time two same-author messages must be to cluster (tight gap,
 * shared footer) rather than stand alone with their own meta. */
const CLUSTER_GAP_MS = 5 * 60 * 1000;

export function ChatView() {
  const [highlightedId, setHighlightedId] = createSignal<number | null>(null);
  const [lightbox, setLightbox] = createSignal<{ src: string; alt: string } | null>(null);
  const [dragging, setDragging] = createSignal(false);
  let dragDepth = 0;

  const attachments = createComposerAttachments();

  // Consume the one-shot composer-focus intent (the phone queue-home chat bar's
  // dormant pill drills in with `focusComposer`): land with the main composer
  // focused and the keyboard up. On mount, since this component remounts on each
  // queue→chat drill-in and the flag is set synchronously before we mount.
  onMount(() => {
    if (untrack(() => state.composerAutofocus)) {
      focusMainComposer();
      clearComposerAutofocus();
    }
  });

  // How many of the tail to render. Grows via "load older"; the window always
  // slices from the end so newly-arrived messages stay visible at the bottom.
  const [renderLimit, setRenderLimit] = createSignal(RENDER_WINDOW);
  const windowStart = createMemo(() => Math.max(0, state.messages.length - renderLimit()));
  const windowedMessages = createMemo(() => state.messages.slice(windowStart()));
  const hasOlder = () => windowStart() > 0;

  // ref/quote resolution stays over the FULL buffer so a reply can still cite a
  // message that scrolled out of the render window.
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

  // Interleave day-break markers between messages when the calendar day changes,
  // and cluster consecutive same-author messages: a run within CLUSTER_GAP_MS
  // gets a tight gap and a single timestamp/meta footer at its boundary rather
  // than one per bubble. Built from the render window, not the full buffer.
  const rows = createMemo<Row[]>(() => {
    const out: Row[] = [];
    const msgRows: MsgRow[] = [];
    let lastDay: string | null = null;
    let prev: DisplayMessage | null = null;
    for (const m of windowedMessages()) {
      const key = dayKey(m.ts);
      const dayChanged = key !== lastDay;
      if (dayChanged) {
        out.push({ kind: "day", key: `day-${key}-${m.id}`, label: dayLabel(m.ts) });
        lastDay = key;
      }
      const clustered =
        !dayChanged &&
        prev !== null &&
        prev.author === m.author &&
        new Date(m.ts).getTime() - new Date(prev.ts).getTime() < CLUSTER_GAP_MS;
      const row: MsgRow = { kind: "msg", key: `msg-${m.id}`, message: m, clustered, showMeta: true };
      out.push(row);
      msgRows.push(row);
      prev = m;
    }
    // Meta shows only at a cluster boundary: when the next message does not
    // cluster onto this one (different author, a >gap pause, or a day break).
    for (let i = 0; i < msgRows.length; i++) {
      const next = msgRows[i + 1];
      msgRows[i].showMeta = !next || !next.clustered;
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
    openSideChat(ref.sc);
    clearPendingSideChatOpen();
  });

  // v2.0: a Side Chat conclusion just landed in main chat — close its sheet
  // (if that's the one on screen) and land-and-highlight the new owner bubble
  // with the same scroll+highlight machinery a quoted-ref tap uses (critique
  // P2, "peak-end positive": the work resolves into the main thread).
  createEffect(() => {
    const lastConclusion = state.lastConclusion;
    if (!lastConclusion) return;
    // Only tear the sheet down if it's the one currently on screen for this sc:
    // the region owns the pane, `activeSideChatSc` is the backing data.
    if (state.rightRegion === "sideChat" && state.activeSideChatSc === lastConclusion.sc) {
      // Genuine teardown — the sc concluded; clear the data and return to Pings.
      setActiveSideChatSc(null);
      closeRightRegion();
      // The side composer is gone; hand focus back to the main composer.
      focusMainComposer();
    }
    scrollToId(lastConclusion.messageId);
    clearLastConclusion();
  });

  // Generative-UI tier: auto-surface a NEW canvas view — but never STEAL an
  // occupied right region. Tracks the newest canvas instance id; when it changes
  // to a fresh id (a genuinely new view, not an in-place content update to a
  // dismissed one) the Canvas surfaces ONLY if the region is idle
  // (`rightRegion === "pings"`). If the user is in Settings/Processes/Side Chat,
  // the arrival is recorded (so it won't ambush them later) and the availability
  // affordance — the CanvasButton — appears instead. An in-place update to an
  // already-seen view does NOT re-nag.
  let lastCanvasId: string | undefined;
  createEffect(() => {
    const list = canvasViews(state.views);
    const newest = list.length > 0 ? list[list.length - 1].instance_id : undefined;
    if (!newest) {
      lastCanvasId = undefined;
      return;
    }
    if (newest === lastCanvasId) return;
    lastCanvasId = newest;
    if (untrack(() => state.rightRegion) === "pings") showCanvas();
  });

  const replyingTo = () =>
    state.composerDraft ? messagesById().get(state.composerDraft.ref) : null;

  const thinking = () => state.agentActivity.state === "thinking";

  // "Reply" from a message's actions menu: quote it into the composer (the same
  // composerDraft plumbing the Ping "Reply" uses) and hand focus to the input.
  function handleReply(id: number) {
    setComposerReplyTarget(id);
    focusMainComposer();
  }

  // Running-turn elapsed reading, ticking once a second while the agent is
  // thinking and reset the moment the turn ends. Self-contained: the effect
  // re-runs only when thinking() flips, so the interval is armed/cleared cleanly.
  const [turnElapsed, setTurnElapsed] = createSignal<number | null>(null);
  createEffect(() => {
    if (!thinking()) {
      setTurnElapsed(null);
      return;
    }
    const start = Date.now();
    setTurnElapsed(0);
    const iv = setInterval(() => setTurnElapsed(Date.now() - start), 1000);
    onCleanup(() => clearInterval(iv));
  });

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

  /** "Load older messages" header row (D10). Reveals RENDER_STEP more of the
   * in-memory history, holding scroll position steady, and auto-loads when the
   * Owner scrolls back to the top. Rendered inside the scroller so it can read
   * the scroll edges + the prepend-preserving helper. */
  function LoadOlder() {
    const scroller = useMessageScroller();
    const { atStart } = useMessageScrollerScrollable();
    const load = () =>
      scroller.preserveOnPrepend(() =>
        setRenderLimit((l) => Math.min(state.messages.length, l + RENDER_STEP)),
      );
    // Auto-load on scroll-to-top, but only once the Owner has scrolled away from
    // the top at least once — so the initial restore-to-bottom (where atStart is
    // momentarily true) never triggers a spurious prepend.
    let armed = false;
    createEffect(() => {
      if (!atStart()) {
        armed = true;
        return;
      }
      if (armed && untrack(() => hasOlder())) load();
    });
    return (
      <MessageScrollerItem class="flex justify-center px-3 py-2">
        <button
          type="button"
          class="inline-flex items-center gap-1 rounded-full border border-border bg-card px-3 py-1 text-xs text-muted-foreground transition-colors hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring"
          onClick={load}
        >
          <ChevronUp class="size-3.5" aria-hidden="true" />
          Load older messages
        </button>
      </MessageScrollerItem>
    );
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
          home. Right side carries the ModelChip + Canvas reopen affordance.
          Phone is untouched. */}
      <header class="hidden h-12 flex-shrink-0 items-center justify-between gap-2 border-b border-border px-4 rail:flex">
        <AgentStatus />
        <div class="flex min-w-0 shrink items-center gap-2">
          <ModelChip />
          <CanvasButton />
        </div>
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
        {/* A post-auth protocol error, made visible inline instead of vanishing
            into the console. Sits above the transcript; dismissed by the Owner. */}
        <Show when={state.protocolError}>
          <div class="mx-3 mt-3 flex items-start gap-2 rounded-md border border-destructive/30 bg-destructive/10 px-3 py-2 text-xs text-destructive">
            <CircleAlert class="mt-px size-4 shrink-0" aria-hidden="true" />
            <span class="min-w-0 flex-1 wrap-break-word">{state.protocolError}</span>
            <button
              type="button"
              class="-mr-0.5 shrink-0 rounded p-0.5 transition-colors hover:bg-destructive/20"
              aria-label="Dismiss error"
              onClick={clearProtocolError}
            >
              <X class="size-3.5" />
            </button>
          </div>
        </Show>

        {/* Empty vs. connecting: the empty state is reserved for a genuinely
            empty, post-hello thread. Before hello (or while the socket is down)
            we can't yet know the thread is empty, so we show a quiet restoring
            marker rather than falsely inviting the Owner to "start". */}
        <Show when={state.messages.length === 0}>
          <Show
            when={state.connection === "connected"}
            fallback={
              <div class="flex flex-1 flex-col items-center justify-center gap-2 text-muted-foreground">
                <LoaderCircle class="size-5 animate-spin" aria-hidden="true" />
                <span class="text-sm">
                  {state.connection === "reconnecting" ? "Restoring…" : "Connecting…"}
                </span>
              </div>
            }
          >
            <Empty class="flex-1 border-none">
              <EmptyHeader>
                <EmptyMedia variant="icon">
                  <MessagesSquare />
                </EmptyMedia>
                <EmptyTitle>Nothing here yet</EmptyTitle>
                <EmptyDescription>
                  Messages between you and the agent will appear here.
                </EmptyDescription>
              </EmptyHeader>
            </Empty>
          </Show>
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
                <Show when={hasOlder()}>
                  <LoadOlder />
                </Show>
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
                        const msgRow = row as MsgRow;
                        const m = msgRow.message;
                        return (
                          <MessageScrollerItem
                            scrollAnchor={m.author === "owner"}
                            class={msgRow.clustered ? "-mt-2" : undefined}
                          >
                            <MessageBubble
                              message={m}
                              refTarget={m.ref !== null ? messagesById().get(m.ref) : undefined}
                              showQuote={m.ref !== null && m.ref !== prevIdOf().get(m.id)}
                              turnDetails={state.turnDetails[m.id]}
                              isConclusion={state.conclusionChips.includes(m.id)}
                              highlighted={highlightedId() === m.id}
                              showMeta={msgRow.showMeta}
                              queued={
                                m.mode === "next_turn" && thinking() && unansweredOwnerIds().has(m.id)
                              }
                              onReply={handleReply}
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

                {/* Generative-UI tier: `chat`-placed views render as inline
                    blocks at the foot of the transcript, in arrival order. */}
                <Show when={chatViews(state.views).length > 0}>
                  <MessageScrollerItem class="flex flex-col gap-3 px-3 py-1">
                    <For each={chatViews(state.views)}>
                      {(v) => (
                        <ViewRenderer
                          spec={v.spec}
                          instanceId={v.instance_id}
                          placement={v.placement}
                        />
                      )}
                    </For>
                  </MessageScrollerItem>
                </Show>

                {/* Live "Thinking…" status via a shimmering Marker (ephemeral),
                    with the running turn's timeline (prose ↔ tools ↔ reasoning,
                    in seq order) beneath it. */}
                <Show when={thinking() || state.turnEvents.length > 0}>
                  <MessageScrollerItem class="flex flex-col gap-1.5 px-4 py-1">
                    <Show when={thinking()}>
                      <div class="flex min-w-0 items-center gap-2">
                        <Marker>
                          <MarkerContent class="shimmer text-sm">
                            {state.agentActivity.text ?? "Thinking…"}
                          </MarkerContent>
                        </Marker>
                        <Show when={turnElapsed() !== null}>
                          <span
                            class="shrink-0 font-mono text-[0.68rem] tabular-nums text-muted-foreground/60"
                            aria-label="Elapsed"
                          >
                            {formatElapsed(turnElapsed() as number)}
                          </span>
                        </Show>
                      </div>
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

      {/* Right region — ONE exclusive owner (v2.3): `state.rightRegion` picks
          exactly one of these to render; the others UNMOUNT (no hidden-focus
          leak, nothing to clip). Each desktop presentation is a single in-flow
          `<aside>` at the shared width token (Pings `clamp(340px,38vw,440px)`);
          on phone each is a full-screen sheet. Each self-gates on the region, so
          source order no longer encodes precedence — the enum does. Chat (the
          center pane) is always live to the left; closing any pane returns the
          region to `pings`.
            • Side Chat  — `rightRegion === "sideChat"` (backed by activeSideChatSc)
            • Canvas     — `rightRegion === "canvas"`   (CanvasRail / CanvasSheet)
            • Processes  — `rightRegion === "processes"`
            • Settings   — `rightRegion === "settings"`
            • Pings      — `rightRegion === "pings"` (idle resting state) */}
      <SideChatSheet />
      <CanvasRail />
      <PingsRail />
      <CanvasSheet />
      <ProcessesSheet />
      <SettingsSheet />
    </div>
  );
}
