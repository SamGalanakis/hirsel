/** Pure classification helpers for what arrives on the composer via clipboard
 * or drag-and-drop. Kept free of Solid and of the store so the routing rules
 * (what counts as a "large" paste, what a dropped folder does) are unit-testable
 * without rendering a composer. */

/** Above either bound a paste is staged as an attachment instead of being typed
 * into the field. Peer clients sit far higher — ChatGPT converts at roughly
 * 10k characters — but their composers grow to fill the viewport. Hirsel's is a
 * single quiet capsule capped at 112px (DESIGN §4), so a paste that would take
 * more than a screenful of scrolling inside a four-line pill is already past the
 * point where leaving it inline helps anyone. */
export const LARGE_PASTE_CHARS = 2500;
export const LARGE_PASTE_LINES = 30;

/** Decoded-byte ceiling for one attachment. Mirrors the Host's
 * `MAX_BLOB_SIZE_BYTES` in crates/hirsel-host/src/attachments.rs — the Host
 * rejects anything larger, so the client refuses it at staging time rather than
 * letting a send fail after the upload round-trip. */
export const MAX_ATTACHMENT_BYTES = 15 * 1024 * 1024;

export function countLines(text: string): number {
  if (text.length === 0) return 0;
  return text.split("\n").length;
}

/** True when a pasted string should become an attachment chip rather than
 * inline text. Small pastes always stay inline. */
export function isLargePaste(text: string): boolean {
  return text.length >= LARGE_PASTE_CHARS || countLines(text) >= LARGE_PASTE_LINES;
}

/** `pasted-text-1.txt` — a real, named, downloadable file rather than an opaque
 * block, so the Agent sees it by path like any other attachment. */
export function pastedTextFile(text: string, index: number): File {
  return new File([text], `pasted-text-${index}.txt`, { type: "text/plain" });
}

const IMAGE_EXTENSIONS: Record<string, string> = {
  "image/png": "png",
  "image/jpeg": "jpg",
  "image/gif": "gif",
  "image/webp": "webp",
  "image/avif": "avif",
  "image/svg+xml": "svg",
  "image/bmp": "bmp",
  "image/tiff": "tiff",
};

export function pastedImageName(mime: string, index: number): string {
  return `pasted-image-${index}.${IMAGE_EXTENSIONS[mime] ?? "png"}`;
}

/** Clipboard images arrive as `image.png` or with no useful name at all;
 * everything else keeps the name it came with. */
export function namePastedFile(file: File, index: number): File {
  if (!file.type.startsWith("image/")) return file;
  return new File([file], pastedImageName(file.type, index), { type: file.type });
}

export type AttachmentKind = "image" | "text" | "file";

export function attachmentKind(mime: string): AttachmentKind {
  if (mime.startsWith("image/")) return "image";
  if (mime.startsWith("text/")) return "text";
  return "file";
}

export interface ExtractedTransfer {
  files: File[];
  /** Entries that cannot become an attachment, with the reason to surface. */
  rejected: string[];
}

interface EntryItem {
  kind: string;
  webkitGetAsEntry?: () => { isDirectory?: boolean } | null;
  getAsFile: () => File | null;
}

/** Files carried by a drop or paste. Directories are refused explicitly: the
 * File a browser hands back for a dropped folder is a zero-byte impostor that
 * would upload as an empty blob, so it is named as a rejection instead of
 * silently becoming garbage. */
export function extractTransferFiles(data: DataTransfer | null | undefined): ExtractedTransfer {
  const out: ExtractedTransfer = { files: [], rejected: [] };
  if (!data) return out;
  const items = data.items as unknown as EntryItem[] | undefined;
  if (items && items.length > 0) {
    for (const item of Array.from(items)) {
      if (item.kind !== "file") continue;
      const entry = item.webkitGetAsEntry?.();
      if (entry?.isDirectory) {
        out.rejected.push("Folders can't be attached — drop the files inside");
        continue;
      }
      const file = item.getAsFile();
      if (file) out.files.push(file);
    }
    return out;
  }
  for (const file of Array.from(data.files ?? [])) out.files.push(file);
  return out;
}
