import { createEffect, createSignal, onCleanup, onMount, Show } from "solid-js";
import { AgentStatus } from "./components/chat/AgentStatus";
import { ChatView } from "./components/chat/ChatView";
import { EventScroller } from "./components/eventq/EventScroller";
import { CommandPalette, ShortcutHelp } from "./components/CommandPalette";
import { ConnectionPill } from "./components/ConnectionPill";
import { NavRail } from "./components/NavRail";
import { PhoneNavBar } from "./components/PhoneNavBar";
import { PhoneOverflowMenu } from "./components/PhoneOverflowMenu";
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
import { state } from "./store/store";
import { getStoredToken, setStoredToken, startClient } from "./ws/client";

const WS_URL = resolveWsUrl();

const BASE_TITLE = "hirsel";

function App() {
  const [token, setToken] = createSignal<string | null>(getStoredToken());
  // A rejected/expired token surfaces here (C5): the ws client clears the stored
  // token and calls back; we drop to the gate and show this inline error instead
  // of the old "reconnecting…" forever dead-end.
  const [authError, setAuthError] = createSignal<string | null>(null);

  // Whether the Side Chat pane owns the right region — widens the shell for the
  // desktop split. Keys on the region (the pane SELECTION), not on
  // `activeSideChatSc` (the data), so leaving the side chat narrows the shell
  // back even though the side chat stays alive/resumable underneath.
  const splitActive = () => state.rightRegion === "sideChat";

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
    const count = openUnreadCount(state.pings, state.unreadOverrides, state.resolveOverrides);
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
          {/* Phone header (§4 IA cleanup). The north-star — brand + FULL agent
              status — gets priority width (the left group is `flex-1 min-w-0`,
              AgentStatus never truncates to "Agen…"); the right group is a bare
              connection dot (expands to the full pill only when reconnecting/
              offline) plus ONE overflow (Model · Canvas · Processes · Settings),
              so the header holds ≤4 chunks. The whole header is `rail:hidden`, so
              it doesn't double up with the NavRail at desktop widths. */}
          <header class="flex flex-shrink-0 items-center justify-between gap-2 border-b border-border px-4 py-3 rail:hidden">
            <div class="flex min-w-0 flex-1 items-center gap-2.5">
              <h1 class="m-0 shrink-0 text-base font-semibold tracking-[0.01em]">hirsel</h1>
              <span class="h-4 w-px shrink-0 bg-border" aria-hidden="true" />
              <AgentStatus />
            </div>
            <div class="flex shrink-0 items-center gap-0.5">
              <ConnectionPill compact />
              <PhoneOverflowMenu />
            </div>
          </header>
          {/* The home is the event-queue scroller (ADR-0012); Chat is the
              drill-in shell reached from a judgment's Discuss, the NavRail, or
              the phone overflow. The scroller is the phone home and the desktop
              center measure; the chat shell owns the 3-pane layout + the right
              region when drilled into. */}
          <main class="flex min-h-0 flex-1 flex-col">
            <Show when={state.home === "chat"} fallback={<EventScroller />}>
              <ChatView />
            </Show>
          </main>
          {/* Phone bottom navigation bar: two icon buttons (Feed / Chat) that
              move the Owner BOTH ways between the queue and Chat, so he can
              always get back. Phone only (`rail:hidden` — desktop has the
              NavRail) and shown on BOTH homes; on chat it sits below the
              composer (standard tab-bar placement). It is an in-flow sibling
              below the surface, so the scroller/chat height math already
              accounts for it. */}
          <PhoneNavBar />
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
