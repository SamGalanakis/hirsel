import { attachShowcaseTransport, disconnectShowcase, handleShowcaseMessage, resetShowcase } from "./showcase-store";
import { resetDraftArtifacts } from "./draft-context";
import { createStore } from "solid-js";
import type { ServerMessage } from "../protocol";
import type { Artifact, ArtifactClientMessage, ArtifactSummary } from "./types";

/** The preview is one lifecycle, so it is one value: every writer replaces the
 * whole variant and the surface renders exactly one branch. The selected id is
 * carried by the variant rather than stored beside it, so a dropped connection
 * cannot leave a selection with nothing to render. */
export type ArtifactPreview =
  | { status: "idle" }
  | { status: "loading"; id: number }
  | { status: "ready"; id: number; artifact: Artifact }
  | { status: "error"; id: number; message: string };
/** The inventory lifecycle, likewise: a spinner and a retry banner are two
 * variants of one value and can never render together. `ready` is what tells
 * the empty state apart from "not fetched yet". */
export type ArtifactInventory =
  | { status: "idle" }
  | { status: "loading" }
  | { status: "ready" }
  | { status: "error"; message: string };
export interface ArtifactState {
  summaries: ArtifactSummary[];
  preview: ArtifactPreview;
  inventory: ArtifactInventory;
}

const [artifactState, updateArtifacts] = createStore<ArtifactState>({
  summaries: [],
  preview: { status: "idle" },
  inventory: { status: "idle" },
});
export { artifactState };
export function setArtifactState(patch: Partial<ArtifactState>) {
  updateArtifacts(draft => { Object.assign(draft, patch); });
}
/** The artifact the preview is about, in every non-idle variant. */
export function previewedArtifactId(): number | null {
  return artifactState.preview.status === "idle" ? null : artifactState.preview.id;
}
/** The inventory’s failure message, when that is the variant it is in. */
export function inventoryError(): string | null {
  return artifactState.inventory.status === "error" ? artifactState.inventory.message : null;
}
/** The loaded artifact, or null while loading, failed or closed. */
export function openedArtifact(): Artifact | null {
  return artifactState.preview.status === "ready" ? artifactState.preview.artifact : null;
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
  for (const id of requests.keys()) finish(id);
  const preview = artifactState.preview;
  if (preview.status === "loading") setArtifactState({ preview: { status: "error", id: preview.id, message: "Reconnect to load this artifact." } });
  setArtifactState({ inventory: { status: "error", message: "Reconnect to load artifacts." } });
}
function requestFailed(artifactId: number | undefined, detail: string) {
  if (artifactId === undefined) setArtifactState({ inventory: { status: "error", message: detail } });
  else if (previewedArtifactId() === artifactId) setArtifactState({ preview: { status: "error", id: artifactId, message: detail } });
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
  setArtifactState({ inventory: { status: "loading" } });
  send({ type: "list_artifacts", client_id: latestListRequest });
}
export function openArtifact(id: number) {
  setArtifactState({ preview: { status: "loading", id } });
  if (latestOpenRequest) finish(latestOpenRequest);
  latestOpenRequest = crypto.randomUUID();
  send({ type: "open_artifact", client_id: latestOpenRequest, artifact_id: id }, id);
}
export function closeArtifact() { setArtifactState({ preview: { status: "idle" } }); }
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
    case "hello_ok": {
      listArtifacts();
      const selected = previewedArtifactId();
      if (selected !== null) openArtifact(selected);
      break;
    }
    case "artifacts_listed":
      if (!finish(message.client_id)) break;
      updateArtifacts(draft => {
        for (const artifact of message.artifacts) mergeSummary(draft.summaries, artifact);
        sortSummaries(draft.summaries);
        draft.inventory = { status: "ready" };
      });
      break;
    case "artifact_opened": {
      const pending = finish(message.client_id);
      if (pending?.artifactId !== message.artifact.id || previewedArtifactId() !== message.artifact.id) break;
      upsert(message.artifact);
      setArtifactState({ preview: { status: "ready", id: message.artifact.id, artifact: message.artifact } });
      break;
    }
    case "artifact_upsert":
      upsert(message.artifact);
      if (previewedArtifactId() === message.artifact.id) openArtifact(message.artifact.id);
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
  setArtifactState({ summaries: [], preview: { status: "idle" }, inventory: { status: "idle" } });
}
