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
export function attachArtifactTransport(send: (frame: ArtifactClientMessage) => void) { transport = send; }
export function disconnectArtifacts() {
  transport = null;
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
    const thread_ids = [...new Set([...prior.thread_ids, ...artifact.thread_ids])];
    if (prior.updated_at <= artifact.updated_at) Object.assign(prior, artifact);
    prior.thread_ids = thread_ids;
  } else rows.push(artifact);
}
function sortSummaries(rows: ArtifactSummary[]) {
  rows.sort((a, b) => b.updated_at.localeCompare(a.updated_at) || b.id - a.id);
}
function upsert(artifact: ArtifactSummary) {
  updateArtifacts(draft => { mergeSummary(draft.summaries, artifact); sortSummaries(draft.summaries); });
}
export function handleArtifactMessage(message: ServerMessage) {
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
}
