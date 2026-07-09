import { Check, Clock, GitFork, MoreHorizontal, RotateCcw } from "lucide-solid";
import { createMemo, createSignal, onCleanup, onMount, Show } from "solid-js";
import type { InboxItem, QuickReply } from "../../protocol";
import { state } from "../../store/store";
import { isItemRead, isResolvedStatus, latestReplyForAnchor, sideChatForItem } from "../../store/selectors";
import { createSeenTimer } from "../../lib/auto-read";
import { snippet } from "../../lib/format";
import { Markdown } from "../Markdown";
import { Button } from "../ui/button";
import { Card } from "../ui/card";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "../ui/dropdown-menu";
import { QuickReplyButtons } from "./QuickReplyButtons";
import { ReplyInput } from "./ReplyInput";

interface Props {
  item: InboxItem;
  /** Send a reply anchored to this item, in place — no navigation. Shared by
   * the Quick Reply taps and the inline freeform input. */
  onSendReply: (item: InboxItem, body: string) => void;
  /** Secondary affordance: jump to the item's Anchor in Chat. */
  onJumpToChat: (item: InboxItem) => void;
  /** Mark done (wire `archive_item`, ADR-0009 terminal state) — ⋯ menu only.
   * Non-destructive: the item stays findable under Done. */
  onDelete: (item: InboxItem) => void;
  /** Mark read (auto-read on view/interaction, or the ⋯ "Mark read"): sends
   * read_item + optimistic flip. */
  onRead: (item: InboxItem) => void;
  /** Mark unread (⋯ menu): client-only override, no wire op. */
  onMarkUnread: (item: InboxItem) => void;
  /** v2.0 (ADR-0008): "Discuss" (fresh) / "in progress · resume" (a live side
   * chat already exists for this item) — the effort ladder's third rung after
   * Quick Reply and Reply. */
  onDiscuss: (item: InboxItem) => void;
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

export function InboxItemCard(props: Props) {
  const isOpen = () => props.item.status === "open";
  // v2.1 (ADR-0009): "done" is the terminal state (legacy "archived" is a
  // synonym); the card dims and shows a non-destructive "Done" tag.
  const isDone = () => isResolvedStatus(props.item.status);
  // Effective read state (wire `read` minus any local "Mark unread" override).
  const read = () => isItemRead(props.item, state.unreadOverrides);
  // Email-like "unread" = an open item not yet seen. Deleted items are never
  // "unread" (they've left the active list).
  const unread = () => isOpen() && !read();
  // Auto-read candidate: open, still unread on the wire, and not being kept
  // unread by an explicit "Mark unread". (Manual unread suppresses auto-read so
  // marking unread then leaving the card on screen doesn't instantly re-read.)
  const autoReadCandidate = () =>
    isOpen() && props.item.read !== true && !state.unreadOverrides.includes(props.item.id);

  // Reveal the inline input on non-requires_response cards via "Reply"; on
  // requires_response cards it is expanded by default.
  const [revealed, setRevealed] = createSignal(false);
  const showInput = () => isOpen() && (props.item.requires_response || revealed());

  // v2.0: a live side chat for this item, if any — flips the "Discuss"
  // affordance to "in progress · resume" (derived from hello_ok.side_chats +
  // open/closed tracking, never a bespoke flag).
  const sideChatRef = () => sideChatForItem(state.sideChatRefs, props.item.id);

  // Replied state, derived from Chat (no persisted inbox reply state): the
  // latest owner message anchored to this item, if any.
  const reply = createMemo(() => latestReplyForAnchor(state.messages, props.item.anchor));

  // --- Auto-read: email-like "seen" once visible ~1.5s or on interaction. ---
  let cardEl: HTMLElement | undefined;
  const markSeen = () => {
    if (autoReadCandidate()) props.onRead(props.item);
  };
  const seen = createSeenTimer({ onSeen: markSeen });

  onMount(() => {
    // jsdom (unit tests) has no IntersectionObserver; the interaction path and
    // the ⋯ "Mark read" still cover read there. The headless scenario installs
    // a stub observer to exercise the view→read path.
    if (typeof IntersectionObserver === "undefined" || !cardEl) return;
    const observer = new IntersectionObserver((entries) => {
      for (const entry of entries) {
        seen.setVisible(entry.isIntersecting && autoReadCandidate());
      }
    });
    observer.observe(cardEl);
    onCleanup(() => observer.disconnect());
  });
  onCleanup(() => seen.dispose());

  // Interacting with the card (reveal input, quick reply, inline send) counts
  // as seen immediately.
  const reveal = () => {
    setRevealed(true);
    seen.interacted();
  };
  const sendReply = (body: string) => {
    seen.interacted();
    props.onSendReply(props.item, body);
  };

  return (
    <Card
      ref={(el: HTMLElement) => (cardEl = el)}
      size="sm"
      class="mx-3 gap-2 border-l-2 px-3 py-3 transition-opacity"
      classList={{
        "border-l-primary": props.item.requires_response,
        "border-l-transparent": !props.item.requires_response,
        "opacity-60": isDone(),
      }}
      data-read={read() ? "true" : "false"}
      data-status={props.item.status}
    >
      <div class="flex items-center justify-between gap-2">
        <span class="flex items-center gap-1.5">
          <Show when={unread()}>
            <span
              class="size-2 shrink-0 rounded-full bg-primary"
              aria-label="Unread"
              data-slot="unread-dot"
            />
          </Show>
          <span class="text-[0.7rem] text-muted-foreground">{formatTime(props.item.ts)}</span>
        </span>
        <div class="flex items-center gap-1">
          <Show when={isDone()}>
            <span class="inline-flex items-center gap-1 text-[0.68rem] uppercase tracking-[0.03em] text-muted-foreground">
              <Check class="size-3 text-status-success" aria-hidden="true" />
              Done
            </span>
          </Show>
          <DropdownMenu>
            <DropdownMenuTrigger
              class="-mr-1 rounded p-1 text-muted-foreground transition-colors hover:text-foreground"
              aria-label="More actions"
            >
              <MoreHorizontal class="size-4" />
            </DropdownMenuTrigger>
            <DropdownMenuContent>
              <Show when={isOpen()}>
                <DropdownMenuItem onSelect={reveal}>Reply</DropdownMenuItem>
                <Show
                  when={read()}
                  fallback={
                    <DropdownMenuItem onSelect={() => props.onRead(props.item)}>
                      Mark read
                    </DropdownMenuItem>
                  }
                >
                  <DropdownMenuItem onSelect={() => props.onMarkUnread(props.item)}>
                    Mark unread
                  </DropdownMenuItem>
                </Show>
              </Show>
              <DropdownMenuItem onSelect={() => props.onJumpToChat(props.item)}>
                View in chat
              </DropdownMenuItem>
              {/* v2.1 (ADR-0009): "Mark done" replaces the destructive
                  "Delete" — resolving an item is non-destructive (it stays
                  findable under Done), so no destructive styling and no
                  confirm. */}
              <Show when={isOpen()}>
                <DropdownMenuSeparator />
                <DropdownMenuItem onSelect={() => props.onDelete(props.item)}>
                  Mark done
                </DropdownMenuItem>
              </Show>
            </DropdownMenuContent>
          </DropdownMenu>
        </div>
      </div>

      {/* Content: full-strength when unread (the "bold email" look), dimmed once
          read/dealt-with, extra-muted when deleted. */}
      <div
        classList={{
          "font-medium text-foreground": unread(),
          "text-muted-foreground": !unread(),
        }}
      >
        <Markdown>{props.item.content}</Markdown>
      </div>

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
          onTap={(qr: QuickReply) => sendReply(qr.value)}
        />
      </Show>

      <Show when={showInput()}>
        <ReplyInput
          autofocus={revealed()}
          placeholder="Reply…"
          onSend={(body) => sendReply(body)}
        />
      </Show>

      {/* Effort ladder, rung 2 and 3 (rung 1 is QuickReplyButtons above):
          "Reply" only while the input isn't already showing; "Discuss" (or,
          for an item with a live side chat, "in progress · resume") stays
          reachable regardless — even mid-typing an answer, the Owner can
          still bail into a side chat instead. Always labeled, never icon-only
          (critique: icon-only leave/discard-style controls cause mis-taps). */}
      <Show when={isOpen()}>
        <div class="-ml-2.5 mt-1 flex flex-wrap items-center gap-1">
          <Show when={!showInput()}>
            <Button type="button" variant="link" size="sm" onClick={reveal}>
              Reply
            </Button>
          </Show>
          <Show
            when={sideChatRef()}
            fallback={
              <Button
                type="button"
                variant="link"
                size="sm"
                class="gap-1"
                onClick={() => props.onDiscuss(props.item)}
              >
                <GitFork class="size-3.5" aria-hidden="true" />
                Discuss
              </Button>
            }
          >
            <Button
              type="button"
              variant="link"
              size="sm"
              class="gap-1 text-status-active"
              onClick={() => props.onDiscuss(props.item)}
            >
              <GitFork class="size-3.5" aria-hidden="true" />
              in progress · resume
            </Button>
          </Show>
        </div>
      </Show>
    </Card>
  );
}
