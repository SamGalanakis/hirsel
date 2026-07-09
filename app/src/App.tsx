import { createEffect, createSignal, onCleanup, Show } from "solid-js";
import { ChatView } from "./components/chat/ChatView";
import { ConnectionPill } from "./components/ConnectionPill";
import { PingsRestoreButton } from "./components/inbox/Tray";
import { ProcessesButton } from "./components/processes/ProcessesButton";
import { Toaster } from "./components/Toaster";
import { TokenGate } from "./components/TokenGate";
import { openUnreadCount } from "./store/selectors";
import { state } from "./store/store";
import { getStoredToken, setStoredToken, startClient } from "./ws/client";

// VITE_WS_URL always wins. Otherwise: in dev, default to the mock server's
// port; in production the Hirsel Host serves this app from the same origin
// with its WS endpoint at /ws, so default to same-origin.
const WS_URL =
  import.meta.env.VITE_WS_URL ??
  (import.meta.env.DEV
    ? `ws://${window.location.hostname}:8787`
    : `${window.location.protocol === "https:" ? "wss://" : "ws://"}${window.location.host}/ws`);

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
          phone-width single column by default. Two width overrides layer on top,
          both pure CSS so first paint is correct and no width signal threads
          through the store:
            • `rail` (≥1100px): the frame fills to a cap (~1360px), centered,
              so ChatView can stand a Pings rail beside the chat measure — the
              empty-desktop void is filled to a cap, never stretched to glass.
            • `split` (≥900px) while a Side Chat is open: the fork-ui two-pane
              width (~980px), for the 900–1099 band where the rail has no room.
          Below `split` nothing changes — the phone column is the fallback. */}
      <div
        data-slot="app-frame"
        class="relative mx-auto flex w-full min-h-0 flex-1 flex-col duration-200 ease-out motion-safe:transition-[max-width]"
        classList={{
          "max-w-[560px] rail:max-w-[1360px]": !splitActive(),
          "max-w-[560px] split:max-w-[980px] rail:max-w-[1360px]": splitActive(),
        }}
      >
        <header class="flex flex-shrink-0 items-center justify-between border-b border-border px-4 py-3">
          <h1 class="m-0 text-base font-semibold tracking-[0.01em]">hirsel</h1>
          <div class="flex items-center gap-1.5">
            {/* Precedence affordance: while a Side Chat holds the right region,
                bring the Pings rail back (rail width only). */}
            <PingsRestoreButton />
            <ProcessesButton />
            <ConnectionPill />
          </div>
        </header>
        {/* Chat is the whole app (spec [P1]). ChatView owns the two-zone
            desktop layout: chat measure on the left, the shared right region
            (Pings rail / Side Chat / Processes inspector) on the right. */}
        <main class="flex min-h-0 flex-1 flex-col">
          <ChatView />
        </main>
      </div>
      <Toaster />
    </Show>
  );
}

export default App;
