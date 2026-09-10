import { createEffect, createSignal, onSettled, Show } from "solid-js";

import { CommandPalette, ShortcutHelp } from "./components/CommandPalette";
import { ThreadShell } from "./threads/ThreadShell";
import { threadState } from "./threads/store";
import { Toaster } from "./components/Toaster";
import { TokenGate } from "./components/TokenGate";
import { resolveWsUrl } from "./lib/endpoint";
import {
  commandPaletteOpen,
  commandPaletteIntent,
  installGlobalKeymap,
  setCommandPaletteOpen,
  setShortcutHelpOpen,
  shortcutHelpOpen,
} from "./lib/keymap";
import { titleBadgeEnabled } from "./lib/prefs";
import { startPlugins } from "./plugins/loader";
import { state } from "./store/store";
import { getStoredToken, setStoredToken, startClient } from "./ws/client";

const WS_URL = resolveWsUrl();

const BASE_TITLE = "hirsel";

/** Dev convenience: loopback hosts run with the justfile's default token, so
 * first run skips the gate. Stored like a submitted token (HTTP blob fetches
 * read it too); a host with a real token rejects it and drops to the gate. */
function initialToken(): string | null {
  const stored = getStoredToken();
  if (stored !== null) return stored;
  const loopback = ["127.0.0.1", "localhost", "[::1]"].includes(location.hostname);
  if (!import.meta.env.DEV && !loopback) return null;
  setStoredToken("dev-token");
  return "dev-token";
}

function App() {
  const [token, setToken] = createSignal<string | null>(initialToken());
  // A rejected/expired token surfaces here (C5): the ws client clears the stored
  // token and calls back; we drop to the gate and show this inline error instead
  // of the old "reconnecting…" forever dead-end.
  const [authError, setAuthError] = createSignal<string | null>(null);

  // The global keyboard layer (focus composer, `g`-chord pane switches, jump to
  // latest, ⌘K palette, `?` cheat-sheet). Window-level; it suppresses itself
  // while the Owner is typing or an overlay owns input, so it never fights the
  // composer or a focus-trap.
  onSettled(() => {
    const dispose = installGlobalKeymap();
    return dispose;
  });

  // Open (and tear down) the single WebSocket connection whenever the token is
  // set. Components run once in Solid; this effect re-runs only when token()
  // changes (first-run gate submit).
  createEffect(token, (t) => {
    if (!t) return;
    const client = startClient(WS_URL, t, {
      onAuthReject: (detail) => {
        // The client already cleared the stored token and stopped reconnecting;
        // clearing the signal swaps back to the gate with the error line.
        setToken(null);
        setAuthError(detail);
      },
    });
    return () => client.close();
  });

  // Plugin tier: load browser bundles once the socket has actually
  // authenticated. The boot manifest and every plugin RPC use the same owner
  // token, so loading before `hello_ok` would just race a 401; `startPlugins`
  // latches, so a later reconnect never mounts a plugin's components twice.
  createEffect(() => state.connection, (connection) => {
    if (connection === "connected") startPlugins();
  });

  // Attention notifications follow authoritative Thread attention.
  const needsYouCount = () =>
    threadState.threads.filter(t => !t.settled_at && !t.archived_at && t.attention === "needs_owner").length;

  // Reflect the needs-you count in document.title, so it's visible from a
  // backgrounded tab without push. (Replaces the React useTitleBadge hook.)
  createEffect(() => ({ count: needsYouCount(), enabled: titleBadgeEnabled() }), ({ count, enabled }) => {
    document.title =
      enabled && count > 0 ? `(${count}) ${BASE_TITLE}` : BASE_TITLE;
  });

  // Swap the tab favicon to the dotted variant while anything needs you, so a
  // backgrounded tab reads "attend to this" at a glance — one calm indigo dot on
  // the cube mark, never a red count. Reverts to the plain mark at zero.
  createEffect(() => needsYouCount() > 0, (dotted) => {
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
  createEffect(() => threadState.threads.filter(t => !t.settled_at && !t.archived_at && t.attention === "needs_owner").map(t => ({ id: t.id, title: t.title })), (blocking) => {
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
        body: newest.title,
        tag: `hirsel-thread-${newest.id}`,
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
      {/* One responsive shell with independently owned Thread conversations. */}
      <ThreadShell />
      <Toaster />
      {/* Summoned surfaces — no standing chrome. Opened from the keymap (⌘K /
          `?`) and command-palette affordances. */}
      <CommandPalette intent={commandPaletteIntent()} open={commandPaletteOpen()} onOpenChange={setCommandPaletteOpen} />
      <ShortcutHelp open={shortcutHelpOpen()} onOpenChange={setShortcutHelpOpen} />
    </Show>
  );
}

export default App;
