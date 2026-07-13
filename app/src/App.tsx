import { Settings } from "lucide-solid";
import { createEffect, createSignal, onCleanup, onMount, Show } from "solid-js";
import { AgentStatus } from "./components/chat/AgentStatus";
import { ChatView } from "./components/chat/ChatView";
import { ModelChip } from "./components/chat/ModelChip";
import { CommandPalette, ShortcutHelp } from "./components/CommandPalette";
import { ConnectionPill } from "./components/ConnectionPill";
import { NavRail } from "./components/NavRail";
import { ProcessesButton } from "./components/processes/ProcessesButton";
import { CanvasButton } from "./components/views/CanvasSurface";
import { Toaster } from "./components/Toaster";
import { TokenGate } from "./components/TokenGate";
import { resolveWsUrl } from "./lib/endpoint";
import {
  commandPaletteOpen,
  installGlobalKeymap,
  setCommandPaletteOpen,
  setShortcutHelpOpen,
  shortcutHelpOpen,
} from "./lib/keymap";
import { titleBadgeEnabled } from "./lib/prefs";
import { openUnreadCount } from "./store/selectors";
import { setSettingsOpen, state } from "./store/store";
import { getStoredToken, setStoredToken, startClient } from "./ws/client";

const WS_URL = resolveWsUrl();

const BASE_TITLE = "hirsel";

function App() {
  const [token, setToken] = createSignal<string | null>(getStoredToken());
  // A rejected/expired token surfaces here (C5): the ws client clears the stored
  // token and calls back; we drop to the gate and show this inline error instead
  // of the old "reconnecting…" forever dead-end.
  const [authError, setAuthError] = createSignal<string | null>(null);

  // Whether a Side Chat is open — widens the shell for the desktop split.
  const splitActive = () => state.activeSideChatSc !== null;

  // The global keyboard layer (focus composer, `g`-chord pane switches, jump to
  // latest, ⌘K palette, `?` cheat-sheet). Window-level; it suppresses itself
  // while the Owner is typing or an overlay owns input, so it never fights the
  // composer or a focus-trap.
  onMount(() => {
    const dispose = installGlobalKeymap();
    onCleanup(dispose);
  });

  // Open (and tear down) the single WebSocket connection whenever the token is
  // set. Components run once in Solid; this effect re-runs only when token()
  // changes (first-run gate submit).
  createEffect(() => {
    const t = token();
    if (!t) return;
    const client = startClient(WS_URL, t, {
      onAuthReject: (detail) => {
        // The client already cleared the stored token and stopped reconnecting;
        // clearing the signal swaps back to the gate with the error line.
        setToken(null);
        setAuthError(detail);
      },
    });
    onCleanup(() => client.close());
  });

  // Reflect the Inbox's email-like unread count (open + unread) in
  // document.title, so it's visible from a backgrounded tab without push
  // notifications. (Replaces the React useTitleBadge hook with a plain effect.)
  createEffect(() => {
    const count = openUnreadCount(state.pings, state.unreadOverrides);
    document.title =
      titleBadgeEnabled() && count > 0 ? `(${count}) ${BASE_TITLE}` : BASE_TITLE;
  });

  return (
    <Show
      when={token()}
      fallback={
        <div class="mx-auto flex w-full max-w-[560px] flex-1 flex-col">
          <TokenGate
            error={authError()}
            onSubmit={(t) => {
              setAuthError(null);
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
          <header class="flex flex-shrink-0 items-center justify-between gap-3 border-b border-border px-4 py-3 rail:hidden">
            {/* The wordmark paired with the live agent-status indicator, so the
                north-star "what is my agent doing" is answerable at rest on the
                phone's glance-first surface (previously the header showed only a
                static wordmark). AgentStatus is the shared extracted component
                (dot + idle/thinking/tool-name label). */}
            <div class="flex min-w-0 items-center gap-2.5">
              <h1 class="m-0 shrink-0 text-base font-semibold tracking-[0.01em]">hirsel</h1>
              <span class="h-4 w-px shrink-0 bg-border" aria-hidden="true" />
              <AgentStatus />
            </div>
            <div class="flex min-w-0 shrink items-center gap-1.5">
              {/* Quick main-model variant switch; collapses to just the variant
                  on narrow phones (its label hides < 400px). Full model +
                  sub-agent config lives in Settings. */}
              <ModelChip />
              <CanvasButton />
              <ProcessesButton />
              {/* Settings is otherwise reachable only from the desktop NavRail
                  gear, which is gone below `rail`; this header entry is the sole
                  phone path to theme, Forget token, diagnostics, and the endpoint
                  (C3). The whole header is `rail:hidden`, so it doesn't double up
                  with the NavRail item at desktop widths. */}
              <button
                type="button"
                class="flex items-center rounded-full p-1.5 text-muted-foreground transition-colors hover:text-foreground"
                onClick={() => setSettingsOpen(true)}
                aria-label="Settings"
              >
                <Settings class="size-5" aria-hidden="true" />
              </button>
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
      {/* Summoned surfaces — no standing chrome. Opened from the keymap (⌘K /
          `?`) and the NavRail palette affordance. */}
      <CommandPalette open={commandPaletteOpen()} onOpenChange={setCommandPaletteOpen} />
      <ShortcutHelp open={shortcutHelpOpen()} onOpenChange={setShortcutHelpOpen} />
    </Show>
  );
}

export default App;
