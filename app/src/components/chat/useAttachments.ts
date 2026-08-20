import { onCleanup } from "solid-js";
import { createStore, produce, reconcile } from "solid-js/store";
import type { Blob } from "../../protocol";
import { getClient, makeClientId } from "../../ws/client";
import { fileToBase64 } from "../../lib/format";
import { toast } from "../../lib/toast";
import {
  type AttachmentKind,
  attachmentKind,
  countLines,
  extractTransferFiles,
  MAX_ATTACHMENT_BYTES,
  namePastedFile,
  pastedTextFile,
} from "./paste";

/** Where one staged file is in its upload, as a closed set of states carrying
 * exactly what each state has. `done` OWNS the resolved blob and `error` owns
 * the reason, so "done with no blob" — previously representable across a
 * `{state, blobId?}` pair and defended at the one read site — cannot be spelled.
 * `idle` is a file staged but not yet sent. */
export type UploadStatus =
  | { state: "idle" }
  | { state: "uploading" }
  | { state: "done"; blob: Blob }
  | { state: "error"; message: string };

/** A file staged in the composer before send: the raw File, its preview
 * object-URL, and its upload lifecycle, all in ONE record. The lifecycle used to
 * live in a parallel `uploads` slice of the global store, joined back to this
 * one by `clientId` — two records for one thing, kept in step by six actions. */
export interface PendingFile {
  clientId: string;
  file: File;
  name: string;
  size: number;
  mime: string;
  kind: AttachmentKind;
  upload: UploadStatus;
  previewUrl?: string; // object URL, images only
  /** Original string for a chip made from a large paste. Retained so the paste
   * can be put back into the field ("Insert as text") — the escape hatch that
   * keeps auto-attaching from ever being a trap. */
  text?: string;
  /** Line count, for the "Pasted text · N lines" description. */
  lines?: number;
}

export interface AttachmentsController {
  files: () => PendingFile[];
  addFiles: (list: FileList | File[]) => void;
  /** Clipboard files: images get a `pasted-image-N.ext` name, since the
   * clipboard supplies either `image.png` or nothing at all. */
  addPastedFiles: (list: File[]) => void;
  /** Stage a large paste as a `pasted-text-N.txt` chip. */
  addPastedText: (text: string) => void;
  /** Files carried by a drop, refusing directories with a named reason. */
  addFromTransfer: (data: DataTransfer | null | undefined) => void;
  /** Remove a pasted-text chip and hand its text back for inline insertion.
   * Returns null when the chip is not a retained paste. */
  takeText: (clientId: string) => string | null;
  removeFile: (clientId: string) => void;
  retry: (clientId: string) => void;
  clear: () => void;
  /** Upload everything staged; resolves with blobs in order, throws if any
   * fail (leaving the failed chips in an error state for retry). */
  uploadAll: () => Promise<Blob[]>;
}

/** Owns the composer's staged attachments and the upload orchestration. Created
 * once per TaskShell so both the composer (paperclip / paste) and the drop
 * paste path feeds the same queue. */
export function createComposerAttachments(): AttachmentsController {
  // A store, not a signal: the chips render through `<For>`, which is keyed by
  // object reference, so replacing a row to record its upload state would tear
  // down and rebuild that chip's DOM (losing the focus that just clicked Retry).
  // A path write touches only `upload` and leaves the row itself alone.
  const [staged, setStaged] = createStore<{ files: PendingFile[] }>({ files: [] });
  const files = (): PendingFile[] => staged.files;

  // `reconcile`, not a bare object: a plain store write MERGES into the leaf,
  // which would leave `error`'s `message` clinging to the `done` that replaced
  // it — the union's whole point is that only one variant's fields exist at a
  // time. Reconcile replaces the record and drops the fields that left.
  function setUpload(clientId: string, upload: UploadStatus): void {
    setStaged("files", (f) => f.clientId === clientId, "upload", reconcile(upload));
  }

  // Per-composer counters so a message's pasted items read pasted-image-1,
  // pasted-image-2 … rather than colliding on one name.
  let pastedImages = 0;
  let pastedTexts = 0;

  /** Stage one file, or return the reason it cannot be staged. The size ceiling
   * is the Host's, checked here so an over-cap file is refused while the user
   * is still composing instead of failing the send. */
  function stage(file: File, extra?: Partial<PendingFile>): PendingFile | string {
    if (file.size > MAX_ATTACHMENT_BYTES) {
      return `"${file.name}" is too large (max 15 MB)`;
    }
    const mime = file.type || "application/octet-stream";
    const kind = attachmentKind(mime);
    return {
      clientId: makeClientId(),
      file,
      name: file.name,
      size: file.size,
      mime,
      kind,
      upload: { state: "idle" },
      previewUrl: kind === "image" ? URL.createObjectURL(file) : undefined,
      ...extra,
    };
  }

  function admit(staged: (PendingFile | string)[]): void {
    const accepted: PendingFile[] = [];
    for (const s of staged) {
      if (typeof s === "string") toast(s, { variant: "error" });
      else accepted.push(s);
    }
    if (accepted.length > 0) setStaged("files", (f) => [...f, ...accepted]);
  }

  function addFiles(list: FileList | File[]): void {
    admit(Array.from(list).map((file) => stage(file)));
  }

  function addPastedFiles(list: File[]): void {
    admit(
      list.map((file) => {
        const named = file.type.startsWith("image/")
          ? namePastedFile(file, ++pastedImages)
          : file;
        return stage(named);
      }),
    );
  }

  function addPastedText(text: string): void {
    const file = pastedTextFile(text, ++pastedTexts);
    admit([stage(file, { text, lines: countLines(text) })]);
  }

  function addFromTransfer(data: DataTransfer | null | undefined): void {
    const { files: dropped, rejected } = extractTransferFiles(data);
    for (const reason of new Set(rejected)) toast(reason, { variant: "error" });
    if (dropped.length > 0) addPastedFiles(dropped);
  }

  function takeText(clientId: string): string | null {
    const pf = files().find((x) => x.clientId === clientId);
    if (!pf?.text) return null;
    removeFile(clientId);
    return pf.text;
  }

  function removeFile(clientId: string): void {
    setStaged(
      "files",
      produce((f: PendingFile[]) => {
        const idx = f.findIndex((x) => x.clientId === clientId);
        if (idx === -1) return;
        if (f[idx].previewUrl) URL.revokeObjectURL(f[idx].previewUrl);
        f.splice(idx, 1);
      }),
    );
  }

  /** Run one file's upload, moving its `upload` through the lifecycle. The
   * error state is recorded HERE and only here: the ws client used to dispatch
   * its own `upload_error` on a correlated error frame, which is the same
   * failure this catch already sees (it rejects the very promise being awaited),
   * so the chip was marked failed twice. */
  async function runUpload(pf: PendingFile): Promise<Blob> {
    setUpload(pf.clientId, { state: "uploading" });
    try {
      const b64 = await fileToBase64(pf.file);
      const client = getClient();
      if (!client) throw new Error("not connected");
      const blob = await client.uploadBlob(pf.clientId, pf.name, pf.mime, b64);
      setUpload(pf.clientId, { state: "done", blob });
      return blob;
    } catch (e) {
      setUpload(pf.clientId, {
        state: "error",
        message: e instanceof Error ? e.message : "upload failed",
      });
      throw e;
    }
  }

  function retry(clientId: string): void {
    const pf = files().find((x) => x.clientId === clientId);
    if (!pf) return;
    void runUpload(pf).catch(() => {
      toast(`Couldn't upload "${pf.name}"`, { variant: "error" });
    });
  }

  async function uploadAll(): Promise<Blob[]> {
    const pfs = files();
    const results = await Promise.allSettled(
      // An already-uploaded file carries its blob, so a re-send after one chip
      // failed never re-uploads what already landed.
      pfs.map(async (pf) => (pf.upload.state === "done" ? pf.upload.blob : runUpload(pf))),
    );
    const blobs: Blob[] = [];
    let failed = false;
    for (const r of results) {
      if (r.status === "fulfilled") blobs.push(r.value);
      else failed = true;
    }
    if (failed) throw new Error("one or more uploads failed");
    return blobs;
  }

  function clear(): void {
    for (const x of files()) if (x.previewUrl) URL.revokeObjectURL(x.previewUrl);
    setStaged("files", []);
    // Numbering is per message, so the next one starts at pasted-image-1 again.
    pastedImages = 0;
    pastedTexts = 0;
  }

  onCleanup(() => {
    for (const x of files()) if (x.previewUrl) URL.revokeObjectURL(x.previewUrl);
  });

  return {
    files,
    addFiles,
    addPastedFiles,
    addPastedText,
    addFromTransfer,
    takeText,
    removeFile,
    retry,
    clear,
    uploadAll,
  };
}
