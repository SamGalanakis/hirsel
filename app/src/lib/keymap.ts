// The global keyboard layer — hirsel's CLI-lineage "shortcuts are features"
// surface (Linear/Superhuman). A single window-level keydown listener routes
// bare keys and `g`-prefixed chords to app actions, and is deliberately quiet:
// it suppresses itself whenever the Owner is typing in a field or an overlay
// owns input, so it never steals a keystroke from the composer or a dialog.
//
// Esc is intentionally NOT handled here — the composer's "stop turn" and the
// focus-trap Escape handling own it, and this layer must not fight them.

import { createSignal } from "solid-js";
import { openProcesses, openSettings } from "../store/store";
import { getClient } from "../ws/client";
// SEAM (created in a parallel worktree): true while a modal/overlay/focus-trap
// owns input. Used to suppress the bare-key layer so summoned surfaces keep the
// keyboard. Documented import: `import { anyOverlayOpen } from "./lib/focus"`.
import { anyOverlayOpen, focusMainComposer, focusTaskIndex } from "./focus";

/** Max gap (ms) between the `g` leader and its second key for a chord to count. */
const CHORD_MS = 900;

export type PaneTarget = "tasks" | "composer" | "processes" | "settings";

// ---- Summoned-overlay visibility (module singletons; one app instance) -------
// The command palette and the shortcut cheat-sheet are summoned, never standing,
// so their open-state lives here where the keymap, App, and command affordances
// can all reach it without prop-drilling.
export const [commandPaletteOpen, setCommandPaletteOpen] = createSignal(false);
export const [shortcutHelpOpen, setShortcutHelpOpen] = createSignal(false);

// ---- Actions (shared by the keymap and the command palette, so both agree) ---

/** Land the caret in the one globally aware Hirsel composer. */
export function focusComposer(): void {
  focusMainComposer();
}

/** Focus one of the two standing surfaces, or summon a utility. */
export function goPane(target: PaneTarget): void {
  switch (target) {
    case "composer":
      focusMainComposer();
      break;
    case "tasks":
      focusTaskIndex();
      break;
    case "processes":
      openProcesses();
      break;
    case "settings":
      openSettings();
      break;
  }
}

/** Jump to the newest task-context material in the current field. */
export function jumpToLatest(): void {
  document.querySelector<HTMLElement>('[data-slot="task-scroll"]')?.scrollTo({
    top: Number.MAX_SAFE_INTEGER,
    behavior: "smooth",
  });
}

/** Best-effort cancel of the live turn — a no-op when the agent is idle. */
export function stopActiveTurn(): void {
  getClient()?.cancelTurn();
}

// ---- Cheat-sheet / hint vocabulary (one source for help + palette hints) ------

export interface Shortcut {
  /** Display tokens for the keys, rendered as mono chips. A two-element array is
   * a chord (`g` then `t`); a comma in a single token means "or". */
  keys: string[];
  label: string;
  group: "Tasks" | "General" | "Focus" | "Hirsel";
}

export const SHORTCUTS: Shortcut[] = [
  { keys: ["⌘", "K"], label: "Command palette", group: "General" },
  { keys: ["?"], label: "Keyboard shortcuts", group: "General" },
  { keys: ["/"], label: "Focus Hirsel", group: "Hirsel" },
  { keys: ["G"], label: "Jump to latest", group: "Hirsel" },
  { keys: ["Enter"], label: "Send message", group: "Hirsel" },
  { keys: ["⇧", "Enter"], label: "New line", group: "Hirsel" },
  { keys: ["Tab"], label: "Queue for next turn", group: "Hirsel" },
  { keys: ["Esc"], label: "Stop the active turn", group: "Hirsel" },
  { keys: ["@"], label: "Mention a task", group: "Hirsel" },
  { keys: ["g", "t"], label: "Focus tasks", group: "Focus" },
  { keys: ["g", "h"], label: "Focus Hirsel", group: "Focus" },
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
  t: "tasks",
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
