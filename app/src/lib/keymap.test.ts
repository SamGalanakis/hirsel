import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { escapeField, installGlobalKeymap, isEditableTarget, type KeymapHandlers } from "./keymap";
import { clearTaskFocus, dispatch, state, toggleTaskFocus } from "../store/store";

// The `anyOverlayOpen` seam (created in a parallel worktree) isn't in this
// branch; mock the module so the keymap's suppression check is drivable here.
const { overlayRef } = vi.hoisted(() => ({ overlayRef: { open: false } }));
vi.mock("./focus", () => ({
  anyOverlayOpen: () => overlayRef.open,
  focusMainComposer: () => {},
  focusTaskIndex: () => {},
}));

function makeHandlers(): KeymapHandlers {
  return {
    focusComposer: vi.fn(),
    goPane: vi.fn(),
    jumpToLatest: vi.fn(),
    openPalette: vi.fn(),
    showHelp: vi.fn(),
    escapeField: vi.fn(() => true),
  };
}

function press(key: string, init: KeyboardEventInit = {}, target?: EventTarget) {
  const ev = new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true, ...init });
  (target ?? window).dispatchEvent(ev);
  return ev;
}

describe("keymap", () => {
  let dispose: () => void;
  let handlers: KeymapHandlers;

  beforeEach(() => {
    overlayRef.open = false;
    handlers = makeHandlers();
    dispose = installGlobalKeymap(handlers);
  });
  afterEach(() => dispose());

  it("focuses the composer on `/` and `c`", () => {
    press("/");
    press("c");
    expect(handlers.focusComposer).toHaveBeenCalledTimes(2);
  });

  it("runs `g`-prefixed pane chords", () => {
    press("g");
    press("p");
    expect(handlers.goPane).toHaveBeenCalledWith("processes");
    press("g");
    press("h");
    expect(handlers.goPane).toHaveBeenCalledWith("composer");
    press("g");
    press("t");
    expect(handlers.goPane).toHaveBeenCalledWith("tasks");
    // `c` alone (no leader) is still focus-composer, not a pane switch.
    expect(handlers.focusComposer).toHaveBeenCalledTimes(0);
  });

  it("jumps to latest on Shift+G and shows help on `?`", () => {
    press("G");
    expect(handlers.jumpToLatest).toHaveBeenCalledTimes(1);
    press("?");
    expect(handlers.showHelp).toHaveBeenCalledTimes(1);
  });

  it("opens the palette on ⌘K / Ctrl+K", () => {
    press("k", { metaKey: true });
    press("k", { ctrlKey: true });
    expect(handlers.openPalette).toHaveBeenCalledTimes(2);
  });

  it("suppresses the bare-key layer while typing in a field", () => {
    const input = document.createElement("input");
    document.body.appendChild(input);
    press("/", {}, input);
    expect(handlers.focusComposer).not.toHaveBeenCalled();
    input.remove();
  });

  it("suppresses shortcuts while an overlay owns input", () => {
    overlayRef.open = true;
    press("/");
    press("k", { metaKey: true }); // palette must not stack over an open overlay
    expect(handlers.focusComposer).not.toHaveBeenCalled();
    expect(handlers.openPalette).not.toHaveBeenCalled();
  });

  it("routes Esc to the ladder even while the caret is in a field", () => {
    const input = document.createElement("input");
    document.body.appendChild(input);
    const ev = press("Escape", {}, input);
    expect(handlers.escapeField).toHaveBeenCalledTimes(1);
    expect(ev.defaultPrevented).toBe(true);
    input.remove();
  });

  it("leaves Esc alone when the ladder declines it", () => {
    (handlers.escapeField as ReturnType<typeof vi.fn>).mockReturnValue(false);
    const ev = press("Escape");
    expect(ev.defaultPrevented).toBe(false);
  });

  it("prevents default on handled keys", () => {
    const ev = press("/");
    expect(ev.defaultPrevented).toBe(true);
  });

  it("detects editable targets", () => {
    const input = document.createElement("input");
    const div = document.createElement("div");
    expect(isEditableTarget(input)).toBe(true);
    expect(isEditableTarget(div)).toBe(false);
    expect(isEditableTarget(null)).toBe(false);
  });
});

describe("Esc ladder", () => {
  const idle = () =>
    dispatch({ type: "agent_activity", payload: { state: "idle", text: null } });
  const thinking = () =>
    dispatch({ type: "agent_activity", payload: { state: "thinking", text: null } });

  beforeEach(() => {
    overlayRef.open = false;
    clearTaskFocus();
    idle();
  });

  it("clears task focus when nothing above it owns Esc", () => {
    toggleTaskFocus(7);
    expect(escapeField()).toBe(true);
    expect(state.focusedTaskId).toBeNull();
  });

  it("yields to a running turn so Esc stops it instead of leaving the task", () => {
    toggleTaskFocus(7);
    thinking();
    expect(escapeField()).toBe(false);
    expect(state.focusedTaskId).toBe(7);
  });

  it("yields to an open overlay's focus trap", () => {
    toggleTaskFocus(7);
    overlayRef.open = true;
    expect(escapeField()).toBe(false);
    expect(state.focusedTaskId).toBe(7);
  });

  it("does nothing in the ambient field", () => {
    expect(escapeField()).toBe(false);
  });
});
