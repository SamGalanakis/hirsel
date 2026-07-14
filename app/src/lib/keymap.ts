// The global keyboard layer — hirsel's CLI-lineage "shortcuts are features"
// surface (Linear/Superhuman). A single window-level keydown listener routes
// bare keys and `g`-prefixed chords to app actions, and is deliberately quiet:
// it suppresses itself whenever the Owner is typing in a field or an overlay
// owns input, so it never steals a keystroke from the composer or a dialog.
//
// Esc is intentionally NOT handled here — the composer's "stop turn" and the
// focus-trap Escape handling own it, and this layer must not fight them.

import { createSignal } from "solid-js";
import {
  goToChat,
  goToQueue,
  openPings,
  openProcesses,
  openSettings,
  setScrollTarget,
  setTrayExpanded,
  state,
} from "../store/store";
import { getClient } from "../ws/client";
// SEAM (created in a parallel worktree): true while a modal/overlay/focus-trap
// owns input. Used to suppress the bare-key layer so summoned surfaces keep the
// keyboard. Documented import: `import { anyOverlayOpen } from "./lib/focus"`.
import { anyOverlayOpen, focusFeedColumn, focusMainComposer } from "./focus";

/** Max gap (ms) between the `g` leader and its second key for a chord to count. */
const CHORD_MS = 900;

export type PaneTarget = "chat" | "feed" | "pings" | "processes" | "settings";

/** Desktop-unified (`rail`, ≥1100px): Feed and Chat are visible AT ONCE, so the
 * old "home switch" chords (`g c`, `g f`/`g i`) become FOCUS moves between the
 * standing panes rather than navigations. Below the rail the phone keeps the
 * home-switch semantics. Read straight off matchMedia — the key layer is a
 * window singleton with no width signal to thread. */
function atRailNow(): boolean {
  return typeof window !== "undefined" && !!window.matchMedia?.("(min-width: 1100px)").matches;
}

// ---- Summoned-overlay visibility (module singletons; one app instance) -------
// The command palette and the shortcut cheat-sheet are summoned, never standing,
// so their open-state lives here where the keymap, App (render), and the NavRail
// affordance can all reach it without prop-drilling.
export const [commandPaletteOpen, setCommandPaletteOpen] = createSignal(false);
export const [shortcutHelpOpen, setShortcutHelpOpen] = createSignal(false);

// ---- Actions (shared by the keymap and the command palette, so both agree) ---

/** Land the caret in the main composer. Desktop: Chat is always on screen, so
 * this is a pure focus move — it must NOT reset the right region (a `/` mid-task
 * should never evict an open Processes/Settings pane). Phone: Chat is a drill-in,
 * so land there first, then focus. */
export function focusComposer(): void {
  if (!atRailNow()) goToChat();
  focusMainComposer();
}

/** Switch/focus the primary surface. On the phone every case is a navigation
 * (home switch or a docked right-region pane). On desktop (unified shell) Feed
 * and Chat are standing panes, so `chat`/`feed`/`pings` FOCUS the visible pane
 * instead of navigating; `processes`/`settings` still dock the shared right
 * region either way. Routing through these intents keeps the keyboard and the
 * command palette in agreement about what each target means. */
export function goPane(target: PaneTarget): void {
  const rail = atRailNow();
  switch (target) {
    case "chat":
      if (rail) focusMainComposer();
      else goToChat();
      break;
    case "feed":
      if (rail) focusFeedColumn();
      else goToQueue();
      break;
    case "pings":
      // The Feed IS the needs-you / pings surface on desktop, so `g i` focuses
      // it there; on phone it summons the Tray overlay.
      if (rail) focusFeedColumn();
      else {
        openPings();
        setTrayExpanded(true);
      }
      break;
    case "processes":
      openProcesses();
      break;
    case "settings":
      openSettings();
      break;
  }
}

/** Jump to the newest message. Desktop: Chat is on screen, so just ask it to
 * scroll (no home/region change). Phone: return to Chat with the scroll intent. */
export function jumpToLatest(): void {
  const msgs = state.messages;
  const lastId = msgs.length > 0 ? msgs[msgs.length - 1].id : null;
  if (atRailNow()) {
    if (lastId !== null) setScrollTarget(lastId);
    return;
  }
  if (lastId !== null) goToChat({ scrollToMessageId: lastId });
  else goToChat();
}

/** Best-effort cancel of the live turn — a no-op when the agent is idle. */
export function stopActiveTurn(): void {
  getClient()?.cancelTurn();
}

// ---- Cheat-sheet / hint vocabulary (one source for help + palette hints) ------

export interface Shortcut {
  /** Display tokens for the keys, rendered as mono chips. A two-element array is
   * a chord (`g` then `i`); a comma in a single token means "or". */
  keys: string[];
  label: string;
  group: "Feed" | "General" | "Focus" | "Chat";
}

export const SHORTCUTS: Shortcut[] = [
  // Feed — the event queue (the phone scroller's flick/decide keys; the desktop
  // Feed column decides the same options by click / Tab + Enter). Surfaced here
  // so the `?` sheet tells the truth about the needs-you surface's keys.
  { keys: ["J / K"], label: "Next / previous event", group: "Feed" },
  { keys: ["→"], label: "Accept the recommended pick", group: "Feed" },
  { keys: ["←"], label: "Snooze to the end", group: "Feed" },
  { keys: ["A–Z"], label: "Choose that option", group: "Feed" },
  { keys: ["P"], label: "Peek the whole queue", group: "Feed" },
  { keys: ["⌘", "K"], label: "Command palette", group: "General" },
  { keys: ["?"], label: "Keyboard shortcuts", group: "General" },
  { keys: ["/"], label: "Focus composer", group: "Chat" },
  { keys: ["G"], label: "Jump to latest", group: "Chat" },
  // Composer keys (fine pointer). Formerly a standing hint row under the input;
  // that teaching chrome is gone, so the `?` sheet is now their only home.
  { keys: ["Enter"], label: "Send message", group: "Chat" },
  { keys: ["⇧", "Enter"], label: "New line", group: "Chat" },
  { keys: ["Tab"], label: "Queue for next turn", group: "Chat" },
  { keys: ["Esc"], label: "Stop the active turn", group: "Chat" },
  { keys: ["@"], label: "Mention a Ping", group: "Chat" },
  // Focus — desktop shows Feed + Chat at once, so `g` chords move focus between
  // the standing panes (below the rail they navigate home instead).
  { keys: ["g", "f"], label: "Focus Feed", group: "Focus" },
  { keys: ["g", "c"], label: "Focus Chat", group: "Focus" },
  { keys: ["g", "p"], label: "Processes", group: "Focus" },
  { keys: ["g", "s"], label: "Settings", group: "Focus" },
];

// ---- The listener -----------------------------------------------------------

export interface KeymapHandlers {
  focusComposer(): void;
  goPane(target: PaneTarget): void;
  jumpToLatest(): void;
  openPalette(): void;
  showHelp(): void;
}

/** The production wiring: bare-key/chord actions run the shared action helpers;
 * palette/help toggle the summoned-overlay signals. Injectable so the routing
 * can be unit-tested against spies without touching the store. */
export const defaultHandlers: KeymapHandlers = {
  focusComposer,
  goPane,
  jumpToLatest,
  openPalette: () => setCommandPaletteOpen(true),
  showHelp: () => setShortcutHelpOpen(true),
};

/** True when the event originated in a text-entry surface — where a bare key is
 * content, not a command. */
export function isEditableTarget(target: EventTarget | null): boolean {
  const el = target as HTMLElement | null;
  if (!el || typeof el.tagName !== "string") return false;
  const tag = el.tagName;
  return tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || el.isContentEditable === true;
}

const CHORD_PANES: Record<string, PaneTarget> = {
  c: "chat",
  f: "feed",
  i: "pings",
  p: "processes",
  s: "settings",
};

/** Install the global keydown layer. Returns a disposer that removes the
 * listener (call from `onCleanup`). */
export function installGlobalKeymap(handlers: KeymapHandlers = defaultHandlers): () => void {
  let pendingG = false;
  let gTimer: ReturnType<typeof setTimeout> | undefined;
  const clearG = () => {
    pendingG = false;
    if (gTimer !== undefined) clearTimeout(gTimer);
    gTimer = undefined;
  };

  const onKeyDown = (e: KeyboardEvent) => {
    const meta = e.metaKey || e.ctrlKey;

    // ⌘K / Ctrl+K summons the palette from anywhere — even mid-type — since the
    // modifier means it can never be mistaken for typed content. Still yields to
    // an already-open overlay so it can't stack over itself.
    if (meta && (e.key === "k" || e.key === "K")) {
      if (anyOverlayOpen()) return;
      e.preventDefault();
      handlers.openPalette();
      return;
    }

    // No other modifier combo belongs to this layer.
    if (meta || e.altKey) return;

    // The bare-key layer is silent while the Owner is typing or an overlay owns
    // input — this is what keeps it from ever eating a composer keystroke.
    if (isEditableTarget(e.target) || anyOverlayOpen()) {
      clearG();
      return;
    }

    // Resolve a pending `g` chord (`g i`, `g p`, `g s`, `g c`).
    if (pendingG) {
      const dest = CHORD_PANES[e.key.toLowerCase()];
      clearG();
      if (dest) {
        e.preventDefault();
        handlers.goPane(dest);
      }
      return;
    }

    switch (e.key) {
      case "g":
        pendingG = true;
        gTimer = setTimeout(clearG, CHORD_MS);
        e.preventDefault();
        return;
      case "/":
      case "c":
        e.preventDefault();
        handlers.focusComposer();
        return;
      case "G": // Shift+G — vim-lineage "jump to bottom".
        e.preventDefault();
        handlers.jumpToLatest();
        return;
      case "?": // Shift+/ — the cheat-sheet.
        e.preventDefault();
        handlers.showHelp();
        return;
    }
  };

  window.addEventListener("keydown", onKeyDown);
  return () => {
    window.removeEventListener("keydown", onKeyDown);
    clearG();
  };
}
