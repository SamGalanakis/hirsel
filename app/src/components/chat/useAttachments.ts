import { createSignal, onCleanup } from "solid-js";
import type { Blob } from "../../protocol";
import { dispatch, state } from "../../store/store";
import { getClient, makeClientId } from "../../ws/client";
import { fileToBase64 } from "../../lib/format";
import { toast } from "../../lib/toast";

const MAX_BYTES = 15 * 1024 * 1024; // 15 MB (protocol decoded-size cap)

/** A file staged in the composer before send. The raw File + preview object-URL
 * live here; the upload state machine (uploading/done/error + blob id) lives in
 * the global store keyed by the same `clientId`. */
export interface PendingFile {
  clientId: string;
  file: File;
  name: string;
  size: number;
  mime: string;
  previewUrl?: string; // object URL, images only
}

export interface AttachmentsController {
  files: () => PendingFile[];
  addFiles: (list: FileList | File[]) => void;
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

  function addFiles(list: FileList | File[]): void {
    const accepted: PendingFile[] = [];
    for (const file of Array.from(list)) {
      if (file.size > MAX_BYTES) {
        toast(`"${file.name}" is too large (max 15 MB)`, { variant: "error" });
        continue;
      }
      const mime = file.type || "application/octet-stream";
      const previewUrl = mime.startsWith("image/") ? URL.createObjectURL(file) : undefined;
      accepted.push({ clientId: makeClientId(), file, name: file.name, size: file.size, mime, previewUrl });
    }
    if (accepted.length > 0) setFiles((f) => [...f, ...accepted]);
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
    dispatch({ type: "uploads_clear" });
  }

  onCleanup(() => {
    for (const x of files()) if (x.previewUrl) URL.revokeObjectURL(x.previewUrl);
  });

  return { files, addFiles, removeFile, retry, clear, uploadAll };
}
