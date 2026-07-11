import { createEffect, createSignal, onCleanup, Show } from "solid-js";
import { ChatView } from "./components/chat/ChatView";
import { ConnectionPill } from "./components/ConnectionPill";
import { NavRail } from "./components/NavRail";
import { ProcessesButton } from "./components/processes/ProcessesButton";
import { Toaster } from "./components/Toaster";
import { TokenGate } from "./components/TokenGate";
import { resolveWsUrl } from "./lib/endpoint";
import { openUnreadCount } from "./store/selectors";
import { state } from "./store/store";
import { getStoredToken, setStoredToken, startClient } from "./ws/client";

const WS_URL = resolveWsUrl();

const BASE_TITLE = "hirsel";

function App() {
  const [token, setToken] = createSignal<string | null>(getStoredToken());

  // Whether a Side Chat is open — widens the shell for the desktop split.
  const splitActive = () => state.activeSideChatSc !== null;

  // Open (and tear down) the single WebSocket connection whenever the token is
  // set. Components run once in Solid; this effect re-runs only when token()
  // changes (first-run gate submit).
  createEffect(() => {
    const t = token();
    if (!t) return;
    const client = startClient(WS_URL, t);
    onCleanup(() => client.close());
  });

  // Reflect the Inbox's email-like unread count (open + unread) in
  // document.title, so it's visible from a backgrounded tab without push
  // notifications. (Replaces the React useTitleBadge hook with a plain effect.)
  createEffect(() => {
    const count = openUnreadCount(state.pings, state.unreadOverrides);
    document.title = count > 0 ? `(${count}) ${BASE_TITLE}` : BASE_TITLE;
  });

  return (
    <Show
      when={token()}
      fallback={
        <div class="mx-auto flex w-full max-w-[560px] flex-1 flex-col">
          <TokenGate
            onSubmit={(t) => {
              setStoredToken(t);
              setToken(t);
            }}
          />
        </div>
      }
    >
      {/* The desktop shell frame (desktop-shell pass). Mobile-first: a
          phone-width single column by default; at `rail` it becomes the
          persistent 3-pane row (nav rail ∣ chat ∣ context). Two width overrides
          layer on top, both pure CSS so first paint is correct and no width
          signal threads through the store:
            • `rail` (≥1100px): the frame becomes a row and fills to a cap
              (~1600px), centered — the width is used by real structure (nav +
              chat + context), never stretched to glass.
            • `split` (≥900px) while a Side Chat is open: the fork-ui two-pane
              width (~980px), for the 900–1099 band where the rail has no room.
          Below `split` nothing changes — the phone column is the fallback. */}
      <div
        data-slot="app-frame"
        class="relative mx-auto flex w-full min-h-0 flex-1 flex-col rail:flex-row duration-200 ease-out motion-safe:transition-[max-width]"
        classList={{
          "max-w-[560px] rail:max-w-[1600px]": !splitActive(),
          "max-w-[560px] split:max-w-[980px] rail:max-w-[1600px]": splitActive(),
        }}
      >
        {/* Desktop nav rail — hidden below `rail`, a flex column at/above it. */}
        <NavRail />
        {/* Main column: the phone header (desktop moves brand + nav +
            connection into the NavRail, so the header is `rail:hidden`) above
            ChatView, which owns the chat measure + the shared right context
            region (Pings rail / Side Chat / Processes + Settings inspectors). */}
        <div class="flex min-h-0 min-w-0 flex-1 flex-col">
          <header class="flex flex-shrink-0 items-center justify-between border-b border-border px-4 py-3 rail:hidden">
            <h1 class="m-0 text-base font-semibold tracking-[0.01em]">hirsel</h1>
            <div class="flex items-center gap-1.5">
              <ProcessesButton />
              <ConnectionPill />
            </div>
          </header>
          {/* Chat is the whole app (spec [P1]). */}
          <main class="flex min-h-0 flex-1 flex-col">
            <ChatView />
          </main>
        </div>
      </div>
      <Toaster />
    </Show>
  );
}

export default App;
