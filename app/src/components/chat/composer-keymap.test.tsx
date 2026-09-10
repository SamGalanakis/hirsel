import { fireEvent, render } from "@solidjs/testing-library";
import userEvent from "@testing-library/user-event";
import { createRoot, flush } from "solid-js";

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
  it("queues the draft on Ctrl+Shift+Enter, in next_turn mode", async () => {
    const onSend = vi.fn();
    const { textarea } = await renderComposer({ onSend });
    fireEvent.input(textarea, { target: { value: "later work" } });

    fireEvent.keyDown(textarea, { key: "Enter", ctrlKey: true, shiftKey: true });

    expect(onSend).toHaveBeenCalledOnce();
    // onSend(body, mode, blobs, mentions) — mode is the 2nd arg.
    expect(onSend.mock.calls[0][1]).toBe("next_turn");
  });

  it("leaves Tab as ordinary focus movement with an unfinished draft", async () => {
    const onSend = vi.fn();
    const { textarea } = await renderComposer({ onSend });
    fireEvent.input(textarea, { target: { value: "unfinished draft" } });
    expect(fireEvent.keyDown(textarea, { key: "Tab" })).toBe(true);
    expect(onSend).not.toHaveBeenCalled();
    expect(textarea.value).toBe("unfinished draft");
  });

  it("writes BOTH queue routes into the shortcut sheet, since the capsule shows neither", async () => {
    const { SHORTCUTS } = await import("../../lib/keymap");
    const queue = SHORTCUTS.filter((s) => /Queue for next turn/.test(s.label));
    expect(queue.map((s) => s.keys.join("+")).sort()).toEqual(["Hold Send", "⌘/Ctrl+Shift+Enter"]);
    expect(queue.every((s) => s.group === "Hirsel")).toBe(true);
  });
});

describe("No dead affordances in the capsule (composer redesign)", () => {
  it("keeps a working Send button on fine pointers alongside Enter", async () => {
    const onSend = vi.fn();
    const { getByLabelText, queryByLabelText, textarea } = await renderComposer({ onSend });
    expect(queryByLabelText("More send options")).toBeNull();
    expect(getByLabelText("Send")).toBeDisabled();
    fireEvent.input(textarea, { target: { value: "a non-empty draft" } });
    expect(getByLabelText("Send")).not.toBeDisabled();
    await userEvent.setup().click(getByLabelText("Send"));
    expect(onSend).toHaveBeenCalledOnce();
    expect(onSend.mock.calls[0][0]).toBe("a non-empty draft");
    expect(onSend.mock.calls[0][1]).toBe("send");
    expect(textarea.value).toBe("");
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
    expect(onSend.mock.calls[0][1]).toBe("send");
  });

  it.each(["send", "next_turn"])("keeps busy touch %s available without stopping the active turn", async (mode) => {
    const matchMedia = vi.spyOn(window, "matchMedia").mockImplementation(query => ({
      matches: query === "(pointer: coarse)", media: query, onchange: null,
      addEventListener: () => {}, removeEventListener: () => {},
      addListener: () => {}, removeListener: () => {}, dispatchEvent: () => false,
    }));
    try {
      const onSend = vi.fn(), onStop = vi.fn();
      const { textarea, getByLabelText } = await renderComposer({ thinking: true, onSend, onStop });
      flush();
      fireEvent.input(textarea, { target: { value: "Continue with this follow-up" } });
      const send = getByLabelText("Send");
      expect(getByLabelText("Stop the agent")).toBeInTheDocument();
      if (mode === "next_turn") {
        vi.useFakeTimers();
        fireEvent.pointerDown(send);
        flush(() => vi.advanceTimersByTime(500));
        fireEvent.pointerUp(send);
      }
      fireEvent.click(send);
      expect(onSend).toHaveBeenCalledOnce();
      expect(onSend.mock.calls[0]).toEqual(["Continue with this follow-up", mode, [], [], []]);
      expect(onStop).not.toHaveBeenCalled();
      expect(textarea.value).toBe("");
      fireEvent.click(getByLabelText("Stop the agent"));
      expect(onStop).toHaveBeenCalledOnce();
    } finally { vi.useRealTimers(); matchMedia.mockRestore(); }
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

it.each([false, true])("snapshots artifact context before upload and rejects a changed history (%s)", async (resetHistory) => {
  const { Composer } = await import("./Composer");
  const { createSignal } = await import("solid-js");
  const { setHistoryId } = await import("../../lib/history");
  const { draftArtifact, stageDraftArtifact, consumeDraftArtifact } = await import("../../artifacts/draft-context");
  flush(() => { setHistoryId("upload-history"); stageDraftArtifact(1,{id:44,title:"Original"}); });
  const [context, setContext] = createSignal({id:44,title:"Original"});
  let finish!: (blobs: Blob[]) => void;
  const attachments = stubAttachments();
  attachments.files = () => [{ clientId:"upload",file:new File(["data"],"notes.txt"),name:"notes.txt",mime:"text/plain",size:4,kind:"file",upload:{state:"idle"} }];
  attachments.uploadAll = () => new Promise(resolve => { finish=resolve; });
  const onSend=vi.fn();
  const view=render(()=> <Composer attachments={attachments} thinking={false} artifactContext={context()} onConsumeArtifactContext={id=>consumeDraftArtifact(1,id)} onSend={onSend} onStop={()=>{}} getLastOwnerBody={()=>null} />);
  const textarea=view.container.querySelector("textarea")!;
  fireEvent.input(textarea,{target:{value:"Edit the referenced result"}});fireEvent.keyDown(textarea,{key:"Enter"});
  flush(()=>{stageDraftArtifact(1,{id:55,title:"Replacement"});setContext({id:55,title:"Replacement"});if(resetHistory)setHistoryId("different-history");});
  finish([]); await new Promise(resolve=>setTimeout(resolve,0));
  if(resetHistory)expect(onSend).not.toHaveBeenCalled();
  else {expect(onSend).toHaveBeenCalledWith("Edit the referenced result","send",[],[],[44]);expect(draftArtifact(1)?.id).toBe(55);}
});
