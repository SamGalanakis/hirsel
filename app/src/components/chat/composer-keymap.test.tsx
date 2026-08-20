import { fireEvent, render } from "@solidjs/testing-library";
import userEvent from "@testing-library/user-event";
import { createRoot } from "solid-js";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Blob } from "../../protocol";
import type { AttachmentsController } from "./useAttachments";

// Composer keymap / draft / overlay behaviour (ui/composer pass): the Esc
// overlay gate (task 1), per-surface draft persistence (task 2), and the
// labeled queue affordance (task 4). Fresh store singleton per test, and a
// clean localStorage so a draft left by one test never leaks into the next.
// This env's jsdom `localStorage` is a bare object with no getItem/setItem, so
// the app's draft persistence silently no-ops. Install a real Map-backed stub
// so the persistence assertions actually exercise the store.
function installLocalStorage() {
  const store = new Map<string, string>();
  const ls: Storage = {
    getItem: (k) => (store.has(k) ? (store.get(k) as string) : null),
    setItem: (k, v) => void store.set(k, String(v)),
    removeItem: (k) => void store.delete(k),
    clear: () => store.clear(),
    key: (i) => Array.from(store.keys())[i] ?? null,
    get length() {
      return store.size;
    },
  };
  Object.defineProperty(globalThis, "localStorage", { value: ls, configurable: true });
}

beforeEach(() => {
  vi.resetModules();
  installLocalStorage();
});

function stubAttachments(): AttachmentsController {
  return {
    files: () => [],
    addFiles: () => {},
    addPastedFiles: () => {},
    addPastedText: () => {},
    addFromTransfer: () => {},
    takeText: () => null,
    removeFile: () => {},
    retry: () => {},
    clear: () => {},
    uploadAll: async () => [] as Blob[],
  };
}

async function renderComposer(props: {
  thinking?: boolean;
  onStop?: () => void;
  onSend?: (...a: unknown[]) => void;
  getLastOwnerBody?: () => string | null;
}) {
  const { Composer } = await import("./Composer");
  const utils = render(() => (
    <Composer
      attachments={stubAttachments()}
      thinking={props.thinking ?? false}
      onSend={(props.onSend ?? (() => {})) as never}
      onStop={props.onStop ?? (() => {})}
      getLastOwnerBody={props.getLastOwnerBody ?? (() => null)}
    />
  ));
  const textarea = utils.container.querySelector(
    '[data-composer="main"]',
  ) as HTMLTextAreaElement;
  return { ...utils, textarea };
}

describe("Esc overlay gate (task 1)", () => {
  it("stops the turn on Esc when the agent is thinking and no overlay is open", async () => {
    const onStop = vi.fn();
    const { textarea } = await renderComposer({ thinking: true, onStop });
    fireEvent.keyDown(textarea, { key: "Escape" });
    expect(onStop).toHaveBeenCalledOnce();
  });

  it("does NOT stop the turn when an overlay / focus trap is open", async () => {
    const { createFocusTrap } = await import("../../lib/focus");
    const onStop = vi.fn();
    const { textarea } = await renderComposer({ thinking: true, onStop });

    // Open an overlay (pushes the shared trap stack anyOverlayOpen() reads).
    const host = document.createElement("div");
    host.tabIndex = -1;
    document.body.appendChild(host);
    const dispose = createRoot((d) => {
      createFocusTrap(() => host, {});
      return d;
    });

    fireEvent.keyDown(textarea, { key: "Escape" }); // meant for the overlay
    expect(onStop).not.toHaveBeenCalled();

    // Close the overlay: Esc reaches the turn again.
    dispose();
    fireEvent.keyDown(textarea, { key: "Escape" });
    expect(onStop).toHaveBeenCalledOnce();
  });

  it("is a no-op when the agent is idle", async () => {
    const onStop = vi.fn();
    const { textarea } = await renderComposer({ thinking: false, onStop });
    fireEvent.keyDown(textarea, { key: "Escape" });
    expect(onStop).not.toHaveBeenCalled();
  });
});

describe("Per-surface draft persistence (task 2)", () => {
  it("restores the main draft after the composer unmounts and remounts", async () => {
    const first = await renderComposer({});
    fireEvent.input(first.textarea, { target: { value: "half-written thought" } });
    expect(localStorage.getItem("hirsel.draft.main")).toBe("half-written thought");
    first.unmount();

    const second = await renderComposer({});
    expect(second.textarea.value).toBe("half-written thought");
  });

  it("clears the stored draft on a successful send", async () => {
    const onSend = vi.fn();
    const { textarea } = await renderComposer({ onSend });
    fireEvent.input(textarea, { target: { value: "ship it" } });
    fireEvent.keyDown(textarea, { key: "Enter" });
    expect(onSend).toHaveBeenCalledOnce();
    expect(localStorage.getItem("hirsel.draft.main")).toBeNull();
  });
});

describe("Queue-next-turn affordance (task 4)", () => {
  it("queues the draft on Tab, in next_turn mode", async () => {
    const onSend = vi.fn();
    const { textarea } = await renderComposer({ onSend });
    fireEvent.input(textarea, { target: { value: "later work" } });

    fireEvent.keyDown(textarea, { key: "Tab" });

    expect(onSend).toHaveBeenCalledOnce();
    // onSend(body, ref, mode, blobs, mentions) — mode is the 3rd arg.
    expect(onSend.mock.calls[0][2]).toBe("next_turn");
  });

  it("leaves Tab as ordinary focus movement when the draft is empty", async () => {
    const onSend = vi.fn();
    const { textarea } = await renderComposer({ onSend });
    fireEvent.keyDown(textarea, { key: "Tab" });
    expect(onSend).not.toHaveBeenCalled();
  });

  it("writes BOTH queue routes into the shortcut sheet, since the capsule shows neither", async () => {
    const { SHORTCUTS } = await import("../../lib/keymap");
    const queue = SHORTCUTS.filter((s) => /Queue for next turn/.test(s.label));
    expect(queue.map((s) => s.keys.join("+")).sort()).toEqual(["Hold Send", "Tab"]);
    expect(queue.every((s) => s.group === "Hirsel")).toBe(true);
  });
});

describe("No dead affordances in the capsule (composer redesign)", () => {
  // Enter is the send on a fine pointer, so a Send button beside it was a
  // second route to the same thing; the caret next to it opened a send-options
  // menu that was `disabled` on an empty draft — the affordance did nothing
  // exactly when it was reached for. Both are gone. A coarse pointer keeps
  // Send, because there Enter is a newline. jsdom reports a fine pointer.
  it("shows neither a Send button nor a send-options caret on a fine pointer", async () => {
    const { queryByLabelText, textarea } = await renderComposer({});
    expect(queryByLabelText("More send options")).toBeNull();
    expect(queryByLabelText("Send")).toBeNull();
    fireEvent.input(textarea, { target: { value: "a non-empty draft" } });
    expect(queryByLabelText("More send options")).toBeNull();
    expect(queryByLabelText("Send")).toBeNull();
  });

  it("still sends on Enter and keeps attach and Stop reachable while thinking", async () => {
    const onSend = vi.fn();
    const onStop = vi.fn();
    const { textarea, getByLabelText } = await renderComposer({ onSend, onStop, thinking: true });
    expect(getByLabelText("Attach files")).toBeInTheDocument();

    const user = userEvent.setup();
    await user.click(getByLabelText("Stop the agent"));
    expect(onStop).toHaveBeenCalledOnce();

    fireEvent.input(textarea, { target: { value: "ship it" } });
    fireEvent.keyDown(textarea, { key: "Enter" });
    expect(onSend).toHaveBeenCalledOnce();
    expect(onSend.mock.calls[0][2]).toBe("send");
  });

  it("rests one line high: the capsule's own padding plus a 36px text row", async () => {
    const { container, textarea } = await renderComposer({});
    const shell = container.querySelector('[data-slot="composer-shell"]') as HTMLElement;
    // The resting capsule is 44px on a fine pointer (py-1 + min-h-9), not the
    // 60px slab it was (py-2 + min-h-11). Asserted through the classes because
    // jsdom does not lay out.
    expect(shell.className).toContain("py-1");
    expect(shell.className).not.toContain("py-2");
    expect(textarea.className).toContain("min-h-9");
    expect(textarea.className).toContain("max-h-28");
  });
});
