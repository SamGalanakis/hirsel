// The overlay-presence contract, exercised against the REAL registry: the
// palette and the cheat-sheet are Kobalte dialogs that never push a focus trap,
// so their only claim on the keyboard is `createOverlayPresence`. Every other
// keymap suite mocks `lib/focus`, which is exactly how the two-registry split
// (bare keys firing under an open modal, Esc double-firing) went unnoticed —
// so nothing here is mocked.

import { render, waitFor } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { CommandPalette, ShortcutHelp } from "../components/CommandPalette";
import { ProcessesSheet } from "../components/processes/ProcessesSheet";
import {
  clearTaskFocus,
  closeRightRegion,
  dispatch,
  openProcesses,
  state,
  toggleTaskFocus,
} from "../store/store";
import { anyOverlayOpen } from "./focus";
import { defaultHandlers, installGlobalKeymap, type KeymapHandlers } from "./keymap";

function press(key: string, init: KeyboardEventInit = {}) {
  const ev = new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true, ...init });
  window.dispatchEvent(ev);
  return ev;
}

function spyHandlers(overrides: Partial<KeymapHandlers> = {}): KeymapHandlers {
  return {
    focusComposer: vi.fn(),
    goPane: vi.fn(),
    jumpToLatest: vi.fn(),
    openPalette: vi.fn(),
    showHelp: vi.fn(),
    escapeField: vi.fn(() => true),
    ...overrides,
  };
}

describe("overlay presence (real registry)", () => {
  beforeEach(() => {
    clearTaskFocus();
    closeRightRegion();
    dispatch({ type: "agent_activity", payload: { state: "idle", text: null } });
    // Auto-cleanup unmounted the previous test's dialog; if a token had leaked,
    // the global bare-key layer would be dead for the rest of the session.
    expect(anyOverlayOpen()).toBe(false);
  });

  it("does not clear task focus when Esc dismisses the open palette", async () => {
    toggleTaskFocus(7);
    render(() => <CommandPalette open onOpenChange={() => {}} />);
    await waitFor(() => expect(anyOverlayOpen()).toBe(true));

    // Real Esc ladder: rung 1 (an overlay owns Esc) must swallow it.
    const dispose = installGlobalKeymap(defaultHandlers);
    const ev = press("Escape");
    dispose();

    expect(state.focusedTaskId).toBe(7);
    expect(ev.defaultPrevented).toBe(false);
  });

  it("does not clear task focus when Esc dismisses a focus-trapped utility", async () => {
    toggleTaskFocus(3);
    openProcesses();
    render(() => <ProcessesSheet />);
    await waitFor(() => expect(anyOverlayOpen()).toBe(true));

    // Rung 1 again, for the other half of the registry: a utility pane whose
    // only keyboard claim is its own capture-phase focus trap. The trap closes
    // the pane synchronously, so unless it also CONSUMES the keystroke the
    // bubble-phase ladder sees no overlay left and falls through to rung 3,
    // clearing task focus on the same Esc that only dismissed the pane.
    const dispose = installGlobalKeymap(defaultHandlers);
    press("Escape");
    dispose();

    expect(state.rightRegion).toBe("none");
    expect(state.focusedTaskId).toBe(3);
  });

  it("suppresses bare keys while the shortcut cheat-sheet is open", async () => {
    render(() => <ShortcutHelp open onOpenChange={() => {}} />);
    await waitFor(() => expect(anyOverlayOpen()).toBe(true));

    const handlers = spyHandlers();
    const dispose = installGlobalKeymap(handlers);
    press("/");
    press("c");
    press("G");
    press("g");
    press("s");
    press("?");
    press("k", { metaKey: true });
    press("/", { metaKey: true });
    dispose();

    expect(handlers.focusComposer).not.toHaveBeenCalled();
    expect(handlers.jumpToLatest).not.toHaveBeenCalled();
    expect(handlers.goPane).not.toHaveBeenCalled();
    expect(handlers.showHelp).not.toHaveBeenCalled();
    expect(handlers.openPalette).not.toHaveBeenCalled();
  });

  it("restores the bare-key layer when the overlay closes", async () => {
    const [open, setOpen] = createSignal(true);
    render(() => <ShortcutHelp open={open()} onOpenChange={setOpen} />);
    await waitFor(() => expect(anyOverlayOpen()).toBe(true));

    setOpen(false);
    await waitFor(() => expect(anyOverlayOpen()).toBe(false));

    const handlers = spyHandlers();
    const dispose = installGlobalKeymap(handlers);
    press("/");
    press("/", { metaKey: true });
    dispose();

    expect(handlers.focusComposer).toHaveBeenCalledTimes(1);
    expect(handlers.showHelp).toHaveBeenCalledTimes(1);
  });
});
