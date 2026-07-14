import { createEffect, createSignal, onCleanup, onMount, Show } from "solid-js";
import { BrandMark } from "./components/BrandMark";
import { ChatView } from "./components/chat/ChatView";
import { DesktopShell } from "./components/DesktopShell";
import { EventScroller } from "./components/eventq/EventScroller";
import { CommandPalette, ShortcutHelp } from "./components/CommandPalette";
import { ConnectionPill } from "./components/ConnectionPill";
import { PhoneHome } from "./components/PhoneHome";
import { PhoneNavBar } from "./components/PhoneNavBar";
import { PhoneOverflowMenu } from "./components/PhoneOverflowMenu";
import { Toaster } from "./components/Toaster";
import { TokenGate } from "./components/TokenGate";
import { resolveWsUrl } from "./lib/endpoint";
import { createMediaFlag } from "./lib/focus";
import {
  commandPaletteOpen,
  installGlobalKeymap,
  setCommandPaletteOpen,
  setShortcutHelpOpen,
  shortcutHelpOpen,
} from "./lib/keymap";
import { titleBadgeEnabled } from "./lib/prefs";
import {
  eventTitle,
  isOpenJudgment,
  openJudgmentCount,
  visibleEvents,
} from "./store/selectors";
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

  // The desktop-unified breakpoint. Below it, the phone shell (a single column
  // that switches home between Feed and Chat) is byte-identical to what shipped;
  // at/above it, the whole surface becomes the DesktopShell — Feed + Chat side by
  // side, `state.home` unused. A JS flag (not a pure-CSS reflow) because the two
  // shells are structurally different trees, and only one may mount at a time so
  // Chat/Feed never double-mount their state. `createMediaFlag` reads matchMedia
  // synchronously, so first paint is already correct in this client-rendered PWA.
  const atRail = createMediaFlag("(min-width: 1100px)");

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

  // The "needs you" count is the SINGLE truth the attention layer reads: open,
  // undecided judgments over the resting (non-archived) queue — the same count
  // the Feed header shows as its one red. Wave 1 rewires the title badge, the
  // favicon dot, and desktop notifications onto THIS (they read the
  // never-populated `state.pings` before — a live bug).
  const needsYouCount = () =>
    openJudgmentCount(
      visibleEvents(state.events, state.eventArchiveOverrides),
      state.eventDecideOverrides,
    );

  // Reflect the needs-you count in document.title, so it's visible from a
  // backgrounded tab without push. (Replaces the React useTitleBadge hook.)
  createEffect(() => {
    const count = needsYouCount();
    document.title =
      titleBadgeEnabled() && count > 0 ? `(${count}) ${BASE_TITLE}` : BASE_TITLE;
  });

  // Swap the tab favicon to the dotted variant while anything needs you, so a
  // backgrounded tab reads "attend to this" at a glance — one calm indigo dot on
  // the cube mark, never a red count. Reverts to the plain mark at zero.
  createEffect(() => {
    const dotted = needsYouCount() > 0;
    const link = document.querySelector<HTMLLinkElement>('link[rel="icon"]');
    if (link) link.href = dotted ? "/favicon-dot.svg" : "/favicon.svg";
  });

  // Optional desktop notification for a NEW blocking judgment while the tab is
  // hidden — but ONLY when the Owner has already granted permission from the
  // quiet Settings row (never a permission prompt on load, per the "no
  // notification slot machine" rule). Primed on first run so the initial
  // snapshot never notifies for pre-existing work; one silent notification per
  // freshly-arrived blocking judgment.
  let knownBlockingIds: Set<number> | null = null;
  createEffect(() => {
    const blocking = visibleEvents(state.events, state.eventArchiveOverrides).filter(
      (e) => isOpenJudgment(e, state.eventDecideOverrides) && e.blocking,
    );
    const ids = new Set(blocking.map((e) => e.id));
    if (knownBlockingIds === null) {
      knownBlockingIds = ids;
      return;
    }
    const fresh = blocking.filter((e) => !knownBlockingIds!.has(e.id));
    knownBlockingIds = ids;
    if (
      fresh.length === 0 ||
      typeof Notification === "undefined" ||
      Notification.permission !== "granted" ||
      document.visibilityState !== "hidden"
    ) {
      return;
    }
    const newest = fresh[fresh.length - 1];
    try {
      const note = new Notification("hirsel — needs you", {
        body: eventTitle(newest),
        tag: `hirsel-judgment-${newest.id}`,
        silent: true,
      });
      // Clicking the notification brings the tab forward — the one useful action.
      note.onclick = () => {
        try {
          window.focus();
        } catch {
          /* best-effort */
        }
      };
    } catch {
      /* best-effort; a denied/unsupported environment just stays quiet */
    }
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
      {/* The app frame. Mobile-first: a phone-width single column by default; at
          `rail` it becomes the desktop-unified row (icon rail ∣ Feed ∣ Chat ∣
          right region) and fills to a ~1600px cap, centred. The max-width classes
          stay pure CSS so first paint is correct:
            • `rail` (≥1100px): the frame is a row filled to a cap — the width is
              used by real structure (Feed + Chat + inspectors), never stretched.
            • `split` (≥900px) while a Side Chat is open: the fork-ui two-pane
              width (~980px) for the 900–1099 band, where the phone shell still
              rules but Chat's own split opens.
          Below `split` nothing changes — the phone column is the base. The tree
          INSIDE the frame is chosen by `atRail()`: the desktop shell mounts BOTH
          Feed and Chat; the phone shell mounts one home at a time. */}
      <div
        data-slot="app-frame"
        class="relative mx-auto flex w-full min-h-0 flex-1 flex-col rail:flex-row duration-200 ease-out motion-safe:transition-[max-width]"
        classList={{
          "max-w-[560px] rail:max-w-[1600px]": !splitActive(),
          "max-w-[560px] split:max-w-[980px] rail:max-w-[1600px]": splitActive(),
        }}
      >
        <Show
          when={atRail()}
          fallback={
            // The phone shell — byte-identical to what shipped: a single column
            // that switches home between the Feed scroller and Chat, with the
            // phone header (brand + full agent status + overflow) and the bottom
            // two-way Feed/Chat nav bar.
            <div class="flex min-h-0 min-w-0 flex-1 flex-col">
              {/* Phone header (§4 IA cleanup). Just the wordmark and the right
                  controls now: the agent's live work is announced once, by the
                  inline transcript marker on the Chat surface — never as standing
                  header furniture, and idle is never announced at all. The right
                  group is a bare connection dot (the ONE exception surface here —
                  it expands to the full pill when reconnecting/offline) plus ONE
                  overflow (Model · Canvas · Processes · Settings). */}
              <header class="flex flex-shrink-0 items-center justify-between gap-2 border-b border-border px-4 py-3 rail:hidden">
                <div class="flex shrink-0 items-center gap-2">
                  <BrandMark size={22} />
                  <h1 class="m-0 text-base font-semibold tracking-[0.01em]">hirsel</h1>
                </div>
                <div class="flex shrink-0 items-center gap-0.5">
                  <ConnectionPill compact />
                  <PhoneOverflowMenu />
                </div>
              </header>
              {/* The phone home is the event-queue scroller (ADR-0012); Chat is
                  the drill-in reached from a judgment's Discuss, the bottom nav,
                  or the overflow. PhoneHome cross-slides between the two on a
                  `state.home` change (Feed↔Chat), mounting only one at a time. */}
              <PhoneHome feed={() => <EventScroller />} chat={() => <ChatView />} />
              {/* Phone bottom navigation bar: two icon buttons (Feed / Chat) that
                  move the Owner BOTH ways, so he can always get back. Shown on
                  BOTH homes; on chat it sits below the composer. */}
              <PhoneNavBar />
            </div>
          }
        >
          {/* Desktop-unified: Feed + Chat side by side, one workspace. */}
          <DesktopShell />
        </Show>
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
