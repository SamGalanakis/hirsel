import { createEffect, createSignal, onCleanup, Show } from "solid-js";
import { ChatView } from "./components/chat/ChatView";
import { ConnectionPill } from "./components/ConnectionPill";
import { InboxView } from "./components/inbox/InboxView";
import { TabBar } from "./components/TabBar";
import { TokenGate } from "./components/TokenGate";
import { openRequiresResponseCount } from "./store/selectors";
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

  // Open (and tear down) the single WebSocket connection whenever the token is
  // set. Components run once in Solid; this effect re-runs only when token()
  // changes (first-run gate submit).
  createEffect(() => {
    const t = token();
    if (!t) return;
    const client = startClient(WS_URL, t);
    onCleanup(() => client.close());
  });

  // Reflect the Inbox's requires-response badge count in document.title, so
  // it's visible from a backgrounded tab without push notifications. (Replaces
  // the React useTitleBadge hook with a plain effect.)
  createEffect(() => {
    const count = openRequiresResponseCount(state.inbox);
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
      <div class="mx-auto flex w-full max-w-[560px] min-h-0 flex-1 flex-col">
        <header class="flex flex-shrink-0 items-center justify-between border-b border-border px-4 py-3">
          <h1 class="m-0 text-base font-semibold tracking-[0.01em]">hirsel</h1>
          <ConnectionPill />
        </header>
        <main class="flex min-h-0 flex-1 flex-col">
          <Show when={state.activeTab === "chat"} fallback={<InboxView />}>
            <ChatView />
          </Show>
        </main>
        <TabBar />
      </div>
    </Show>
  );
}

export default App;
