import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { installGlobalKeymap, isEditableTarget, type KeymapHandlers } from "./keymap";

// The `anyOverlayOpen` seam (created in a parallel worktree) isn't in this
// branch; mock the module so the keymap's suppression check is drivable here.
const { overlayRef } = vi.hoisted(() => ({ overlayRef: { open: false } }));
vi.mock("./focus", () => ({
  anyOverlayOpen: () => overlayRef.open,
  focusMainComposer: () => {},
}));

function makeHandlers(): KeymapHandlers {
  return {
    focusComposer: vi.fn(),
    goPane: vi.fn(),
    jumpToLatest: vi.fn(),
    openPalette: vi.fn(),
    showHelp: vi.fn(),
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
    press("i");
    expect(handlers.goPane).toHaveBeenCalledWith("inbox");
    press("g");
    press("p");
    expect(handlers.goPane).toHaveBeenCalledWith("processes");
    press("g");
    press("c");
    expect(handlers.goPane).toHaveBeenCalledWith("chat");
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
