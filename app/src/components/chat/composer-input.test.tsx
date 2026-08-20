import { fireEvent, render } from "@solidjs/testing-library";
import { createRoot } from "solid-js";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Blob } from "../../protocol";
import { LARGE_PASTE_CHARS } from "./paste";

// Composer input parity (paste images, large-paste-as-ref, drag & drop). These
// drive the real attachments controller rather than a stub, so the routing
// under test is the one that ships: what the composer decides to do with a
// clipboard or a drop, and what ends up staged as a result.

beforeEach(() => {
  vi.resetModules();
});

/** Clipboard/drop payloads jsdom cannot construct: `DataTransfer` exists but
 * has no usable `items`, so the handlers get a shaped stand-in. */
function transfer(opts: {
  text?: string;
  files?: File[];
  directories?: string[];
}): DataTransfer {
  const items = [
    ...(opts.text !== undefined ? [{ kind: "string", getAsFile: () => null }] : []),
    ...(opts.files ?? []).map((file) => ({
      kind: "file",
      webkitGetAsEntry: () => ({ isDirectory: false }),
      getAsFile: () => file,
    })),
    ...(opts.directories ?? []).map((name) => ({
      kind: "file",
      webkitGetAsEntry: () => ({ isDirectory: true }),
      getAsFile: () => new File([], name),
    })),
  ];
  return {
    items,
    files: [],
    types: items.some((i) => i.kind === "file") ? ["Files"] : ["text/plain"],
    getData: () => opts.text ?? "",
    dropEffect: "none",
  } as unknown as DataTransfer;
}

async function renderComposer() {
  const { Composer } = await import("./Composer");
  const { createComposerAttachments } = await import("./useAttachments");
  const attachments = createRoot(() => createComposerAttachments());
  const utils = render(() => (
    <Composer
      attachments={attachments}
      thinking={false}
      onSend={() => {}}
      onStop={() => {}}
      getLastOwnerBody={() => null}
    />
  ));
  const textarea = utils.container.querySelector(
    '[data-composer="main"]',
  ) as HTMLTextAreaElement;
  return { ...utils, textarea, attachments };
}

function paste(el: Element, data: DataTransfer) {
  const event = new Event("paste", { bubbles: true, cancelable: true });
  Object.defineProperty(event, "clipboardData", { value: data });
  el.dispatchEvent(event);
  return event;
}

describe("pasting images", () => {
  it("stages a pasted image as a named chip instead of typing into the field", async () => {
    const { textarea, attachments, findByText } = await renderComposer();
    const png = new File(["bytes"], "image.png", { type: "image/png" });

    const event = paste(textarea, transfer({ files: [png] }));

    expect(event.defaultPrevented).toBe(true);
    expect(attachments.files()).toHaveLength(1);
    expect(attachments.files()[0].kind).toBe("image");
    expect(await findByText("pasted-image-1.png")).toBeTruthy();
    expect(textarea.value).toBe("");
  });

  it("numbers multiple pasted images within one message", async () => {
    const { textarea, attachments } = await renderComposer();
    paste(
      textarea,
      transfer({
        files: [
          new File(["a"], "image.png", { type: "image/png" }),
          new File(["b"], "image.png", { type: "image/png" }),
        ],
      }),
    );
    expect(attachments.files().map((f) => f.name)).toEqual([
      "pasted-image-1.png",
      "pasted-image-2.png",
    ]);
  });
});

describe("pasting text", () => {
  it("leaves a small paste to the browser's own inline insertion", async () => {
    const { textarea, attachments } = await renderComposer();

    const event = paste(textarea, transfer({ text: "a short thought" }));

    expect(event.defaultPrevented).toBe(false);
    expect(attachments.files()).toEqual([]);
  });

  it("stages a large paste as a pasted-text ref described by line count", async () => {
    const { textarea, attachments, findByText } = await renderComposer();
    const big = "x".repeat(LARGE_PASTE_CHARS);

    const event = paste(textarea, transfer({ text: big }));

    expect(event.defaultPrevented).toBe(true);
    expect(attachments.files()).toHaveLength(1);
    const staged = attachments.files()[0];
    expect(staged.name).toBe("pasted-text-1.txt");
    expect(staged.kind).toBe("text");
    expect(staged.text).toBe(big);
    expect(await findByText("Pasted text · 1 lines")).toBeTruthy();
    expect(textarea.value).toBe("");
  });

  it("puts the paste back in the field when asked to insert it as text", async () => {
    const { textarea, attachments, findByLabelText } = await renderComposer();
    const big = "x".repeat(LARGE_PASTE_CHARS);
    paste(textarea, transfer({ text: big }));

    const insert = await findByLabelText('Insert "pasted-text-1.txt" as text');
    fireEvent.click(insert);

    expect(attachments.files()).toEqual([]);
    expect(textarea.value).toBe(big);
  });

  it("offers no insert-as-text action on an ordinary file chip", async () => {
    const { textarea, queryByLabelText, findByText } = await renderComposer();
    paste(textarea, transfer({ files: [new File(["x"], "a.png", { type: "image/png" })] }));
    await findByText("pasted-image-1.png");
    expect(queryByLabelText(/as text$/)).toBeNull();
  });
});

describe("drag and drop", () => {
  it("shows the drop target while files are over the window and stages the drop", async () => {
    const { container, attachments, findByText } = await renderComposer();
    const shell = container.querySelector('[data-slot="composer-shell"]') as HTMLElement;
    const data = transfer({ files: [new File(["x"], "notes.pdf", { type: "application/pdf" })] });

    fireEvent(window, Object.assign(new Event("dragenter", { bubbles: true }), { dataTransfer: data }));
    expect(shell.dataset.dropping).toBe("true");
    expect(await findByText(/Drop to attach/)).toBeTruthy();

    fireEvent(window, Object.assign(new Event("drop", { bubbles: true, cancelable: true }), { dataTransfer: data }));

    expect(shell.dataset.dropping).toBe("false");
    expect(attachments.files().map((f) => f.name)).toEqual(["notes.pdf"]);
    expect(attachments.files()[0].kind).toBe("file");
  });

  it("refuses a dropped folder with a stated reason and stages nothing", async () => {
    const { attachments } = await renderComposer();
    const { toasts } = await import("../../lib/toast");

    const data = transfer({ directories: ["src"] });
    fireEvent(window, Object.assign(new Event("drop", { bubbles: true, cancelable: true }), { dataTransfer: data }));

    expect(attachments.files()).toEqual([]);
    expect(toasts().map((t) => t.message)).toContain(
      "Folders can't be attached — drop the files inside",
    );
  });

  it("ignores a drag that carries no files", async () => {
    const { container } = await renderComposer();
    const shell = container.querySelector('[data-slot="composer-shell"]') as HTMLElement;
    const data = transfer({ text: "dragged selection" });
    fireEvent(window, Object.assign(new Event("dragenter", { bubbles: true }), { dataTransfer: data }));
    expect(shell.dataset.dropping).toBe("false");
  });
});

describe("host limits", () => {
  it("refuses a file past the Host's 15 MB blob ceiling at staging time", async () => {
    const { textarea, attachments } = await renderComposer();
    const huge = new File(["x"], "huge.bin", { type: "application/octet-stream" });
    Object.defineProperty(huge, "size", { value: 16 * 1024 * 1024 });

    paste(textarea, transfer({ files: [huge] }));

    const { toasts } = await import("../../lib/toast");
    expect(attachments.files()).toEqual([]);
    // The cap is stated, and stated before the send rather than after a
    // round-trip the Host would have rejected.
    expect(toasts().map((t) => t.message)).toContain('"huge.bin" is too large (max 15 MB)');
  });
});

describe("the upload lifecycle on the staged file", () => {
  /** Drive the real controller against a stubbed ws client, so the record under
   * test is the one the chips render from. */
  async function stagedWithClient(uploadBlob: (clientId: string) => Promise<Blob>) {
    vi.doMock("../../ws/client", async () => {
      const real = await vi.importActual<typeof import("../../ws/client")>("../../ws/client");
      return {
        ...real,
        getClient: () => ({ uploadBlob: (clientId: string) => uploadBlob(clientId) }),
      };
    });
    const rendered = await renderComposer();
    paste(rendered.textarea, transfer({ files: [new File(["x"], "a.png", { type: "image/png" })] }));
    return rendered;
  }

  const blobFor = (id: string): Blob => ({ id, name: "a.png", mime: "image/png", size: 1 });

  it("stages idle, ends done carrying the blob, and never re-uploads it", async () => {
    let calls = 0;
    const { attachments } = await stagedWithClient(async () => {
      calls += 1;
      return blobFor("blob-1");
    });
    expect(attachments.files()[0].upload).toEqual({ state: "idle" });

    expect(await attachments.uploadAll()).toEqual([blobFor("blob-1")]);
    // Done OWNS the blob: there is no done-without-a-blob to defend against.
    expect(attachments.files()[0].upload).toEqual({ state: "done", blob: blobFor("blob-1") });

    // A second send re-uses the resolved blob rather than uploading again.
    expect(await attachments.uploadAll()).toEqual([blobFor("blob-1")]);
    expect(calls).toBe(1);
  });

  it("records the failure reason once, and retry returns the chip to uploading", async () => {
    let fail = true;
    const { attachments, findByLabelText, findByText } = await stagedWithClient(async () => {
      if (fail) throw new Error("host said no");
      return blobFor("blob-2");
    });

    await expect(attachments.uploadAll()).rejects.toThrow();
    expect(attachments.files()[0].upload).toEqual({ state: "error", message: "host said no" });
    // The chip re-rendered off the record — reactivity survives the path write.
    expect(await findByText("Upload failed")).toBeTruthy();

    fail = false;
    const retry = await findByLabelText("Retry upload");
    fireEvent.click(retry);
    await vi.waitFor(() =>
      expect(attachments.files()[0].upload).toEqual({ state: "done", blob: blobFor("blob-2") }),
    );
  });
});
