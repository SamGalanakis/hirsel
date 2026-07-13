import { describe, expect, it, vi } from "vitest";
import { handleSubmitKeys, type SubmitKeymapHandlers } from "./submitKeymap";

// The shared composer submit keymap (extracted from Composer / ReplyInput /
// SideChatSheet so the three never drift). These are pure-function tests over a
// stubbed KeyboardEvent — no DOM.

function keyEvent(init: Partial<KeyboardEvent> & { key: string }): KeyboardEvent {
  return {
    key: init.key,
    metaKey: init.metaKey ?? false,
    ctrlKey: init.ctrlKey ?? false,
    shiftKey: init.shiftKey ?? false,
    preventDefault: vi.fn(),
  } as unknown as KeyboardEvent;
}

function handlers(over: Partial<SubmitKeymapHandlers> = {}): SubmitKeymapHandlers {
  return {
    value: () => "",
    coarse: () => false,
    onSend: vi.fn(),
    ...over,
  };
}

describe("handleSubmitKeys", () => {
  it("Cmd+Enter always sends, even on coarse pointers", () => {
    const onSend = vi.fn();
    const h = handlers({ coarse: () => true, onSend, value: () => "hi" });
    const e = keyEvent({ key: "Enter", metaKey: true });
    expect(handleSubmitKeys(e, h)).toBe(true);
    expect(onSend).toHaveBeenCalledOnce();
    expect(e.preventDefault).toHaveBeenCalled();
  });

  it("Ctrl+Enter always sends", () => {
    const onSend = vi.fn();
    const e = keyEvent({ key: "Enter", ctrlKey: true });
    expect(handleSubmitKeys(e, handlers({ onSend }))).toBe(true);
    expect(onSend).toHaveBeenCalledOnce();
  });

  it("plain Enter sends on fine pointers, but Shift+Enter is a newline", () => {
    const onSend = vi.fn();
    const h = handlers({ onSend });
    expect(handleSubmitKeys(keyEvent({ key: "Enter" }), h)).toBe(true);
    expect(onSend).toHaveBeenCalledOnce();

    onSend.mockClear();
    expect(handleSubmitKeys(keyEvent({ key: "Enter", shiftKey: true }), h)).toBe(false);
    expect(onSend).not.toHaveBeenCalled();
  });

  it("coarse pointers keep plain Enter as a newline (send button submits)", () => {
    const onSend = vi.fn();
    const h = handlers({ coarse: () => true, onSend });
    expect(handleSubmitKeys(keyEvent({ key: "Enter" }), h)).toBe(false);
    expect(onSend).not.toHaveBeenCalled();
  });

  it("ArrowUp on an empty input recalls the last owner message", () => {
    const onRecall = vi.fn();
    const h = handlers({
      value: () => "",
      recallLast: () => "prev message",
      onRecall,
    });
    const e = keyEvent({ key: "ArrowUp" });
    expect(handleSubmitKeys(e, h)).toBe(true);
    expect(onRecall).toHaveBeenCalledWith("prev message");
    expect(e.preventDefault).toHaveBeenCalled();
  });

  it("ArrowUp on a non-empty input does nothing (caret moves normally)", () => {
    const onRecall = vi.fn();
    const h = handlers({ value: () => "typing", recallLast: () => "prev", onRecall });
    expect(handleSubmitKeys(keyEvent({ key: "ArrowUp" }), h)).toBe(false);
    expect(onRecall).not.toHaveBeenCalled();
  });

  it("ArrowUp is inert when recall is not configured (ReplyInput)", () => {
    expect(handleSubmitKeys(keyEvent({ key: "ArrowUp" }), handlers())).toBe(false);
  });

  it("ArrowUp with nothing to recall consumes the key but calls no recall", () => {
    const onRecall = vi.fn();
    const h = handlers({ recallLast: () => null, onRecall });
    expect(handleSubmitKeys(keyEvent({ key: "ArrowUp" }), h)).toBe(true);
    expect(onRecall).not.toHaveBeenCalled();
  });
});
