import { createSignal, onCleanup } from "solid-js";
import type { Blob } from "../../protocol";
import { dispatch, state } from "../../store/store";
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

/** A file staged in the composer before send. The raw File + preview object-URL
 * live here; the upload state machine (uploading/done/error + blob id) lives in
 * the global store keyed by the same `clientId`. */
export interface PendingFile {
  clientId: string;
  file: File;
  name: string;
  size: number;
  mime: string;
  kind: AttachmentKind;
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
  const [files, setFiles] = createSignal<PendingFile[]>([]);
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
    if (accepted.length > 0) setFiles((f) => [...f, ...accepted]);
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
    setFiles((f) =>
      f.filter((x) => {
        if (x.clientId === clientId && x.previewUrl) URL.revokeObjectURL(x.previewUrl);
        return x.clientId !== clientId;
      }),
    );
    dispatch({ type: "upload_remove", clientId });
  }

  async function runUpload(pf: PendingFile): Promise<Blob> {
    dispatch({
      type: "upload_start",
      clientId: pf.clientId,
      name: pf.name,
      size: pf.size,
      mime: pf.mime,
    });
    try {
      const b64 = await fileToBase64(pf.file);
      const client = getClient();
      if (!client) throw new Error("not connected");
      return await client.uploadBlob(pf.clientId, pf.name, pf.mime, b64);
    } catch (e) {
      dispatch({ type: "upload_error", clientId: pf.clientId });
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
      pfs.map(async (pf) => {
        const existing = state.uploads.find((u) => u.clientId === pf.clientId);
        if (existing?.state === "done" && existing.blobId) {
          return { id: existing.blobId, name: pf.name, mime: pf.mime, size: pf.size } satisfies Blob;
        }
        return runUpload(pf);
      }),
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
    setFiles([]);
    // Numbering is per message, so the next one starts at pasted-image-1 again.
    pastedImages = 0;
    pastedTexts = 0;
    dispatch({ type: "uploads_clear" });
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
