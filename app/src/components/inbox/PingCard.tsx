import { Check, Clock, GitFork, MoreHorizontal, RotateCcw } from "lucide-solid";
import { createMemo, createSignal, onCleanup, onMount, Show } from "solid-js";
import type { Ping, QuickReply } from "../../protocol";
import { state } from "../../store/store";
import { isPingRead, isResolvedStatus, latestReplyForAnchor, sideChatForPing } from "../../store/selectors";
import { createSeenTimer } from "../../lib/auto-read";
import { snippet } from "../../lib/format";
import { Markdown } from "../Markdown";
import { Button } from "../ui/button";
import { Card } from "../ui/card";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "../ui/dropdown-menu";
import { QuickReplyButtons } from "./QuickReplyButtons";
import { ReplyInput } from "./ReplyInput";

interface Props {
  ping: Ping;
  /** Send a reply anchored to this Ping, in place — no navigation. Shared by
   * the Quick Reply taps and the inline freeform input. */
  onSendReply: (ping: Ping, body: string) => void;
  /** Secondary affordance: jump to the Ping's Anchor in Chat. */
  onJumpToChat: (ping: Ping) => void;
  /** Mark done (wire `resolve_ping`, ADR-0009 terminal state) — now a one-tap
   * visible control, not buried in the ⋯ menu. Non-destructive: the Ping stays
   * findable under Done. */
  onResolve: (ping: Ping) => void;
  /** Mark read (auto-read on view/interaction, or the ⋯ "Mark read"): sends
   * read_ping + optimistic flip. */
  onRead: (ping: Ping) => void;
  /** Mark unread (⋯ menu): client-only override, no wire op. */
  onMarkUnread: (ping: Ping) => void;
  /** v2.0 (ADR-0008): "Discuss" (fresh) / "in progress · resume" (a live side
   * chat already exists for this Ping) — the effort ladder's third rung after
   * Quick Reply and Reply. */
  onDiscuss: (ping: Ping) => void;
  /** Render the full card (the promoted state): requires-response, the selected
   * row, or a Done item. When false, the Ping renders as a dense one-line row
   * that expands on tap. Defaults to true so a standalone card renders in full. */
  expanded?: boolean;
  /** Dense-row tap: promote this Ping to a card. No-op in card mode. */
  onSelect?: (ping: Ping) => void;
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

const prefersReducedMotion = () =>
  typeof window !== "undefined" &&
  typeof window.matchMedia === "function" &&
  window.matchMedia("(prefers-reduced-motion: reduce)").matches;

export function PingCard(props: Props) {
  const isOpen = () => props.ping.status === "open";
  // v2.1 (ADR-0009): "done" is the terminal state (legacy "archived" is a
  // synonym); the card dims and shows a non-destructive "Done" tag.
  const isDone = () => isResolvedStatus(props.ping.status);
  // Effective read state (wire `read` minus any local "Mark unread" override).
  const read = () => isPingRead(props.ping, state.unreadOverrides);
  // Email-like "unread" = an open Ping not yet seen. Done Pings are never
  // "unread" (they've left the active list).
  const unread = () => isOpen() && !read();
  // Auto-read candidate: open, still unread on the wire, and not being kept
  // unread by an explicit "Mark unread". (Manual unread suppresses auto-read so
  // marking unread then leaving the card on screen doesn't instantly re-read.)
  const autoReadCandidate = () =>
    isOpen() && props.ping.read !== true && !state.unreadOverrides.includes(props.ping.id);

  // Card (promoted) vs. dense row. Defaults to a card for standalone use.
  const asCard = () => props.expanded ?? true;
  // The human one-liner leads (Inter medium); falls back to the body when a Ping
  // ships no description. The machine @handle is demoted to a chip.
  const titleText = () => props.ping.description.trim() || props.ping.content;
  // Show the content body only when it adds something over the title (i.e. the
  // Ping has a distinct description) — never echo the same line twice.
  const showBody = () =>
    props.ping.description.trim().length > 0 &&
    props.ping.content.trim() !== props.ping.description.trim();

  // Reveal the inline input on non-requires_response cards via "Reply"; on
  // requires_response cards it is expanded by default.
  const [revealed, setRevealed] = createSignal(false);
  const showInput = () => isOpen() && (props.ping.requires_response || revealed());

  // v2.0: a live side chat for this Ping, if any — flips the "Discuss"
  // affordance to "in progress · resume" (derived from hello_ok.side_chats +
  // open/closed tracking, never a bespoke flag).
  const sideChatRef = () => sideChatForPing(state.sideChatRefs, props.ping.id);

  // Replied state, derived from Chat (no persisted reply state): the latest
  // owner message anchored to this Ping, if any.
  const reply = createMemo(() => latestReplyForAnchor(state.messages, props.ping.anchor));

  // --- Auto-read: email-like "seen" once visible ~1.5s or on interaction. ---
  let cardEl: HTMLElement | undefined;
  const markSeen = () => {
    if (autoReadCandidate()) props.onRead(props.ping);
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
    props.onSendReply(props.ping, body);
  };

  // --- Swipe-to-done (phone, touch only). A short left drag past the threshold
  // resolves the Ping — the gesture peer of the one-tap Mark done control. It
  // never blocks a tap (only engages once the drag is clearly horizontal) and
  // yields to vertical scrolling. ---
  const SWIPE_TRIGGER = 96;
  const [dragX, setDragX] = createSignal(0);
  let startX = 0;
  let startY = 0;
  let tracking = false;
  let engaged = false;

  const onPointerDown = (e: PointerEvent) => {
    if (e.pointerType === "mouse" || !isOpen()) return;
    // Let interactive controls own their own pointer.
    if ((e.target as HTMLElement).closest("button, a, textarea, input, [role='menuitem']")) return;
    startX = e.clientX;
    startY = e.clientY;
    tracking = true;
    engaged = false;
  };
  const onPointerMove = (e: PointerEvent) => {
    if (!tracking) return;
    const dx = e.clientX - startX;
    const dy = e.clientY - startY;
    if (!engaged) {
      if (Math.abs(dx) < 8 && Math.abs(dy) < 8) return;
      if (Math.abs(dy) >= Math.abs(dx)) {
        tracking = false; // vertical intent — hand it back to the scroller
        return;
      }
      engaged = true;
      (e.currentTarget as HTMLElement).setPointerCapture?.(e.pointerId);
    }
    setDragX(Math.max(-140, Math.min(0, dx)));
  };
  const endSwipe = () => {
    if (tracking && engaged && dragX() <= -SWIPE_TRIGGER) props.onResolve(props.ping);
    tracking = false;
    engaged = false;
    setDragX(0);
  };
  const swipeProps = () => ({
    onPointerDown,
    onPointerMove,
    onPointerUp: endSwipe,
    onPointerCancel: endSwipe,
    style: {
      "touch-action": "pan-y",
      transform: dragX() ? `translateX(${dragX()}px)` : undefined,
      transition: dragX() === 0 && !prefersReducedMotion() ? "transform 150ms ease-out" : undefined,
    } as Record<string, string | undefined>,
  });

  // The machine @handle, demoted to a mono chip beside the timestamp.
  const HandleChip = () => (
    <span
      class="shrink-0 rounded bg-muted px-1.5 py-0.5 font-mono text-[0.68rem] text-muted-foreground"
      data-slot="ping-name"
    >
      @{props.ping.name}
    </span>
  );

  const Time = () => (
    <span class="shrink-0 text-[0.7rem] text-muted-foreground">{formatTime(props.ping.ts)}</span>
  );

  // One-tap Mark done — thumb-grade 44px hit area, glyph stays 16px. Negative
  // margins keep the visual density while the target stays touch-legal.
  const MarkDoneButton = () => (
    <Show when={isOpen()}>
      <button
        type="button"
        class="-my-2 grid size-11 shrink-0 place-items-center rounded text-muted-foreground transition-colors hover:text-status-success focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/50"
        aria-label="Mark done"
        title="Mark done"
        onClick={() => props.onResolve(props.ping)}
      >
        <Check class="size-4" aria-hidden="true" />
      </button>
    </Show>
  );

  // ⋯ overflow — Mark read/unread + View in chat only (Mark done graduated to a
  // visible control). 44px trigger, 16px glyph.
  const MoreMenu = () => (
    <DropdownMenu>
      <DropdownMenuTrigger
        class="-my-2 -mr-2 grid size-11 shrink-0 place-items-center rounded text-muted-foreground transition-colors hover:text-foreground"
        aria-label="More actions"
      >
        <MoreHorizontal class="size-4" />
      </DropdownMenuTrigger>
      <DropdownMenuContent>
        <Show when={isOpen()}>
          <Show
            when={read()}
            fallback={
              <DropdownMenuItem onSelect={() => props.onRead(props.ping)}>Mark read</DropdownMenuItem>
            }
          >
            <DropdownMenuItem onSelect={() => props.onMarkUnread(props.ping)}>
              Mark unread
            </DropdownMenuItem>
          </Show>
        </Show>
        <DropdownMenuItem onSelect={() => props.onJumpToChat(props.ping)}>
          View in chat
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );

  // --- Dense row: the resting FYI/read state. One glanceable line; tap the
  // description to promote it to a card. ---
  const Row = () => (
    <div
      ref={(el: HTMLElement) => (cardEl = el)}
      class="group/pingrow relative flex min-h-11 items-center gap-2 border-b border-border/60 px-3 transition-opacity"
      classList={{ "opacity-60": isDone() }}
      data-read={read() ? "true" : "false"}
      data-status={props.ping.status}
      data-ping-id={props.ping.id}
      data-ping-name={props.ping.name}
      {...swipeProps()}
    >
      <button
        type="button"
        class="flex min-w-0 flex-1 items-center gap-1.5 py-2.5 text-left"
        aria-label={`Open Ping ${props.ping.name}`}
        onClick={() => props.onSelect?.(props.ping)}
      >
        <Show when={unread()}>
          <span
            class="size-2 shrink-0 rounded-full bg-primary"
            aria-label="Unread"
            data-slot="unread-dot"
          />
        </Show>
        <span
          class="min-w-0 flex-1 truncate text-sm leading-tight"
          classList={{
            "font-medium text-foreground": unread(),
            "text-muted-foreground": !unread(),
          }}
          data-slot="ping-title"
        >
          {titleText()}
        </span>
      </button>
      <div class="flex shrink-0 items-center gap-1.5">
        <HandleChip />
        <Time />
        <MarkDoneButton />
        <MoreMenu />
      </div>
    </div>
  );

  const CardView = () => (
    <Card
      ref={(el: HTMLElement) => (cardEl = el)}
      size="sm"
      class="mx-3 gap-2 border-l-2 px-3 py-3 transition-opacity"
      classList={{
        "border-l-primary": props.ping.requires_response,
        "border-l-transparent": !props.ping.requires_response,
        "opacity-60": isDone(),
      }}
      data-read={read() ? "true" : "false"}
      data-status={props.ping.status}
      data-ping-id={props.ping.id}
      data-ping-name={props.ping.name}
      {...swipeProps()}
    >
      {/* Header: the human description leads as the title (Inter medium); the
          machine @handle is a mono chip beside the timestamp. The unread dot
          leads; Done tag + one-tap Mark done + ⋯ cluster to the right. */}
      <div class="flex items-start justify-between gap-2">
        <div class="flex min-w-0 flex-1 items-start gap-1.5">
          <Show when={unread()}>
            <span
              class="mt-1 size-2 shrink-0 rounded-full bg-primary"
              aria-label="Unread"
              data-slot="unread-dot"
            />
          </Show>
          <span
            class="min-w-0 flex-1 text-sm font-medium leading-snug"
            classList={{
              "text-foreground": unread(),
              "text-muted-foreground": !unread(),
            }}
            data-slot="ping-title"
            title={titleText()}
          >
            {titleText()}
          </span>
        </div>
        <div class="flex shrink-0 items-center gap-1.5">
          <HandleChip />
          <Time />
          <Show when={isDone()}>
            <span class="inline-flex items-center gap-1 text-[0.68rem] uppercase tracking-[0.03em] text-muted-foreground">
              <Check class="size-3 text-status-success" aria-hidden="true" />
              Done
            </span>
          </Show>
          <MarkDoneButton />
          <MoreMenu />
        </div>
      </div>

      {/* Body: the Ping content. Full-strength when unread (the "bold email"
          look), dimmed once read/dealt-with. Suppressed when it would merely
          echo the title (a description-less Ping already surfaced its body as
          the title). */}
      <Show when={showBody()}>
        <div
          classList={{
            "font-medium text-foreground": unread(),
            "text-muted-foreground": !unread(),
          }}
        >
          <Markdown>{props.ping.content}</Markdown>
        </div>
      </Show>

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
          quickReplies={props.ping.quick_replies}
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
          for a Ping with a live side chat, "in progress · resume") stays
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
                onClick={() => props.onDiscuss(props.ping)}
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
              onClick={() => props.onDiscuss(props.ping)}
            >
              <GitFork class="size-3.5" aria-hidden="true" />
              in progress · resume
            </Button>
          </Show>
        </div>
      </Show>
    </Card>
  );

  return (
    <Show when={asCard()} fallback={<Row />}>
      <CardView />
    </Show>
  );
}
