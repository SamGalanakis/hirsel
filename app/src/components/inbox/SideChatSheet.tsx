import { ArrowUp, ChevronDown, ChevronRight, MoreHorizontal, TriangleAlert } from "lucide-solid";
import { createEffect, createMemo, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import type { InboxItem } from "../../protocol";
import { dispatch, setActiveSideChatSc, state } from "../../store/store";
import type { DisplayMessage } from "../../store/types";
import { getClient } from "../../ws/client";
import { Markdown } from "../Markdown";
import { MessageBubble } from "../chat/MessageBubble";
import { Timeline } from "../chat/Timeline";
import { useTextInput } from "../chat/useTextInput";
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

// The Side Chat sheet (ADR-0008 / the binding opus design critique). Full-
// screen, not a partial half-sheet (P0: on a phone with the keyboard open a
// half-sheet leaves ~2 transcript lines). It is deliberately "a fancy reply
// composer": the same MessageBubble/Timeline/Markdown surfaces main Chat
// uses, framed by a labeled header + a pinned seed card rather than any
// recoloring, so it reads as *scoped*, never as a whole second app.

const HIGHLIGHT_MS = 1600;
const MAX_COMPOSER_HEIGHT_PX = 112;
const HINT_DISMISSED_KEY = "hirsel.sidechat.hintDismissed";

function hintDismissed(): boolean {
  try {
    return localStorage.getItem(HINT_DISMISSED_KEY) === "1";
  } catch {
    return false;
  }
}
function dismissHint(): void {
  try {
    localStorage.setItem(HINT_DISMISSED_KEY, "1");
  } catch {
    // Best-effort; the hint just reappears next session, which is harmless.
  }
}

/** "Side chat · <item title>" — a plain-text one-liner stripped of markdown
 * syntax so the title band never renders stray `#`/`*`/backticks. */
function titleSnippet(content: string | undefined): string {
  if (!content) return "this item";
  const firstLine = content.split("\n").find((l) => l.trim().length > 0) ?? content;
  const plain = firstLine.replace(/[#*`_>-]/g, "").trim();
  return plain.length > 48 ? `${plain.slice(0, 48)}…` : plain || "this item";
}

export function SideChatSheet() {
  return (
    <Show when={state.activeSideChatSc}>{(sc) => <SideChatPanel sc={sc()} />}</Show>
  );
}

function SideChatPanel(props: { sc: string }) {
  const sideChat = () => state.sideChats[props.sc];
  const item = (): InboxItem | undefined =>
    state.inbox.find((i) => i.id === sideChat()?.itemId);

  const [highlightedId, setHighlightedId] = createSignal<number | null>(null);
  const [seedExpanded, setSeedExpanded] = createSignal(true);
  const [hintVisible, setHintVisible] = createSignal(!hintDismissed());
  const [discardConfirmOpen, setDiscardConfirmOpen] = createSignal(false);
  const { value, setValue, coarse, setRef, focus, caretToEnd } = useTextInput(
    MAX_COMPOSER_HEIGHT_PX,
  );

  const offline = () => state.connection !== "connected";
  const thinking = () => sideChat()?.agentActivity.state === "thinking";
  const showingDraft = () => sideChat()?.draft !== null && sideChat()?.draft !== undefined;
  const busy = () => sideChat()?.confirming || sideChat()?.discarding;

  // Esc: priority-ordered by what's on top. Discard confirm -> cancel it.
  // Confirm-draft sheet -> "Keep editing" (never discard — the anti-pattern
  // this design guards against is a Back that silently throws work away).
  // Otherwise -> leave-alive, same as the header's `‹ Chat`.
  onMount(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      if (discardConfirmOpen()) {
        setDiscardConfirmOpen(false);
      } else if (showingDraft()) {
        dispatch({ type: "side_chat_keep_editing", sc: props.sc });
      } else {
        setActiveSideChatSc(null);
      }
    };
    window.addEventListener("keydown", onKeyDown);
    onCleanup(() => window.removeEventListener("keydown", onKeyDown));
  });

  const messagesById = createMemo(() => {
    const map = new Map<number, DisplayMessage>();
    for (const m of sideChat()?.messages ?? []) map.set(m.id, m);
    return map;
  });

  function scrollToId(id: number) {
    document.getElementById(`side-msg-${id}`)?.scrollIntoView({ behavior: "smooth", block: "center" });
    setHighlightedId(id);
    setTimeout(() => setHighlightedId((cur) => (cur === id ? null : cur)), HIGHLIGHT_MS);
  }

  function lastOwnerBody(): string | null {
    const msgs = sideChat()?.messages ?? [];
    for (let i = msgs.length - 1; i >= 0; i--) {
      if (msgs[i].author === "owner") return msgs[i].body;
    }
    return null;
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
      return; // the window-level handler above handles leave/keep-editing
    }
    if (e.key === "ArrowUp" && value().length === 0) {
      const last = lastOwnerBody();
      if (last !== null) {
        e.preventDefault();
        setValue(last);
        caretToEnd();
      }
      return;
    }
    if (coarse()) return;
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      submit();
    }
  }

  return (
    <div class="fixed inset-0 z-40 flex flex-col bg-background pb-[env(safe-area-inset-bottom)]">
      <header class="flex flex-shrink-0 flex-col border-b border-border pt-[env(safe-area-inset-top)]">
        <div class="flex items-center gap-1 px-1 py-2">
          <button
            type="button"
            class="flex items-center gap-0.5 rounded-md px-2 py-1 text-sm text-foreground transition-colors hover:bg-muted"
            onClick={() => setActiveSideChatSc(null)}
            aria-label="Leave side chat (stays open — resume any time)"
          >
            <ChevronRight class="size-5 rotate-180" aria-hidden="true" />
            Chat
          </button>
          <div class="min-w-0 flex-1" />
          <Show when={offline()}>
            <span class="flex items-center gap-1 px-1 text-[0.68rem] text-status-attention">
              <span
                class="size-1.5 animate-pulse rounded-full bg-status-attention"
                aria-hidden="true"
              />
              reconnecting…
            </span>
          </Show>
          <Button
            type="button"
            size="sm"
            disabled={!sideChat() || offline() || sideChat()?.drafting || busy() || sideChat()?.ended}
            title={offline() ? "Reconnect to conclude" : undefined}
            onClick={() => getClient()?.concludeSideChat(props.sc)}
          >
            Conclude
          </Button>
          <DropdownMenu>
            <DropdownMenuTrigger
              class="rounded p-1.5 text-muted-foreground transition-colors hover:text-foreground disabled:pointer-events-none disabled:opacity-50"
              aria-label="More actions"
              disabled={!sideChat() || busy() || sideChat()?.ended}
            >
              <MoreHorizontal class="size-4" />
            </DropdownMenuTrigger>
            <DropdownMenuContent>
              <DropdownMenuItem variant="destructive" onSelect={() => setDiscardConfirmOpen(true)}>
                Discard
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        </div>
        <div class="truncate px-3 pb-2 text-xs text-muted-foreground">
          Side chat · {titleSnippet(item()?.content)}
        </div>
      </header>

      {/* Non-blocking: the Agent can archive the item mid-side-chat. Conclude
          and Discard both remain reachable (archive-on-conclude is a no-op if
          already archived). */}
      <Show when={sideChat()?.itemArchived}>
        <div class="flex flex-shrink-0 items-center gap-1.5 border-b border-border bg-status-attention/10 px-3 py-1.5 text-xs text-status-attention">
          <TriangleAlert class="size-3.5 shrink-0" aria-hidden="true" />
          The Agent closed this item.
        </div>
      </Show>

      <Show
        when={!sideChat()?.ended}
        fallback={
          <div class="flex flex-1 flex-col items-center justify-center gap-3 p-6 text-center">
            <p class="text-sm text-muted-foreground">This side chat ended.</p>
            <Button type="button" variant="secondary" onClick={() => setActiveSideChatSc(null)}>
              Back to item
            </Button>
          </div>
        }
      >
        <Show
          when={sideChat()}
          fallback={
            <div class="flex flex-1 items-center justify-center text-sm text-muted-foreground">
              Resuming…
            </div>
          }
        >
          <MessageScroller class="flex-1">
            <MessageScrollerViewport class="py-3">
              <MessageScrollerContent class="gap-3">
                {/* Pinned, collapsible seed card: the visual seed for what is,
                    on a fresh open, an otherwise-empty transcript (the actual
                    seed lives host-side in the session's prompt layer). */}
                <MessageScrollerItem class="px-3">
                  <div class="rounded-lg border border-border bg-muted/40 p-2.5">
                    <button
                      type="button"
                      class="flex w-full items-center gap-1.5 text-left"
                      aria-expanded={seedExpanded()}
                      onClick={() => setSeedExpanded((v) => !v)}
                    >
                      <ChevronDown
                        class="size-3.5 shrink-0 text-muted-foreground transition-transform"
                        classList={{ "-rotate-90": !seedExpanded() }}
                        aria-hidden="true"
                      />
                      <Marker variant="separator" class="min-w-0 flex-1">
                        <MarkerContent class="text-[0.68rem] uppercase tracking-wide">
                          Forked from your chat — seeded with this item and recent context
                        </MarkerContent>
                      </Marker>
                    </button>
                    <Show when={seedExpanded()}>
                      <div class="mt-2 border-l-2 border-border pl-2 text-sm text-muted-foreground">
                        <Markdown>{item()?.content ?? ""}</Markdown>
                      </div>
                    </Show>
                  </div>

                  {/* One-time first-run hint (dismissible; never shown again). */}
                  <Show when={hintVisible()}>
                    <div class="mt-2 flex items-start gap-2 rounded-md bg-primary/10 px-2.5 py-2 text-xs text-foreground">
                      <span class="flex-1">
                        This is a scoped chat — the Agent already has your recent context, and only
                        your final reply goes back to the main chat.
                      </span>
                      <button
                        type="button"
                        class="shrink-0 text-muted-foreground transition-colors hover:text-foreground"
                        aria-label="Dismiss hint"
                        onClick={() => {
                          dismissHint();
                          setHintVisible(false);
                        }}
                      >
                        Got it
                      </button>
                    </div>
                  </Show>
                </MessageScrollerItem>

                <For each={sideChat()?.messages ?? []}>
                  {(m) => (
                    <MessageScrollerItem scrollAnchor={m.author === "owner"}>
                      <div id={`side-msg-${m.id}`}>
                        <MessageBubble
                          message={m}
                          refTarget={m.ref !== null ? messagesById().get(m.ref) : undefined}
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

                {/* sc-scoped thinking/timeline only — this never animates from
                    (or leaks into) the main turn's, and vice versa. */}
                <Show when={thinking() || (sideChat()?.turnEvents.length ?? 0) > 0}>
                  <MessageScrollerItem class="flex flex-col gap-1.5 px-4 py-1">
                    <Show when={thinking()}>
                      <Marker>
                        <MarkerContent class="shimmer text-sm">
                          {sideChat()?.agentActivity.text ?? "Thinking…"}
                        </MarkerContent>
                      </Marker>
                    </Show>
                    <Show when={(sideChat()?.turnEvents.length ?? 0) > 0}>
                      <Timeline events={sideChat()?.turnEvents ?? []} />
                    </Show>
                  </MessageScrollerItem>
                </Show>

                <Show when={sideChat()?.drafting}>
                  <MessageScrollerItem class="px-4 py-1">
                    <Marker>
                      <MarkerContent class="shimmer text-sm">Drafting your reply…</MarkerContent>
                    </Marker>
                  </MessageScrollerItem>
                </Show>
              </MessageScrollerContent>
            </MessageScrollerViewport>
          </MessageScroller>

          <div class="flex-shrink-0 border-t border-border bg-card px-3 py-2">
            <div class="flex items-end gap-2">
              <Textarea
                ref={setRef}
                rows={1}
                class="max-h-28 min-h-0 flex-1 resize-none py-2 leading-snug"
                placeholder="Message the Agent…"
                value={value()}
                disabled={sideChat()?.drafting}
                onInput={(e) => setValue(e.currentTarget.value)}
                onKeyDown={handleKeyDown}
              />
              <Button
                type="button"
                size="icon"
                class="shrink-0 rounded-full"
                onClick={submit}
                disabled={value().trim().length === 0 || sideChat()?.drafting}
                aria-label="Send"
              >
                <ArrowUp class="size-5" />
              </Button>
            </div>
          </div>
        </Show>
      </Show>

      <Show when={showingDraft() && sideChat()}>
        <ConcludeConfirmSheet
          sc={props.sc}
          draft={sideChat()?.draft ?? ""}
          item={item()}
          onKeepEditing={() => dispatch({ type: "side_chat_keep_editing", sc: props.sc })}
          onSend={(text) => {
            const anchor = item()?.anchor;
            if (anchor === undefined) return;
            getClient()?.confirmConclusion(props.sc, text, anchor);
          }}
        />
      </Show>

      <Show when={discardConfirmOpen()}>
        <DiscardConfirmDialog
          onCancel={() => setDiscardConfirmOpen(false)}
          onConfirm={() => {
            getClient()?.discardSideChat(props.sc);
            setDiscardConfirmOpen(false);
            setActiveSideChatSc(null);
          }}
        />
      </Show>
    </div>
  );
}

/** The conclude flow's confirmation sheet (critique, "detailed ergonomics"):
 * title "Send this reply?", the original question shown above for a
 * requires_response item, an editable/scrollable never-truncated textarea
 * pre-filled with the draft, primary "Send reply" (the plain verb — "Conclude"
 * stays the domain term for the frame, not the button a first-timer reads),
 * secondary "Keep editing" that returns to the side chat without discarding. */
function ConcludeConfirmSheet(props: {
  sc: string;
  draft: string;
  item: InboxItem | undefined;
  onKeepEditing: () => void;
  onSend: (text: string) => void;
}) {
  const [text, setText] = createSignal(props.draft);

  // A resumed/late-arriving draft can replace an empty seed; once the Owner
  // starts editing we never clobber their edits from underneath them.
  let seeded = props.draft;
  createEffect(() => {
    if (props.draft !== seeded && text() === seeded) {
      seeded = props.draft;
      setText(props.draft);
    }
  });

  return (
    <div
      class="fixed inset-0 z-50 flex flex-col bg-background pb-[env(safe-area-inset-bottom)]"
      role="dialog"
      aria-modal="true"
      aria-label="Send this reply?"
    >
      <header class="flex-shrink-0 border-b border-border px-4 py-3 pt-[calc(env(safe-area-inset-top)+0.75rem)]">
        <h2 class="m-0 text-base font-semibold">Send this reply?</h2>
      </header>
      <div class="thin-scrollbar flex-1 overflow-y-auto p-4">
        <Show when={props.item?.requires_response}>
          <div class="mb-3 rounded-md border-l-2 border-border bg-muted/40 p-2.5">
            <div class="mb-1 text-[0.68rem] uppercase tracking-wide text-muted-foreground">
              Original question
            </div>
            <div class="text-sm text-muted-foreground">
              <Markdown>{props.item?.content ?? ""}</Markdown>
            </div>
          </div>
        </Show>
        <Textarea
          class="min-h-40 w-full resize-y rounded-md border border-border bg-background p-3 text-sm leading-relaxed"
          value={text()}
          onInput={(e) => setText(e.currentTarget.value)}
          aria-label="Reply text"
        />
      </div>
      <div class="flex flex-shrink-0 flex-col gap-2 border-t border-border p-3">
        <div class="flex flex-col gap-1">
          <Button
            type="button"
            disabled={text().trim().length === 0}
            onClick={() => props.onSend(text())}
          >
            Send reply
          </Button>
          <span class="text-center text-[0.7rem] text-muted-foreground">
            Goes to your chat as your reply; this side chat closes.
          </span>
        </div>
        <Button type="button" variant="secondary" onClick={props.onKeepEditing}>
          Keep editing
        </Button>
      </div>
    </div>
  );
}

/** Discard's confirm — deliberately a separate, effortful step (critique: the
 * one exit that must never be a fat-finger away from "leave"). */
function DiscardConfirmDialog(props: { onCancel: () => void; onConfirm: () => void }) {
  return (
    <div
      class="fixed inset-0 z-50 flex items-center justify-center bg-black/40 p-4"
      onClick={props.onCancel}
    >
      <div
        class="w-full max-w-sm rounded-lg border border-border bg-card p-4 shadow-lg"
        role="alertdialog"
        aria-modal="true"
        aria-label="Discard this side chat?"
        onClick={(e) => e.stopPropagation()}
      >
        <h2 class="m-0 text-sm font-semibold text-foreground">Discard this side chat?</h2>
        <p class="mt-1 text-sm text-muted-foreground">
          Your notes here are deleted; the item stays open.
        </p>
        <div class="mt-4 flex justify-end gap-2">
          <Button type="button" variant="secondary" size="sm" onClick={props.onCancel}>
            Cancel
          </Button>
          <Button type="button" variant="destructive" size="sm" onClick={props.onConfirm}>
            Discard
          </Button>
        </div>
      </div>
    </div>
  );
}
