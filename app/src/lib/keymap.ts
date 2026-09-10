import { openThreadNavigation, closeThreadNavigation } from "../threads/navigation";
import { threadState } from "../threads/store";
// The global keyboard layer — hirsel's CLI-lineage "shortcuts are features"
// surface (Linear/Superhuman). A single window-level keydown listener routes
// bare keys and `g`-prefixed chords to app actions, and is deliberately quiet:
// it suppresses itself whenever the Owner is typing in a field or an overlay
// owns input, so it never steals a keystroke from the composer or a dialog.
//
// Escape stays with the active control; global shortcuts never retarget messages.

import { createSignal } from "solid-js";

import { scrollToBottom } from "./scroll";
import { openProcesses, openSettings } from "../store/store";
import { getClient } from "../ws/client";
import { historyId } from "./history";
// True while a modal/overlay owns input — focus traps and the native dialogs
// that register their own presence both feed it. Used to suppress the bare-key
// layer so summoned surfaces keep the keyboard.
import { anyOverlayOpen, focusMainComposer } from "./focus";

/** Max gap (ms) between the `g` leader and its second key for a chord to count. */
const CHORD_MS = 900;

export type PaneTarget = "threads" | "composer" | "processes" | "settings";

// ---- Summoned-overlay visibility (module singletons; one app instance) -------
// The command palette and the shortcut cheat-sheet are summoned, never standing,
// so their open-state lives here where the keymap, App, and command affordances
// can all reach it without prop-drilling.
export const [commandPaletteOpen, updateCommandPaletteOpen] = createSignal(false);
export const [commandPaletteIntent, setCommandPaletteIntent] = createSignal<"commands" | "threads">("commands");
export function setCommandPaletteOpen(open: boolean): void { setCommandPaletteIntent("commands"); updateCommandPaletteOpen(open); }
export function openThreadSearch(): void {
  closeThreadNavigation();
  queueMicrotask(() => {
    document.querySelector<HTMLButtonElement>('[data-slot="thread-navigation-trigger"]')?.focus();
    setCommandPaletteIntent("threads");
    updateCommandPaletteOpen(true);
  });
}
export const [shortcutHelpOpen, setShortcutHelpOpen] = createSignal(false);

// ---- Actions (shared by the keymap and the command palette, so both agree) ---

/** Land the caret in the one globally aware Hirsel composer. */
export function focusComposer(): void {
  focusMainComposer();
}

/** Open the Thread drawer, focus the composer, or summon a utility. */
export function goPane(target: PaneTarget): void {
  switch (target) {
    case "composer":
      focusMainComposer();
      break;
    case "threads":
      openThreadNavigation();
      break;
    case "processes":
      openProcesses();
      break;
    case "settings":
      openSettings();
      break;
  }
}

/** Jump to the newest Thread context material in the current field. */
export function jumpToLatest(): void {
  const element = document.querySelector<HTMLElement>('[data-slot="thread-scroll"]');
  // Shares the conversation's own bottom-pinning helper, so the keyboard route
  // and the "jump to latest" affordance land in the same place and both go
  // instant under `prefers-reduced-motion` (DESIGN §5).
  if (element) scrollToBottom(element);
}

/** Best-effort cancel of the live turn — a no-op when the agent is idle. */
export function stopActiveTurn(): void {
  const history = historyId();
  if (history && threadState.focusedId !== null) getClient()?.cancelTurn(history, threadState.focusedId);
}

// ---- Cheat-sheet / hint vocabulary (one source for help + palette hints) ------

export interface Shortcut {
  /** Display tokens for the keys, rendered as mono chips. A two-element array is
   * a chord (`g` then `t`); a comma in a single token means "or". */
  keys: string[];
  label: string;
  group: "Spaces & Tasks" | "General" | "Focus" | "Hirsel";
}

export const SHORTCUTS: Shortcut[] = [
  { keys: ["⌘", "K"], label: "Search commands, Spaces and Tasks", group: "General" },
  // Two routes to the same sheet, listed adjacently: ⌘/ reaches it mid-type,
  // `?` is the bare-key one you find by accident.
  { keys: ["⌘", "/"], label: "Keyboard shortcuts", group: "General" },
  { keys: ["?"], label: "Keyboard shortcuts", group: "General" },
  { keys: ["/"], label: "Focus conversation", group: "Hirsel" },
  { keys: ["G"], label: "Jump to latest", group: "Hirsel" },
  { keys: ["Enter"], label: "Send message", group: "Hirsel" },
  { keys: ["⇧", "Enter"], label: "New line", group: "Hirsel" },
  { keys: ["⌘/Ctrl", "Shift", "Enter"], label: "Queue for next turn", group: "Hirsel" },
  // The composer carries no queue button any more, so the sheet is where BOTH
  // routes to a queued turn are written down — the desktop key and the touch
  // gesture, which is the only one a phone can reach.
  { keys: ["Hold Send"], label: "Queue for next turn (touch)", group: "Hirsel" },
  { keys: ["Esc"], label: "Stop active turn (composer focused)", group: "Hirsel" },
  { keys: ["#"], label: "Cite a thread", group: "Hirsel" },
  { keys: ["g", "t"], label: "Open Spaces and Tasks", group: "Focus" },
  { keys: ["g", "h"], label: "Focus conversation", group: "Focus" },
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
  h: "composer",
  t: "threads",
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

    // ⌘/ (Ctrl+/) summons the cheat-sheet on the same terms — reachable with the
    // caret in the composer, where the bare `?` route is (correctly) content.
    if (meta && e.key === "/") {
      if (anyOverlayOpen()) return;
      e.preventDefault();
      handlers.showHelp();
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

    // Resolve a pending `g` chord (`g t`, `g h`, `g p`, `g s`).
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
