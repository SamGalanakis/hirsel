import { flush } from "solid-js";
import { fireEvent, render } from "@solidjs/testing-library";
import { createRoot } from "solid-js";

import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Blob } from "../../protocol";

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
  flush(() => el.dispatchEvent(event));
  return event;
}

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
