import { describe, expect, it } from "vitest";
import {
  attachmentKind,
  countLines,
  extractTransferFiles,
  isLargePaste,
  LARGE_PASTE_CHARS,
  LARGE_PASTE_LINES,
  namePastedFile,
  pastedImageName,
  pastedTextFile,
} from "./paste";

describe("large-paste threshold", () => {
  it("keeps an ordinary paste inline", () => {
    expect(isLargePaste("a quick note")).toBe(false);
    expect(isLargePaste("")).toBe(false);
  });

  it("routes a paste past the character bound to an attachment", () => {
    expect(isLargePaste("x".repeat(LARGE_PASTE_CHARS - 1))).toBe(false);
    expect(isLargePaste("x".repeat(LARGE_PASTE_CHARS))).toBe(true);
  });

  it("routes a many-lined paste to an attachment even when it is short", () => {
    // A stack trace: tiny in characters, unreadable in a four-line pill.
    const shortButTall = Array.from({ length: LARGE_PASTE_LINES }, () => "at f").join("\n");
    expect(shortButTall.length).toBeLessThan(LARGE_PASTE_CHARS);
    expect(isLargePaste(shortButTall)).toBe(true);
    expect(isLargePaste(Array.from({ length: LARGE_PASTE_LINES - 1 }, () => "at f").join("\n"))).toBe(
      false,
    );
  });

  it("counts lines without inventing one for empty text", () => {
    expect(countLines("")).toBe(0);
    expect(countLines("one")).toBe(1);
    expect(countLines("one\ntwo")).toBe(2);
  });
});

describe("naming", () => {
  it("names a pasted text file by its index and carries its bytes", () => {
    const file = pastedTextFile("hello", 2);
    expect(file.name).toBe("pasted-text-2.txt");
    expect(file.type).toBe("text/plain");
    expect(file.size).toBe(5);
  });

  it("maps image mimes to their extension and falls back to png", () => {
    expect(pastedImageName("image/png", 1)).toBe("pasted-image-1.png");
    expect(pastedImageName("image/jpeg", 3)).toBe("pasted-image-3.jpg");
    expect(pastedImageName("image/heic", 1)).toBe("pasted-image-1.png");
  });

  it("renames pasted images but leaves other files' names alone", () => {
    const img = new File(["x"], "image.png", { type: "image/png" });
    expect(namePastedFile(img, 1).name).toBe("pasted-image-1.png");
    const doc = new File(["x"], "report.pdf", { type: "application/pdf" });
    expect(namePastedFile(doc, 1).name).toBe("report.pdf");
  });

  it("classifies chips by mime", () => {
    expect(attachmentKind("image/png")).toBe("image");
    expect(attachmentKind("text/plain")).toBe("text");
    expect(attachmentKind("application/octet-stream")).toBe("file");
  });
});

/** Minimal stand-in for the parts of DataTransfer the extractor reads; jsdom
 * has no constructible DataTransfer with real items. */
function transfer(
  items: { kind: string; file?: File; directory?: boolean }[],
): DataTransfer {
  return {
    items: items.map((i) => ({
      kind: i.kind,
      webkitGetAsEntry: () => (i.directory ? { isDirectory: true } : { isDirectory: false }),
      getAsFile: () => i.file ?? null,
    })),
    files: [],
  } as unknown as DataTransfer;
}

describe("extractTransferFiles", () => {
  it("takes file items and ignores string items", () => {
    const png = new File(["x"], "a.png", { type: "image/png" });
    const { files, rejected } = extractTransferFiles(
      transfer([{ kind: "string" }, { kind: "file", file: png }]),
    );
    expect(files.map((f) => f.name)).toEqual(["a.png"]);
    expect(rejected).toEqual([]);
  });

  it("rejects dropped folders with a stated reason rather than an empty blob", () => {
    const { files, rejected } = extractTransferFiles(
      transfer([{ kind: "file", directory: true, file: new File([], "src") }]),
    );
    expect(files).toEqual([]);
    expect(rejected).toEqual(["Folders can't be attached — drop the files inside"]);
  });

  it("returns nothing for an absent transfer", () => {
    expect(extractTransferFiles(null)).toEqual({ files: [], rejected: [] });
  });
});
