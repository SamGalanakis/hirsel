// The event-fork opener (ADR-0008 + ADR-0012, event forks / v2.4) — the single
// entry point behind every card's "Discuss" / "Ask" door and the "discussion
// open · resume" chip. It fires `open_side_chat {event_id}` (idempotent
// host-side, so fresh-open and resume are one op) and surfaces the fork panel
// (the pinned, still-decidable card over a scoped thread).
//
// Presentation: on desktop the panel lives in the always-mounted ChatView's
// right region, so nothing needs to move. On phone ChatView mounts only on the
// chat home, so the opener drills into the chat shell (`home = chat`) first;
// once the fork's `sc` is known the panel takes over as a full-screen sheet.
//
// A resume already knows the `sc` (the Event carries `fork_sc`), so it surfaces
// the pane immediately; a fresh open has no `sc` yet, so it registers the
// pending open and ChatView's effect opens the pane the moment `side_chat_open`
// lands the `sc`. This mirrors the legacy Discuss/Resume split, keyed on the
// Event instead of a Ping.
import type { EventItem } from "../protocol";
import { goToChatDrillIn, openSideChat, requestSideChatOpen } from "../store/store";
import { getClient } from "../ws/client";

export function openEventFork(event: EventItem): void {
  // Always (re)fetch the scoped transcript + refresh the seed — idempotent, so
  // a resume restores history and a fresh open creates the scope.
  getClient()?.openSideChat(event.id);
  if (event.fork_sc) {
    // Resume: the sc is wire-known (fork_sc). Surface the pane now — this also
    // sets `home = chat` so the phone mounts the panel-hosting ChatView.
    openSideChat(event.fork_sc);
  } else {
    // Fresh open: the sc isn't known until `side_chat_open` answers. Drill into
    // the chat shell (phone) and register the pending open; ChatView opens the
    // pane once the matching ref appears.
    goToChatDrillIn();
    requestSideChatOpen(event.id);
  }
}
