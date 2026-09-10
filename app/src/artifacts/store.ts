import { attachShowcaseTransport, disconnectShowcase, handleShowcaseMessage, resetShowcase } from "./showcase-store";
import { resetDraftArtifacts } from "./draft-context";
import { createStore } from "solid-js";
import type { ServerMessage } from "../protocol";
import type { Artifact, ArtifactClientMessage, ArtifactSummary } from "./types";

const [artifactState, updateArtifacts] = createStore({
  summaries: [] as ArtifactSummary[],
  opened: null as Artifact | null,
  selectedId: null as number | null,
  loading: false,
  error: null as string | null,
  listing: false,
  listed: false,
  listError: null as string | null,
});
export { artifactState };
export function setArtifactState(patch: Partial<typeof artifactState>) {
  updateArtifacts(draft => { Object.assign(draft, patch); });
}
let latestOpenRequest: string | null = null;
let latestListRequest: string | null = null;
let transport: ((frame: ArtifactClientMessage) => void) | null = null;
const requests = new Map<string, { artifactId?: number; timer: ReturnType<typeof setTimeout> }>();
function finish(id: string) {
  const request = requests.get(id);
  if (request) clearTimeout(request.timer);
  requests.delete(id);
  return request;
}
export function attachArtifactTransport(send: (frame: ArtifactClientMessage) => void) { transport = send; attachShowcaseTransport(send); }
export function disconnectArtifacts() {
  transport = null; disconnectShowcase();
  if (artifactState.selectedId !== null && artifactState.loading) setArtifactState({ error: "Reconnect to load this artifact." });
  for (const id of requests.keys()) finish(id);
  setArtifactState({ loading: false, listing: false, listError: "Reconnect to load artifacts." });
}
function requestFailed(artifactId: number | undefined, detail: string) {
  if (artifactId === undefined) setArtifactState({ listing: false, listError: detail });
  else if (artifactState.selectedId === artifactId) setArtifactState({ loading: false, error: detail });
}
function send(frame: ArtifactClientMessage, artifactId?: number) {
  if (!transport) { requestFailed(artifactId, "Reconnect to load artifacts."); return; }
  requests.set(frame.client_id, { artifactId, timer: setTimeout(() => {
    finish(frame.client_id);
    requestFailed(artifactId, "Artifact request timed out. Try again.");
  }, 20_000) });
  transport(frame);
}
export function listArtifacts() {
  if (latestListRequest) finish(latestListRequest);
  latestListRequest = crypto.randomUUID();
  setArtifactState({ listing: true, listError: null });
  send({ type: "list_artifacts", client_id: latestListRequest });
}
export function openArtifact(id: number) {
  setArtifactState({ selectedId: id, loading: true, error: null, opened: null });
  if (latestOpenRequest) finish(latestOpenRequest);
  latestOpenRequest = crypto.randomUUID();
  send({ type: "open_artifact", client_id: latestOpenRequest, artifact_id: id }, id);
}
export function closeArtifact() { setArtifactState({ selectedId: null, opened: null, loading: false, error: null }); }
function mergeSummary(rows: ArtifactSummary[], artifact: ArtifactSummary) {
  const prior = rows.find(row => row.id === artifact.id);
  if (prior) {
    if (compareTimestamps(prior.updated_at, artifact.updated_at) <= 0) Object.assign(prior, artifact);
  } else rows.push(artifact);
}
/** Compare RFC3339 instants without losing the protocol's nanosecond precision.
 * The lexical fallback keeps hand-built fixtures deterministic. */
function compareTimestamps(left: string, right: string): number {
  const instant = (value: string): bigint | null => {
    const match = /^(.*T\d{2}:\d{2}:\d{2})(?:\.(\d+))?(Z|[+-]\d{2}:\d{2})$/.exec(value);
    if (!match) return null;
    const milliseconds = Date.parse(match[1] + match[3]);
    if (!Number.isFinite(milliseconds)) return null;
    return BigInt(milliseconds) * 1_000_000n + BigInt((match[2] ?? "").padEnd(9, "0").slice(0, 9));
  };
  const leftInstant = instant(left), rightInstant = instant(right);
  if (leftInstant !== null && rightInstant !== null) return leftInstant < rightInstant ? -1 : leftInstant > rightInstant ? 1 : 0;
  return left.localeCompare(right);
}
function sortSummaries(rows: ArtifactSummary[]) {
  rows.sort((a, b) => compareTimestamps(b.updated_at, a.updated_at) || b.id - a.id);
}
function upsert(artifact: ArtifactSummary) {
  updateArtifacts(draft => { mergeSummary(draft.summaries, artifact); sortSummaries(draft.summaries); });
}
export function handleArtifactMessage(message: ServerMessage) {
  handleShowcaseMessage(message);
  updateArtifacts(() => {
  switch (message.type) {
    case "hello_ok": listArtifacts(); if (artifactState.selectedId !== null) openArtifact(artifactState.selectedId); break;
    case "artifacts_listed":
      if (!finish(message.client_id)) break;
      updateArtifacts(draft => {
        for (const artifact of message.artifacts) mergeSummary(draft.summaries, artifact);
        sortSummaries(draft.summaries);
        draft.listing = false; draft.listed = true; draft.listError = null;
      });
      break;
    case "artifact_opened": {
      const pending = finish(message.client_id);
      if (pending?.artifactId !== message.artifact.id || artifactState.selectedId !== message.artifact.id) break;
      upsert(message.artifact);
      setArtifactState({ opened: message.artifact, loading: false, error: null });
      break;
    }
    case "artifact_upsert":
      upsert(message.artifact);
      if (artifactState.selectedId === message.artifact.id) openArtifact(message.artifact.id);
      break;
    case "error":
      if (message.client_id) { const request = finish(message.client_id); if (request) requestFailed(request.artifactId, message.detail); }
      break;
  }
  });
}

export function resetArtifacts(): void {
  resetDraftArtifacts(); resetShowcase();
  disconnectArtifacts(); latestOpenRequest = null; latestListRequest = null;
  setArtifactState({ summaries: [], opened: null, selectedId: null, loading: false, error: null, listing: false, listed: false, listError: null });
}
