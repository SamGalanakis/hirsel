import { ArrowUp, ChevronRight, MoreHorizontal, Square, X } from "lucide-solid";
import { createEffect, createMemo, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { untrack } from "solid-js";
import type { EventItem } from "../../protocol";
import { createFocusTrap, createMediaFlag, focusMainComposer } from "../../lib/focus";
import { decideEventWithUndo, undoDecide } from "../../lib/event-decide";
import { handleSubmitKeys } from "../../lib/submitKeymap";
import { toast } from "../../lib/toast";
import { isEventResolved } from "../../store/selectors";
import { closeRightRegion, setActiveSideChatSc, state } from "../../store/store";
import type { DisplayMessage } from "../../store/types";
import { getClient } from "../../ws/client";
import { EventCardRenderer } from "../../views/EventCardRenderer";
import { stripInlineMarkdown } from "../Markdown";
import { MessageBubble } from "../chat/MessageBubble";
import { Timeline } from "../chat/Timeline";
import { useTextInput } from "../chat/useTextInput";
import { DecidedStrip, EventCardHeader } from "../eventq/EventCard";
import { Button } from "../ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "../ui/dropdown-menu";
import { Marker, MarkerContent } from "../ui/marker";
import {
  MessageScroller,
  MessageScrollerContent,
  MessageScrollerItem,
  MessageScrollerViewport,
} from "../ui/message-scroller";
import { Textarea } from "../ui/textarea";

// The event-fork surface (ADR-0008 forks over ADR-0012 events, v2.4) — the
// flagship Discuss experience. It supersedes the legacy SideChatSheet for
// event-addressed forks, reusing its solid pieces (the MessageScroller/Bubble/
// Timeline transcript, the composer, the focus/offline handling) but re-shaped
// around the event model:
//
//   • The EVENT CARD is PINNED at the top — rendered through the SAME
//     EventCardRenderer + EventCardHeader the queue uses, so it looks like THE
//     card, not a quoted seed. It stays read-only EXCEPT a judgment's options,
//     which remain tappable: you decide right from the pinned card.
//   • Below it, the scoped transcript + its own composer. The fork is a real
//     side session — you talk it through with the agent, and only your decision
//     goes back to the main chat.
//   • Deciding (from the pinned card, or when the fork agent calls `fork.decide`
//     over the wire) resolves the event, closes the fork, and posts one
//     anchor-refed "Discussed @name → <label>" owner line to main chat (host
//     side). The panel shows a brief decided confirmation (DecidedStrip
//     vocabulary) then closes the loop.
//   • A summary/info fork has no decision — its exit is a plain, silent Close.
//
// Responsive, one component tree (same contract as the legacy sheet):
//   • Phone (<900px): a full-screen `fixed` slide-in sheet (a true modal — Tab
//     trapped, motion-safe).
//   • Wide (≥900px `split`): an in-flow right rail beside a still-live main Chat.
// The `side-chat-sheet` data-slot name is kept so the right-region machinery and
// its tests address it unchanged.

const MAX_COMPOSER_HEIGHT_PX = 112;
/** Below `split` the fork is a full-screen sheet (trap Tab); at/above it an
 * in-flow rail beside a live main chat, where trapping Tab would strand the
 * keyboard on that side, so Tab stays free. */
const SPLIT_MQ = "(min-width: 900px)";
/** How long the decided confirmation lingers (recovery reachable via its Undo)
 * before the fork closes the loop and the pane dismisses. */
const DECIDED_HOLD_MS = 1500;
const HIGHLIGHT_MS = 1600;

function prefersReduced(): boolean {
  return (
    typeof window !== "undefined" &&
    window.matchMedia?.("(prefers-reduced-motion: reduce)").matches
  );
}

export function EventForkPanel() {
  // Shown only while the fork pane owns the right region (v2.3). The sc
  // (`activeSideChatSc`) stays set when you leave the pane — the fork is
  // alive/resumable underneath — so the region gate, not the data, mounts it.
  return (
    <Show when={state.rightRegion === "sideChat" && state.activeSideChatSc}>
      {(sc) => <ForkPanel sc={sc()} />}
    </Show>
  );
}

function ForkPanel(props: { sc: string }) {
  const fork = () => state.sideChats[props.sc];
  // The forked Event: the LIVE wire truth in the queue (kept current by
  // `event_upsert`, so its `status`/`fork_sc` reflect a decide the moment it
  // lands), keyed by the fork's event id (== pingId).
  const event = (): EventItem | undefined =>
    state.events.find((e) => e.id === fork()?.pingId);

  const [highlightedId, setHighlightedId] = createSignal<number | null>(null);
  const { value, setValue, coarse, setRef, focus, caretToEnd } = useTextInput(
    MAX_COMPOSER_HEIGHT_PX,
    `fork:${props.sc}`,
  );
  let panelRef: HTMLDivElement | undefined;

  const phone = createMediaFlag("(max-width: 899.98px)");
  const offline = () => state.connection !== "connected";
  const thinking = () => fork()?.agentActivity.state === "thinking";
  const ended = () => fork()?.ended === true;
  const isJudgment = () => event()?.kind === "judgment";
  const decided = (): boolean => {
    const ev = event();
    return ev ? isEventResolved(ev, state.eventDecideOverrides) : false;
  };

  // Leave-alive: return the region to idle (the panel unmounts) while keeping
  // the fork's DATA (`activeSideChatSc`) set, so it stays alive/resumable
  // underneath (the "discussion open · resume" chip on the card). Hands focus
  // back to the main composer. Used by the header leave control, Esc, and the
  // "ended" back button, so focus handoff lives in exactly one place.
  function leave() {
    closeRightRegion();
    focusMainComposer();
  }

  // Genuine teardown after a decide or an explicit Close: clear the fork DATA
  // too (this sc is done) and return the region to idle.
  function closeLoop() {
    setActiveSideChatSc(null);
    closeRightRegion();
    focusMainComposer();
  }

  // On open (fresh Discuss/Ask or Resume), land focus in the fork composer.
  onMount(() => {
    queueMicrotask(() => focus());
  });

  // Focus trap + Escape. With no nested modal here, this panel's trap is topmost
  // and Escape leaves-alive. Tab is trapped only when the sheet is full-screen
  // (phone), never on the desktop split where it sits beside a live main chat.
  onMount(() => {
    createFocusTrap(() => panelRef, {
      onEscape: leave,
      trapTab: () => !window.matchMedia(SPLIT_MQ).matches,
      restoreTo: () =>
        document.querySelector<HTMLTextAreaElement>('[data-composer="main"]'),
    });
  });

  // Close-the-loop: when the event flips to decided — the owner tapped the pinned
  // card, or the fork agent called `fork.decide` over the wire — hold the decided
  // confirmation briefly, then close the pane. `confirming` gates the composer
  // off and the DecidedStrip on. The fire re-checks decided(), so an Undo inside
  // the hold keeps the panel open; a genuine close returns focus to main chat.
  const [confirming, setConfirming] = createSignal(false);
  let closeTimer: ReturnType<typeof setTimeout> | undefined;
  createEffect(() => {
    if (decided()) {
      if (!confirming()) {
        setConfirming(true);
        const hold = prefersReduced() ? 0 : DECIDED_HOLD_MS;
        closeTimer = setTimeout(() => {
          closeTimer = undefined;
          if (untrack(decided)) closeLoop();
          else setConfirming(false);
        }, hold);
      }
    } else if (confirming()) {
      // Undo before the hold elapsed: stand the close down, stay open.
      setConfirming(false);
      if (closeTimer) {
        clearTimeout(closeTimer);
        closeTimer = undefined;
      }
    }
  });
  onCleanup(() => {
    if (closeTimer) clearTimeout(closeTimer);
  });

  const messagesById = createMemo(() => {
    const map = new Map<number, DisplayMessage>();
    for (const m of fork()?.messages ?? []) map.set(m.id, m);
    return map;
  });

  function scrollToId(id: number) {
    document.getElementById(`fork-msg-${id}`)?.scrollIntoView({ behavior: "smooth", block: "center" });
    setHighlightedId(id);
    setTimeout(() => setHighlightedId((cur) => (cur === id ? null : cur)), HIGHLIGHT_MS);
  }

  function lastOwnerBody(): string | null {
    const msgs = fork()?.messages ?? [];
    for (let i = msgs.length - 1; i >= 0; i--) {
      if (msgs[i].author === "owner") return msgs[i].body;
    }
    return null;
  }

  // Decide from the pinned card — silent: the on-card DecidedStrip carries the
  // single Undo while it is up (a toast on top would double it), and the queue's
  // own decided card keeps recovery reachable after the pane closes. This flips
  // the optimistic decide override → decided() → the close-the-loop above.
  function decide(action: string, data: unknown) {
    const ev = event();
    if (!ev || decided()) return;
    const label =
      data && typeof data === "object" && "label" in data && typeof data.label === "string"
        ? (data as { label: string }).label
        : undefined;
    decideEventWithUndo(ev.id, action, data, label, { silent: true });
  }

  // Close / abandon the fork with no decision (a summary's plain exit, or a
  // judgment abandoned via ⋯): discard the session — silent, the event stays
  // open, and the host clears `fork_sc` so the card's resume chip retires.
  function closeDiscussion() {
    getClient()?.discardSideChat(props.sc);
    closeLoop();
  }

  function submit() {
    const body = value().trim();
    if (body.length === 0) return;
    getClient()?.sendSideMessage(props.sc, body, null);
    setValue("");
    focus();
  }

  function handleKeyDown(e: KeyboardEvent) {
    if (e.key === "Escape") {
      if (thinking()) {
        e.preventDefault();
        getClient()?.cancelSideTurn(props.sc);
      }
      return; // the trap handles leave
    }
    handleSubmitKeys(e, {
      value,
      coarse,
      onSend: submit,
      recallLast: lastOwnerBody,
      onRecall: (text) => {
        setValue(text);
        caretToEnd();
      },
    });
  }

  // Paste-to-attach isn't supported in a fork (text-only v1). Tell the owner so
  // a pasted file isn't silently swallowed.
  function handlePaste(e: ClipboardEvent) {
    const items = e.clipboardData?.items;
    if (!items) return;
    for (const item of items) {
      if (item.kind === "file") {
        e.preventDefault();
        toast("Attachments aren't supported in a discussion — send files from the main composer.");
        return;
      }
    }
  }

  return (
    <div
      ref={panelRef}
      tabindex={-1}
      data-slot="side-chat-sheet"
      role="dialog"
      aria-modal={phone() ? "true" : undefined}
      aria-label={event()?.name ? `Discussion about @${event()?.name}` : "Discussion"}
      class="flex flex-col bg-background outline-none
        fixed inset-0 z-40 pb-[env(safe-area-inset-bottom)]
        motion-safe:animate-in motion-safe:fade-in motion-safe:slide-in-from-bottom motion-safe:duration-200
        split:relative split:inset-auto split:z-auto
        split:w-[clamp(340px,38vw,440px)] split:shrink-0 split:pb-0
        split:border-l split:border-border
        motion-safe:split:slide-in-from-bottom-0 motion-safe:split:slide-in-from-right-4"
    >
      {/* Header: pure orientation (leave · title · status · ⋯). The leave control
          is `‹ Chat` on phone (a back gesture) and a plain close `✕` on the
          desktop split (Chat is right there on the left, so "back" would lie). */}
      <header class="flex flex-shrink-0 items-center gap-1.5 border-b border-border px-1.5 py-2 pt-[calc(env(safe-area-inset-top)+0.5rem)] split:pt-2 rail:h-12 rail:py-0">
        <button
          type="button"
          class="flex shrink-0 items-center gap-0.5 rounded-md px-2 py-1 text-sm text-foreground transition-colors hover:bg-muted focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/50 split:order-last split:gap-0"
          onClick={leave}
          aria-label="Leave discussion (stays open — resume any time)"
        >
          <ChevronRight class="size-5 rotate-180 split:hidden" aria-hidden="true" />
          <X class="hidden size-4 split:block" aria-hidden="true" />
          <span class="split:hidden">Chat</span>
        </button>
        <div class="min-w-0 flex-1 truncate text-xs text-muted-foreground">
          Discussing{" "}
          <Show when={event()?.name} fallback={<span class="text-foreground/90">this card</span>}>
            <span class="font-mono text-foreground/90">@{event()?.name}</span>
          </Show>
        </div>
        <Show when={offline()}>
          <span class="flex shrink-0 items-center gap-1 px-1 text-[0.68rem] text-status-attention">
            <span
              class="size-1.5 animate-pulse rounded-full bg-status-attention"
              aria-hidden="true"
            />
            reconnecting…
          </span>
        </Show>
        {/* A judgment's abandon path lives in ⋯ (deciding is the primary close);
            a summary's plain Close is a first-class bar below the transcript. */}
        <Show when={isJudgment()}>
          <DropdownMenu>
            <DropdownMenuTrigger
              class="shrink-0 rounded p-1.5 text-muted-foreground transition-colors hover:text-foreground disabled:pointer-events-none disabled:opacity-50"
              aria-label="Discussion actions"
              disabled={!fork() || confirming() || ended()}
            >
              <MoreHorizontal class="size-4" />
            </DropdownMenuTrigger>
            <DropdownMenuContent>
              <DropdownMenuItem onSelect={closeDiscussion}>Close discussion</DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        </Show>
      </header>

      <Show
        when={!ended() || decided()}
        fallback={
          <div class="flex flex-1 flex-col items-center justify-center gap-3 p-6 text-center">
            <p class="text-sm text-muted-foreground">This discussion ended.</p>
            <Button type="button" variant="secondary" onClick={leave}>
              Back to the queue
            </Button>
          </div>
        }
      >
        {/* The PINNED event card — THE card, decidable at the top. Rendered
            through the shared EventCardHeader + EventCardRenderer, so it carries
            the same needs-you accent and body as the queue; only its options are
            interactive (until decided). A very long card scrolls within a capped
            region so the transcript is never crowded out. */}
        <div class="flex-shrink-0 overflow-y-auto border-b border-border bg-background px-3 pb-2.5 pt-3 split:max-h-[46%]">
          <Show
            when={event()}
            fallback={
              <div class="rounded-xl border border-dashed border-border px-3.5 py-6 text-center text-xs text-muted-foreground">
                Loading the card…
              </div>
            }
          >
            {(ev) => (
              <div
                class="relative overflow-hidden rounded-xl border border-border bg-card shadow-sm"
                classList={{ "bg-muted/30 shadow-none": ev().kind !== "judgment" }}
              >
                <EventCardHeader ev={ev()} />
                <div class="px-3.5 pb-3.5 pt-2">
                  <EventCardRenderer ui={ev().ui} disabled={decided()} onAction={decide} />
                </div>
                {/* Close-the-loop: the decided confirmation, reusing DecidedStrip
                    verbatim — "@name → decided · posted to <producer>", with the
                    single Undo. The panel then closes; the main chat shows the
                    "Discussed @name → <label>" owner line via the normal msg
                    broadcast. */}
                <Show when={confirming()}>
                  <DecidedStrip ev={ev()} onUndo={(id) => undoDecide(id)} />
                </Show>
              </div>
            )}
          </Show>
        </div>

        {/* The scoped transcript. */}
        <MessageScroller class="flex-1">
          <MessageScrollerViewport class="py-3">
            <MessageScrollerContent class="gap-3">
              <Show when={(fork()?.messages.length ?? 0) === 0 && !thinking()}>
                <MessageScrollerItem class="px-4 py-2">
                  <p class="text-xs leading-relaxed text-muted-foreground">
                    Talk it through with the agent — it already has your recent context. Only your
                    decision goes back to the main chat.
                  </p>
                </MessageScrollerItem>
              </Show>

              <For each={fork()?.messages ?? []}>
                {(m) => (
                  <MessageScrollerItem scrollAnchor={m.author === "owner"}>
                    <div id={`fork-msg-${m.id}`}>
                      <MessageBubble
                        message={m}
                        refTarget={m.ref !== null ? messagesById().get(m.ref) : undefined}
                        showQuote={m.ref !== null}
                        highlighted={highlightedId() === m.id}
                        queued={false}
                        onTapQuote={scrollToId}
                        onOpenImage={() => {}}
                        onRetry={() => {}}
                        onCancelQueued={() => {}}
                      />
                    </div>
                  </MessageScrollerItem>
                )}
              </For>

              {/* sc-scoped thinking/timeline only — never animates from (or leaks
                  into) the main turn's, and vice versa. */}
              <Show when={thinking() || (fork()?.turnEvents.length ?? 0) > 0}>
                <MessageScrollerItem class="flex flex-col gap-1.5 px-4 py-1">
                  <Show when={thinking()}>
                    <Marker>
                      <MarkerContent class="shimmer text-sm">
                        {stripInlineMarkdown(fork()?.agentActivity.text ?? "Thinking…")}
                      </MarkerContent>
                    </Marker>
                  </Show>
                  <Show when={(fork()?.turnEvents.length ?? 0) > 0}>
                    <Timeline events={fork()?.turnEvents ?? []} />
                  </Show>
                </MessageScrollerItem>
              </Show>
            </MessageScrollerContent>
          </MessageScrollerViewport>
        </MessageScroller>

        {/* Once decided, the composer gives way to a calm wrap-up line while the
            pane winds down — no more typing into a resolved discussion. */}
        <Show
          when={!confirming()}
          fallback={
            <div class="flex flex-shrink-0 items-center gap-2 border-t border-border bg-muted/20 px-4 py-2.5 text-xs text-muted-foreground">
              <span class="min-w-0 flex-1 truncate">
                Decided — your reply is going to the main chat.
              </span>
            </div>
          }
        >
          {/* A summary/info fork has no decision — its exit is a plain, silent
              Close, given a first-class bar (a judgment abandons via ⋯ instead). */}
          <Show when={!isJudgment()}>
            <div class="flex flex-shrink-0 items-center gap-2 border-t border-border bg-muted/20 px-3 py-1.5">
              <span class="min-w-0 flex-1 truncate text-[0.7rem] text-muted-foreground">
                Done here? Close the discussion.
              </span>
              <Button
                type="button"
                size="sm"
                variant="secondary"
                class="shrink-0"
                onClick={closeDiscussion}
              >
                Close discussion
              </Button>
            </div>
          </Show>

          <div class="flex-shrink-0 border-t border-border bg-card px-3 py-2">
            <div class="flex items-end gap-2">
              <Textarea
                ref={setRef}
                rows={1}
                data-composer="side"
                class="max-h-28 min-h-0 flex-1 resize-none py-2 leading-snug"
                placeholder="Reply in this discussion…"
                aria-label="Reply in this discussion"
                value={value()}
                onInput={(e) => setValue(e.currentTarget.value)}
                onKeyDown={handleKeyDown}
                onPaste={handlePaste}
              />
              <Show when={thinking()}>
                <Button
                  type="button"
                  variant="secondary"
                  size="icon"
                  class="shrink-0 rounded-full"
                  classList={{ "size-11": coarse() }}
                  aria-label="Stop the agent"
                  onClick={() => getClient()?.cancelSideTurn(props.sc)}
                >
                  <Square class="size-4 fill-current" />
                </Button>
              </Show>
              <Button
                type="button"
                size="icon"
                class="shrink-0 rounded-full"
                classList={{ "size-11": coarse() }}
                onClick={submit}
                disabled={value().trim().length === 0}
                aria-label="Send"
              >
                <ArrowUp class="size-5" />
              </Button>
            </div>
          </div>
        </Show>
      </Show>
    </div>
  );
}
