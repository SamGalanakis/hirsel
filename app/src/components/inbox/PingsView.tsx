import { ChevronDown, ChevronRight, Inbox as InboxIcon } from "lucide-solid";
import { createMemo, createSignal, For, Show } from "solid-js";
import type { Ping } from "../../protocol";
import { dispatch, goToChat, requestSideChatOpen, state } from "../../store/store";
import { isPingResolved } from "../../store/selectors";
import { markDoneWithUndo } from "../../lib/resolve-undo";
import { getClient } from "../../ws/client";
import { Empty, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle } from "../ui/empty";
import { PingCard } from "./PingCard";

const DONE_LIMIT = 20;

/** Content of the expanded Tray: the open Pings (this component's sole caller
 * is now `TrayOverlay` / `PingsRail`) plus the collapsed Done section (ADR-0009:
 * was "Deleted").
 *
 * Triage layout (v2.2): open Pings split into a **Needs reply** priority
 * section — the active `requires_response` items, each promoted to a full card
 * with its reply affordance — and the rest, which rest as dense one-line rows
 * that expand to a card when tapped (single-selection accordion). This keeps the
 * rail glanceable: only the item that wants you (or the one you're looking at)
 * carries card weight. */
export function PingsView() {
  const [doneExpanded, setDoneExpanded] = createSignal(false);
  // The one FYI/read row currently promoted to a card (tap to expand, tap again
  // to collapse). requires_response items are always cards and never selected.
  const [selectedId, setSelectedId] = createSignal<number | null>(null);

  const partitioned = createMemo(() => {
    const sorted = [...state.pings].sort((a, b) => b.id - a.id);
    // Fold in the optimistic Mark-done override so a just-resolved Ping moves to
    // Done at once (and a mid-window Undo brings it back) — spec item 2.
    const open = sorted.filter((p) => !isPingResolved(p, state.resolveOverrides));
    // Tray (v1.6): requires_response Pings float to the top as their own
    // "Needs reply" section — a triage affordance a chronological-only feed
    // lacks, per the design critique's [P1] fix. Each bucket stays newest-first
    // internally so arrival order is still legible within a bucket.
    return {
      needsReply: open.filter((p) => p.requires_response),
      other: open.filter((p) => !p.requires_response),
      // ADR-0009: resolved Pings (done, or legacy archived, or optimistically
      // Marked done) collect here.
      done: sorted
        .filter((p) => isPingResolved(p, state.resolveOverrides))
        .slice(0, DONE_LIMIT),
    };
  });

  // Answer in place: reuse the same send path an owner Composer send uses
  // (client_id, optimistic reconcile, offline queue). No tab switch, no
  // navigation — the card reconciles the reply inline and stays in the Tray.
  function handleSendReply(ping: Ping, body: string) {
    getClient()?.sendMessage(body, ping.anchor);
  }

  // Secondary affordance: jump to the Ping's Anchor in Chat.
  function handleJumpToChat(ping: Ping) {
    goToChat({ scrollToMessageId: ping.anchor });
  }

  // Mark done (ADR-0009: the Ping's terminal `resolve_ping` state, presented as
  // "Done"). Routed through the Undo controller (spec item 2): the card flips to
  // Done at once, but the wire send is debounced behind a 5s "Undo" toast so a
  // mis-tap/mis-swipe is recoverable before it commits (resolve is terminal on
  // the host — there is no reopen op).
  function handleResolve(ping: Ping) {
    markDoneWithUndo(ping.id);
  }

  // Auto-read / "Mark read": optimistic read flip + read_ping on the wire.
  function handleRead(ping: Ping) {
    getClient()?.readPing(ping.id);
  }

  // "Mark unread": client-only override — there is no wire unread op.
  function handleMarkUnread(ping: Ping) {
    dispatch({ type: "mark_unread_local", pingId: ping.id });
  }

  // v2.0 (ADR-0008): "Discuss" (fresh) / "resume" (a live one already exists)
  // — open_side_chat is idempotent per Ping on the host either way. Recording
  // the pending ping id here is what lets ChatView's effect open the sheet
  // the moment its `sc` (fresh) or transcript (resume) is ready, without the
  // Tray/PingsView needing to know about the sheet at all.
  function handleDiscuss(ping: Ping) {
    getClient()?.openSideChat(ping.id);
    requestSideChatOpen(ping.id);
  }

  // Row tap: promote to a card (or collapse it back). Single selection.
  function toggleSelect(ping: Ping) {
    setSelectedId((cur) => (cur === ping.id ? null : ping.id));
  }

  const cardProps = (ping: Ping) => ({
    ping,
    onSendReply: handleSendReply,
    onJumpToChat: handleJumpToChat,
    onResolve: handleResolve,
    onRead: handleRead,
    onMarkUnread: handleMarkUnread,
    onDiscuss: handleDiscuss,
  });

  return (
    <Show
      when={
        partitioned().needsReply.length > 0 ||
        partitioned().other.length > 0 ||
        partitioned().done.length > 0
      }
      fallback={
        <div class="flex flex-1 flex-col p-3">
          <Empty class="border-none">
            <EmptyHeader>
              <EmptyMedia variant="icon">
                <InboxIcon />
              </EmptyMedia>
              <EmptyTitle>No Pings yet</EmptyTitle>
              <EmptyDescription>Nothing needs your attention right now.</EmptyDescription>
            </EmptyHeader>
          </Empty>
        </div>
      }
    >
      <div class="thin-scrollbar flex flex-1 flex-col gap-4 overflow-y-auto py-3 pb-6">
        {/* Needs reply: the active requires_response items, expanded as cards. */}
        <Show when={partitioned().needsReply.length > 0}>
          <section class="flex flex-col gap-3">
            <h2 class="mx-3 text-[0.68rem] font-semibold uppercase tracking-[0.06em] text-muted-foreground">
              Needs reply ({partitioned().needsReply.length})
            </h2>
            <For each={partitioned().needsReply}>
              {(ping) => <PingCard {...cardProps(ping)} expanded={true} />}
            </For>
          </section>
        </Show>

        {/* Everything else rests as dense hairline rows; tap to promote one.
            The open bucket is ALWAYS labelled whenever another section is
            visible (spec item 5), so a lone open Ping never floats UNLABELED
            above a prominent "Done" accordion — the eye lands on open work, not
            on what's done. Label "Other" when "Needs reply" leads the column,
            else "Open" (it is the only open bucket). */}
        <Show when={partitioned().other.length > 0}>
          <section class="flex flex-col">
            <Show
              when={partitioned().needsReply.length > 0 || partitioned().done.length > 0}
            >
              <h2 class="mx-3 mb-1 text-[0.68rem] font-semibold uppercase tracking-[0.06em] text-muted-foreground">
                {partitioned().needsReply.length > 0 ? "Other" : "Open"} ({partitioned().other.length})
              </h2>
            </Show>
            <For each={partitioned().other}>
              {(ping) => (
                <PingCard
                  {...cardProps(ping)}
                  expanded={selectedId() === ping.id}
                  onSelect={toggleSelect}
                />
              )}
            </For>
          </section>
        </Show>

        {/* Done is demoted and foot-anchored (spec item 5): `mt-auto` pushes it
            to the bottom of the column so open work leads, and it rests smaller
            and quieter (text-xs, size-3.5 chevron), collapsed by default. */}
        <Show when={partitioned().done.length > 0}>
          <div class="mt-auto">
            <button
              type="button"
              class="mx-3 flex w-[calc(100%-1.5rem)] items-center gap-1 border-t border-border py-2.5 text-left text-xs text-muted-foreground transition-colors hover:text-foreground"
              aria-expanded={doneExpanded()}
              onClick={() => setDoneExpanded((v) => !v)}
            >
              <Show when={doneExpanded()} fallback={<ChevronRight class="size-3.5" />}>
                <ChevronDown class="size-3.5" />
              </Show>
              Done ({partitioned().done.length})
            </button>
            <Show when={doneExpanded()}>
              <div class="flex flex-col gap-3">
                <For each={partitioned().done}>
                  {(ping) => <PingCard {...cardProps(ping)} expanded={true} />}
                </For>
              </div>
            </Show>
          </div>
        </Show>
      </div>
    </Show>
  );
}
