import { fireEvent, render } from "@solidjs/testing-library";
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
