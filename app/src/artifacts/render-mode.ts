import type { ArtifactSummary } from "./types";

/** The surface a result is drawn on. One mode per kind, decided here and
 * nowhere else: no reader inspects a MIME type or a filename to guess. */
export type ArtifactRenderMode = "solid" | "html" | "markdown" | "openui" | "image" | "text";

export function renderModeFor(artifact: Pick<ArtifactSummary, "kind">): ArtifactRenderMode {
  switch (artifact.kind) {
    case "solid": return "solid";
    case "html": return "html";
    case "markdown": return "markdown";
    case "openui": return "openui";
    case "image": return "image";
    case "file": return "text";
  }
}

/** How a result can be opened. Derived from the kind alone, in declaration
 * order: the first opener is what a plain click does. */
export type ArtifactOpenerId = "preview" | "source" | "download" | "showcase";
export interface ArtifactOpener {
  id: ArtifactOpenerId;
  label: string;
}
const OPENERS: Record<ArtifactOpenerId, string> = {
  preview: "Preview",
  source: "Source",
  download: "Download",
  showcase: "Showcase in this thread",
};
export function openersFor(artifact: Pick<ArtifactSummary, "kind">): ArtifactOpener[] {
  // A text result is already its own source, so it has no second reading of it.
  const ids: ArtifactOpenerId[] = renderModeFor(artifact) === "text"
    ? ["preview", "download", "showcase"]
    : ["preview", "source", "download", "showcase"];
  return ids.map(id => ({ id, label: OPENERS[id] }));
}
/** Whether the rendered form and the raw source are two different readings. */
export function hasArtifactSource(artifact: Pick<ArtifactSummary, "kind">): boolean {
  return openersFor(artifact).some(opener => opener.id === "source");
}

/** The download's type and name, derived from the kind rather than stored
 * beside it. Only the File variant brings its own name. */
export function downloadIdentity(summary: ArtifactSummary): { mime: string; filename: string } {
  switch (summary.kind) {
    case "solid": return { mime: "text/jsx", filename: `${summary.title}.jsx` };
    case "html": return { mime: "text/html", filename: `${summary.title}.html` };
    case "markdown": return { mime: "text/markdown", filename: `${summary.title}.md` };
    case "openui": return { mime: "text/x-openui", filename: `${summary.title}.openui` };
    case "image": return { mime: summary.mime, filename: `${summary.title}${imageExtension(summary.mime)}` };
    case "file": return { mime: summary.mime, filename: summary.filename ?? `${summary.title}.txt` };
  }
}
function imageExtension(mime: string): string {
  const known: Record<string, string> = {
    "image/svg+xml": ".svg",
    "image/png": ".png",
    "image/jpeg": ".jpg",
    "image/webp": ".webp",
    "image/gif": ".gif",
  };
  return known[mime.toLowerCase()] ?? "";
}

/** The label a card shows under the title: a file's own name, else its mode. */
export function artifactCaption(artifact: ArtifactSummary): string {
  return artifact.kind === "file" && artifact.filename ? artifact.filename : renderModeFor(artifact);
}
