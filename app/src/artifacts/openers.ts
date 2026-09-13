import type { ArtifactPresentationMode } from "./ArtifactPresentationMode";
import { stageDraftArtifact } from "./draft-context";
import { openArtifact } from "./store";
import { threadState } from "../threads/store";

/** Opening a result while a real Thread is selected offers it as context. */
export function stagePreviewContext(id: number, title: string): void {
  const threadId = threadState.focusedId;
  if (threadId !== null && threadState.threads.some(thread => thread.id === threadId)) stageDraftArtifact(threadId, { id, title });
}
/** The Preview and Source openers: one surface, two readings of it. */
export function previewArtifact(id: number, title: string, mode: ArtifactPresentationMode = "rendered"): void {
  stagePreviewContext(id, title);
  openArtifact(id, mode);
}
