import { ChatView } from "./chat/ChatView";
import { FeedColumn } from "./eventq/FeedColumn";
import { IconRail } from "./IconRail";

// The desktop-unified workspace (`rail`, ≥1100px). The owner's diagnosis: the
// pre-unified desktop made Queue and Chat DESTINATIONS you switched between (nav
// rows / `state.home`), so you saw the queue (nav rail + index = two sidebars)
// OR chat, never both — wrong on a wide screen. This shell makes desktop an
// EXPANDED view of mobile: Feed AND Chat stand side by side, decided and typed
// into without ever "navigating". `state.home` is a PHONE concept and is not read
// here — both panes are always mounted.
//
// The four zones, left to right (a Fragment so they are the direct flex children
// of App's capped, centred `app-frame` row):
//   [slim IconRail] · [FeedColumn] · [ChatView: chat pane · right region]
// ChatView is reused verbatim — its `rail:` layout already IS the chat pane plus
// the shared right region (Side Chat / Canvas / Processes / Settings), which now
// materialises only when a pane is active (the standing Pings rail is retired:
// the Feed is the needs-you surface now, so a second needs-you list beside it
// would just be the redundancy the owner complained about). The Feed owns the
// surface's one red; the composer is always mounted for Discuss to type into.
export function DesktopShell() {
  return (
    <>
      <IconRail />
      <FeedColumn />
      <ChatView />
    </>
  );
}
