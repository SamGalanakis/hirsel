import { createStore, untrack } from "solid-js";
import type { ServerMessage } from "../protocol";
import type { Artifact, ArtifactClientMessage } from "./types";

/** The focused Thread's persistent result has its own read request and content.
 * Temporary previews never select, clear, or consume this resource. */
const [showcaseState, updateShowcase] = createStore({
  history: null as string | null, threadId: null as number | null, artifactId: null as number | null,
  artifact: null as Artifact | null, loading: false, error: null as string | null,
});
export { showcaseState };
function setShowcaseState(patch: Partial<typeof showcaseState> | ((draft: typeof showcaseState) => void)) {
  updateShowcase(draft => { if (typeof patch === "function") patch(draft); else Object.assign(draft, patch); });
}
let transport: ((frame: ArtifactClientMessage) => void) | null = null;
let pending: { id: string; timer: ReturnType<typeof setTimeout> } | null = null;
function cancel() { if (pending) clearTimeout(pending.timer); pending = null; }
export function attachShowcaseTransport(send: (frame: ArtifactClientMessage) => void) { transport = send; }
export function disconnectShowcase() {
  transport = null; cancel();
  setShowcaseState(draft => { if (draft.loading) draft.error = "Reconnect to load the showcase."; draft.loading = false; });
}
export function resetShowcase() {
  cancel();
  setShowcaseState({ history: null, threadId: null, artifactId: null, artifact: null, loading: false, error: null });
}
export function selectShowcase(history: string, threadId: number, artifactId: number | null) {
  if (untrack(() => showcaseState.history === history && showcaseState.threadId === threadId && showcaseState.artifactId === artifactId)) return;
  resetShowcase();
  setShowcaseState({ history, threadId, artifactId });
  if (artifactId !== null) requestShowcase(artifactId);
}
export function refreshShowcase() {
  const id = untrack(() => showcaseState.artifactId);
  if (id !== null) requestShowcase(id);
}
function requestShowcase(id: number) {
  cancel();
  if (!transport) { setShowcaseState({ loading: false, error: "Reconnect to load the showcase." }); return; }
  const clientId = crypto.randomUUID();
  setShowcaseState({ loading: true, error: null });
  pending = { id: clientId, timer: setTimeout(() => {
    if (pending?.id !== clientId) return;
    cancel(); setShowcaseState({ loading: false, error: "The showcase request timed out. Try again." });
  }, 20_000) };
  transport({ type: "open_artifact", client_id: clientId, artifact_id: id });
}
export function handleShowcaseMessage(message: ServerMessage) {
  if (message.type === "hello_ok" && showcaseState.history === message.history_id) refreshShowcase();
  else if (message.type === "artifact_upsert" && message.artifact.id === showcaseState.artifactId) refreshShowcase();
  else if (message.type === "artifact_opened" && message.client_id === pending?.id) {
    cancel();
    if (message.artifact.id !== showcaseState.artifactId) { setShowcaseState({ loading: false, error: "The host returned a different artifact. Try again." }); return; }
    setShowcaseState({ artifact: message.artifact, loading: false, error: null });
  } else if (message.type === "error" && pending && message.client_id === pending.id) {
    cancel(); setShowcaseState({ loading: false, error: message.detail });
  }
}
