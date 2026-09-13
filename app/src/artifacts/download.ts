import { downloadIdentity } from "./render-mode";
import type { Artifact } from "./types";
/** Owner download of the original content, outside the isolated preview. */
export function downloadArtifact(artifact: Artifact): void {
  const identity = downloadIdentity(artifact);
  const url = URL.createObjectURL(new Blob([artifact.content], { type: identity.mime }));
  const link = document.createElement("a");
  link.href = url;
  link.download = identity.filename;
  link.click();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}
