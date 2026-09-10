import { createStore } from "solid-js";
import { historyId } from "../lib/history";
export interface DraftArtifact { id: number; title: string }
const PREFIX = "hirsel.artifact-context.";
const [contexts, update] = createStore<Record<string, DraftArtifact | null>>({});
const keyFor = (threadId: number) => historyId() ? `${PREFIX}${historyId()}:thread-${threadId}` : null;
function read(key: string): DraftArtifact | null {
  try {
    const value: unknown = JSON.parse(localStorage.getItem(key) ?? "null");
    if (value && typeof value === "object" && "id" in value && typeof value.id === "number" && Number.isSafeInteger(value.id) && value.id >= 0 && "title" in value && typeof value.title === "string") return { id: value.id, title: value.title };
  } catch { /* Storage may be unavailable; the in-memory draft still works. */ }
  return null;
}
export function draftArtifact(threadId: number): DraftArtifact | null {
  const key = keyFor(threadId);
  return key ? contexts[key] === undefined ? read(key) : contexts[key] : null;
}
export function stageDraftArtifact(threadId: number, artifact: DraftArtifact | null): void {
  const key = keyFor(threadId); if (!key) return;
  update(draft => { draft[key] = artifact; });
  try { if (artifact) localStorage.setItem(key, JSON.stringify(artifact)); else localStorage.removeItem(key); } catch { /* In-memory context is authoritative while storage is unavailable. */ }
}
/** Consume only the submitted context; a replacement staged during upload stays. */
export function consumeDraftArtifact(threadId: number, id: number): void {
  if (draftArtifact(threadId)?.id === id) stageDraftArtifact(threadId, null);
}
export function resetDraftArtifacts(): void {
  update(draft => { for (const key of Object.keys(draft)) delete draft[key]; });
  try {
    const keys = Array.from({ length: localStorage.length }, (_, index) => localStorage.key(index));
    for (const key of keys) if (key?.startsWith(PREFIX)) localStorage.removeItem(key);
  } catch { /* Reset always clears memory, even without persistent storage. */ }
}
