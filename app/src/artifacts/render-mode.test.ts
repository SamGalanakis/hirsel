import { describe, expect, it } from "vitest";
import { artifactCaption, downloadIdentity, hasArtifactSource, openersFor, renderModeFor } from "./render-mode";
import type { ArtifactKind, ArtifactSummary } from "./types";

const summary = (kind: ArtifactKind): ArtifactSummary =>
  ({ id: 1, revision: 1, title: "Result", thread_ids: [], created_at: "a", updated_at: "b", ...kind });

describe("one render discriminator", () => {
  it("maps every kind onto exactly one surface", () => {
    const table: [ArtifactKind, string][] = [
      [{ kind: "solid" }, "solid"],
      [{ kind: "html" }, "html"],
      [{ kind: "markdown" }, "markdown"],
      [{ kind: "openui" }, "openui"],
      [{ kind: "image", mime: "image/svg+xml" }, "image"],
      [{ kind: "image", mime: "image/png" }, "image"],
      [{ kind: "file", mime: "text/plain", filename: "notes.txt" }, "text"],
      [{ kind: "file", mime: "application/json", filename: null }, "text"],
    ];
    for (const [kind, mode] of table) expect(renderModeFor(summary(kind))).toBe(mode);
  });
  it("lists the applicable openers in order, preview first", () => {
    const ids = (kind: ArtifactKind) => openersFor(summary(kind)).map(opener => opener.id);
    expect(ids({ kind: "solid" })).toEqual(["preview", "source", "download", "showcase"]);
    expect(ids({ kind: "html" })).toEqual(["preview", "source", "download", "showcase"]);
    expect(ids({ kind: "markdown" })).toEqual(["preview", "source", "download", "showcase"]);
    expect(ids({ kind: "openui" })).toEqual(["preview", "source", "download", "showcase"]);
    expect(ids({ kind: "image", mime: "image/svg+xml" })).toEqual(["preview", "source", "download", "showcase"]);
    // Plain text is already its own source, so it gets no duplicate reading.
    expect(ids({ kind: "file", mime: "text/plain", filename: null })).toEqual(["preview", "download", "showcase"]);
    expect(hasArtifactSource(summary({ kind: "file", mime: "text/plain", filename: null }))).toBe(false);
    expect(hasArtifactSource(summary({ kind: "markdown" }))).toBe(true);
    expect(openersFor(summary({ kind: "html" }))[0].label).toBe("Preview");
  });
  it("derives download identity and the caption from the kind alone", () => {
    expect(downloadIdentity(summary({ kind: "solid" }))).toEqual({ mime: "text/jsx", filename: "Result.jsx" });
    expect(downloadIdentity(summary({ kind: "markdown" }))).toEqual({ mime: "text/markdown", filename: "Result.md" });
    expect(downloadIdentity(summary({ kind: "openui" }))).toEqual({ mime: "text/x-openui", filename: "Result.openui" });
    expect(downloadIdentity(summary({ kind: "image", mime: "image/png" }))).toEqual({ mime: "image/png", filename: "Result.png" });
    expect(downloadIdentity(summary({ kind: "file", mime: "text/plain", filename: "notes.txt" }))).toEqual({ mime: "text/plain", filename: "notes.txt" });
    expect(downloadIdentity(summary({ kind: "file", mime: "text/plain", filename: null }))).toEqual({ mime: "text/plain", filename: "Result.txt" });
    expect(artifactCaption(summary({ kind: "file", mime: "text/plain", filename: "notes.txt" }))).toBe("notes.txt");
    expect(artifactCaption(summary({ kind: "file", mime: "text/plain", filename: null }))).toBe("text");
    expect(artifactCaption(summary({ kind: "markdown" }))).toBe("markdown");
  });
});
