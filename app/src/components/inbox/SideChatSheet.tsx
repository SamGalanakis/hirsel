import { ArrowUp, ChevronDown, ChevronRight, MoreHorizontal, TriangleAlert, X } from "lucide-solid";
import { createEffect, createMemo, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import type { Ping } from "../../protocol";
import { focusMainComposer } from "../../lib/focus";
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

// The Side Chat surface (ADR-0008 / the binding opus design critique, iterated
// for the fork-ui pass). Responsive presentation, one component tree:
//   • Phone (<900px): a full-screen `fixed` sheet — not a partial half-sheet
//     (P0: on a phone with the keyboard open a half-sheet leaves ~2 transcript
//     lines).
//   • Wide (≥900px): a Slack-style right rail, in-flow beside a still-live main
//     Chat (App widens the shell; ChatView lays the row out). The rail's own
//     border + header do the "this is scoped" labeling the sheet's full-screen
//     frame does on phone.
// Either way it is deliberately "a fancy reply composer": the same
// MessageBubble/Timeline/Markdown surfaces main Chat uses, framed by a labeled
// header + a pinned seed card rather than any recoloring, so it reads as
// *scoped*, never as a whole second app.
//
// Conclude relocation (fork-ui): the goal action moved OFF the header (Sam:
// "the conclude thing should be elsewhere") into a dedicated "wrap-up bar"
// pinned directly above the composer — where the Owner's hands and eyes
// already are, and where "I'm done working this out, draft my reply" reads as
// a natural continuation of composing rather than a top-nav command. The
// header is now pure orientation (leave + title + status + ⋯). The plain verb
// "Wrap up" replaces "Conclude"; the confirm sheet's button stays "Send
// reply". The prominence mapping still holds: leave = back/close (cheapest,
// reversible), wrap up = the one prominent button, discard = buried in ⋯.

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
  if (!content) return "this ping";
  const firstLine = content.split("\n").find((l) => l.trim().length > 0) ?? content;
  const plain = firstLine.replace(/[#*`_>-]/g, "").trim();
  return plain.length > 48 ? `${plain.slice(0, 48)}…` : plain || "this ping";
}

export function SideChatSheet() {
  return (
    <Show when={state.activeSideChatSc}>{(sc) => <SideChatPanel sc={sc()} />}</Show>
  );
}

function SideChatPanel(props: { sc: string }) {
  const sideChat = () => state.sideChats[props.sc];
  const ping = (): Ping | undefined =>
    state.pings.find((p) => p.id === sideChat()?.pingId);

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

  // Leave-alive: close the surface (the side chat persists, resumable) and hand
  // focus back to the main composer. The single exit used by the header
  // leave/close control, Esc, the "ended" back button, and post-discard — so
  // focus handoff is defined in exactly one place.
  function leave() {
    setActiveSideChatSc(null);
    focusMainComposer();
  }

  // On open (fresh Discuss or Resume), land focus in the side composer — the
  // one composer that should hold focus while a side chat is on screen (on the
  // desktop split both composers are visible; exactly one is focused).
  onMount(() => {
    queueMicrotask(() => focus());
  });

  // Esc: priority-ordered by what's on top. Discard confirm -> cancel it.
  // Confirm-draft sheet -> "Keep editing" (never discard — the anti-pattern
  // this design guards against is a Back that silently throws work away).
  // Otherwise -> leave-alive.
  onMount(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      if (discardConfirmOpen()) {
        setDiscardConfirmOpen(false);
      } else if (showingDraft()) {
        dispatch({ type: "side_chat_keep_editing", sc: props.sc });
      } else {
        leave();
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
    <div
      data-slot="side-chat-sheet"
      class="flex flex-col bg-background
        fixed inset-0 z-40 pb-[env(safe-area-inset-bottom)]
        animate-in fade-in slide-in-from-bottom duration-200
        split:relative split:inset-auto split:z-auto
        split:w-[clamp(340px,38vw,440px)] split:shrink-0 split:pb-0
        split:border-l split:border-border
        split:slide-in-from-bottom-0 split:slide-in-from-right-4"
    >
      {/* Header: pure orientation now (leave · title · status · ⋯) — Conclude
          has moved to the wrap-up bar above the composer. Single row (density
          cleanup). The leave control is `‹ Chat` on phone (a back gesture) and
          a plain close `✕` on the desktop split (Chat is right there on the
          left, so "back" would be a lie). */}
      <header class="flex flex-shrink-0 items-center gap-1.5 border-b border-border px-1.5 py-2 pt-[calc(env(safe-area-inset-top)+0.5rem)] split:pt-2">
        <button
          type="button"
          class="flex items-center gap-0.5 rounded-md px-2 py-1 text-sm text-foreground transition-colors hover:bg-muted"
          onClick={leave}
          aria-label="Leave side chat (stays open — resume any time)"
        >
          <ChevronRight class="size-5 rotate-180 split:hidden" aria-hidden="true" />
          <X class="hidden size-4 split:block" aria-hidden="true" />
          <span class="split:hidden">Chat</span>
        </button>
        <div class="min-w-0 flex-1 truncate text-xs text-muted-foreground">
          Side chat ·{" "}
          <Show when={ping()?.name} fallback={titleSnippet(ping()?.content)}>
            <span class="font-mono text-foreground/90">@{ping()?.name}</span>
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
        <DropdownMenu>
          <DropdownMenuTrigger
            class="shrink-0 rounded p-1.5 text-muted-foreground transition-colors hover:text-foreground disabled:pointer-events-none disabled:opacity-50"
            aria-label="Side chat actions"
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
      </header>

      {/* Non-blocking: the Agent can resolve the Ping mid-side-chat. Conclude
          and Discard both remain reachable (resolve-on-conclude is a no-op if
          already done). */}
      <Show when={sideChat()?.pingResolved}>
        <div class="flex flex-shrink-0 items-center gap-1.5 border-b border-border bg-status-attention/10 px-3 py-1.5 text-xs text-status-attention">
          <TriangleAlert class="size-3.5 shrink-0" aria-hidden="true" />
          The Agent closed this Ping.
        </div>
      </Show>

      <Show
        when={!sideChat()?.ended}
        fallback={
          <div class="flex flex-1 flex-col items-center justify-center gap-3 p-6 text-center">
            <p class="text-sm text-muted-foreground">This side chat ended.</p>
            <Button type="button" variant="secondary" onClick={leave}>
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
                  <div class="rounded-md bg-muted/30 p-2">
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
                          Forked from your chat — seeded with this ping and recent context
                        </MarkerContent>
                      </Marker>
                    </button>
                    <Show when={seedExpanded()}>
                      <div class="mt-2 border-l-2 border-border pl-2 text-sm text-muted-foreground">
                        <Markdown>{ping()?.content ?? ""}</Markdown>
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

          {/* Wrap-up bar — the relocated Conclude. Pinned directly above the
              composer (where your hands are), the one prominent action. Plain
              verb "Wrap up"; it drafts your reply, which you then edit/confirm
              in the "Send reply?" sheet. Disabled while offline or drafting. */}
          <div class="flex flex-shrink-0 items-center gap-2 border-t border-border bg-muted/20 px-3 py-1.5">
            <span class="min-w-0 flex-1 truncate text-[0.7rem] text-muted-foreground">
              <Show when={offline()} fallback="Ready? Wrap up to send your reply to chat.">
                Reconnect to wrap up.
              </Show>
            </span>
            <Button
              type="button"
              size="sm"
              class="shrink-0"
              disabled={offline() || sideChat()?.drafting || busy()}
              title={offline() ? "Reconnect to wrap up" : undefined}
              onClick={() => getClient()?.concludeSideChat(props.sc)}
            >
              Wrap up
            </Button>
          </div>

          <div class="flex-shrink-0 border-t border-border bg-card px-3 py-2">
            <div class="flex items-end gap-2">
              <Textarea
                ref={setRef}
                rows={1}
                data-composer="side"
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
          ping={ping()}
          onKeepEditing={() => dispatch({ type: "side_chat_keep_editing", sc: props.sc })}
          onSend={(text) => {
            const anchor = ping()?.anchor;
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
            leave();
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
  ping: Ping | undefined;
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
    // Phone: a full-screen sheet (room to edit with the keyboard up). Desktop
    // (`split`): a centered, scrimmed, capped modal over the side-panel region —
    // never a whole-screen blank for a two-line confirmation (P2). The chat
    // stays visible on the left; only the side panel is scrimmed.
    <div
      class="fixed inset-0 z-50 flex flex-col bg-background pb-[env(safe-area-inset-bottom)]
        split:absolute split:z-30 split:items-center split:justify-center split:bg-black/40 split:p-3 split:pb-3"
      role="dialog"
      aria-modal="true"
      aria-label="Send this reply?"
    >
      <div
        class="flex min-h-0 w-full flex-1 flex-col bg-background
          split:max-h-full split:max-w-[400px] split:flex-none split:overflow-hidden
          split:rounded-lg split:border split:border-border split:shadow-lg"
      >
        <header class="flex-shrink-0 border-b border-border px-4 py-3 pt-[calc(env(safe-area-inset-top)+0.75rem)] split:pt-3">
          <h2 class="m-0 text-base font-semibold">Send this reply?</h2>
        </header>
        <div class="thin-scrollbar flex-1 overflow-y-auto p-4">
          <Show when={props.ping?.requires_response}>
            <div class="mb-3 rounded-md border-l-2 border-border bg-muted/40 p-2.5">
              <div class="mb-1 text-[0.68rem] uppercase tracking-wide text-muted-foreground">
                Original question
              </div>
              <div class="text-sm text-muted-foreground">
                <Markdown>{props.ping?.content ?? ""}</Markdown>
              </div>
            </div>
          </Show>
          <Textarea
            class="min-h-40 w-full resize-y rounded-md border border-border bg-background p-3 text-sm leading-relaxed split:min-h-32"
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
    </div>
  );
}

/** Discard's confirm — deliberately a separate, effortful step (critique: the
 * one exit that must never be a fat-finger away from "leave"). */
function DiscardConfirmDialog(props: { onCancel: () => void; onConfirm: () => void }) {
  return (
    <div
      class="fixed inset-0 z-50 flex items-center justify-center bg-black/40 p-4 split:absolute split:z-30"
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
          Your notes here are deleted; the ping stays open.
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
