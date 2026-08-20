import { createEffect, createSignal, onCleanup, onMount, Show } from "solid-js";
import { CommandPalette, ShortcutHelp } from "./components/CommandPalette";
import { TaskShell } from "./components/tasks/TaskShell";
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
import { startPlugins } from "./plugins/loader";
import {
  eventTitle,
  isOpenJudgment,
  tasksNeedingOwnerCount,
  visibleEvents,
} from "./store/selectors";
import { effectiveEvents, state } from "./store/store";
import { getStoredToken, setStoredToken, startClient } from "./ws/client";

const WS_URL = resolveWsUrl();

const BASE_TITLE = "hirsel";

function App() {
  const [token, setToken] = createSignal<string | null>(getStoredToken());
  // A rejected/expired token surfaces here (C5): the ws client clears the stored
  // token and calls back; we drop to the gate and show this inline error instead
  // of the old "reconnecting…" forever dead-end.
  const [authError, setAuthError] = createSignal<string | null>(null);

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

  // Plugin tier: load browser bundles once the socket has actually
  // authenticated. The boot manifest and every plugin RPC use the same owner
  // token, so loading before `hello_ok` would just race a 401; `startPlugins`
  // latches, so a later reconnect never mounts a plugin's components twice.
  createEffect(() => {
    if (state.connection === "connected") startPlugins();
  });

  // The "needs you" count is the SINGLE truth the attention layer reads: open,
  // undecided judgments over the resting (non-archived) queue — the same count
  // the task header shows as its one red. The title badge, the
  // favicon dot, and desktop notifications onto THIS (they read the
  // superseded legacy state before — a live bug).
  const needsYouCount = () =>
    tasksNeedingOwnerCount(visibleEvents(effectiveEvents()));

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
    const blocking = visibleEvents(effectiveEvents()).filter(
      (e) => isOpenJudgment(e) && e.blocking,
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
      {/* Task Margins: one responsive shell. Opening a task changes the subject
          and generated UI; the standing composer stays connected to global
          Hirsel and scopes through a removable task chip. */}
      <TaskShell />
      <Toaster />
      {/* Summoned surfaces — no standing chrome. Opened from the keymap (⌘K /
          `?`) and command-palette affordances. */}
      <CommandPalette open={commandPaletteOpen()} onOpenChange={setCommandPaletteOpen} />
      <ShortcutHelp open={shortcutHelpOpen()} onOpenChange={setShortcutHelpOpen} />
    </Show>
  );
}

export default App;
