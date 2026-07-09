import { createEffect, createSignal, onCleanup, Show } from "solid-js";
import { ChatView } from "./components/chat/ChatView";
import { ConnectionPill } from "./components/ConnectionPill";
import { ProcessesButton } from "./components/processes/ProcessesButton";
import { ProcessesSheet } from "./components/processes/ProcessesSheet";
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
    const count = openUnreadCount(state.inbox, state.unreadOverrides);
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
      {/* Slack-style split (ADR-0008 fork-ui iteration): the app column is a
          phone-width single column by default. When a Side Chat is open on a
          wide viewport (≥900px, where there is genuinely room for two panes),
          the shell widens so ChatView can lay main Chat + the side panel out
          side-by-side — main stays live on the left, the side panel is the
          right rail. Below 900px the shell stays narrow and the side chat is a
          full-screen sheet (both driven by CSS, one component tree). */}
      <div
        class="mx-auto flex w-full min-h-0 flex-1 flex-col transition-[max-width] duration-200 ease-out"
        classList={{
          "max-w-[560px]": !splitActive(),
          "max-w-[560px] min-[900px]:max-w-[980px]": splitActive(),
        }}
      >
        <header class="flex flex-shrink-0 items-center justify-between border-b border-border px-4 py-3">
          <h1 class="m-0 text-base font-semibold tracking-[0.01em]">hirsel</h1>
          <div class="flex items-center gap-1.5">
            <ProcessesButton />
            <ConnectionPill />
          </div>
        </header>
        {/* Chat is the whole app now (spec [P1]): the bottom TabBar and the
            Inbox tab are gone. Inbox lives in ChatView's Tray; Processes is
            the header icon's full-screen sheet, layered above via Show. */}
        <main class="flex min-h-0 flex-1 flex-col">
          <ChatView />
        </main>
      </div>
      <ProcessesSheet />
      <Toaster />
    </Show>
  );
}

export default App;
