import type { Artifact } from "./types";
/** Owner download of the original content, outside the isolated preview. */
export function downloadArtifact(artifact: Artifact): void {
  const url = URL.createObjectURL(new Blob([artifact.content], { type: artifact.mime }));
  const link = document.createElement("a");
  link.href = url;
  link.download = artifact.filename ?? `${artifact.title}.${artifact.kind === "solid" ? "jsx" : artifact.kind === "html" ? "html" : "txt"}`;
  link.click();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}
