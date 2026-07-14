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
// The four zones, left to right:
//   [slim IconRail] · <main>[FeedColumn] · [ChatView: chat pane · right region]</main>
// The rail stays outside the primary-content landmark; the remaining zones share
// a flexing `<main>` so the desktop shell has exactly one main landmark without
// changing their visual composition.
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
      <main class="flex min-w-0 flex-1">
        <FeedColumn />
        <ChatView />
      </main>
    </>
  );
}
