import { attachShowcaseTransport, disconnectShowcase, handleShowcaseMessage, resetShowcase } from "./showcase-store";
import { resetDraftArtifacts } from "./draft-context";
import { downloadArtifact } from "./download";
import type { ArtifactPresentationMode } from "./ArtifactPresentationMode";
import { createSignal, createStore } from "solid-js";
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
/** A request is either the inventory, the preview, or a direct download; the
 * record says which so a failure lands on the surface that asked for it. */
interface ArtifactRequest { artifactId?: number; download?: boolean; timer: ReturnType<typeof setTimeout> }
const requests = new Map<string, ArtifactRequest>();
/** Which reading of the opened artifact the preview shows. A view state: the
 * artifact itself is never rewritten to change how it is displayed. */
export const [previewMode, setPreviewMode] = createSignal<ArtifactPresentationMode>("rendered");
/** A failure from an opener that has no surface of its own yet. */
export const [openerError, setOpenerError] = createSignal<{ id: number; message: string } | null>(null);
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
function requestFailed(request: Pick<ArtifactRequest, "artifactId" | "download">, detail: string) {
  const { artifactId, download } = request;
  if (artifactId === undefined) setArtifactState({ inventory: { status: "error", message: detail } });
  else if (download) setOpenerError({ id: artifactId, message: detail });
  else if (previewedArtifactId() === artifactId) setArtifactState({ preview: { status: "error", id: artifactId, message: detail } });
}
function send(frame: ArtifactClientMessage, pending: Pick<ArtifactRequest, "artifactId" | "download"> = {}) {
  if (!transport) { requestFailed(pending, "Reconnect to load artifacts."); return; }
  requests.set(frame.client_id, { ...pending, timer: setTimeout(() => {
    finish(frame.client_id);
    requestFailed(pending, "Artifact request timed out. Try again.");
  }, 20_000) });
  transport(frame);
}
export function listArtifacts() {
  if (latestListRequest) finish(latestListRequest);
  latestListRequest = crypto.randomUUID();
  setArtifactState({ inventory: { status: "loading" } });
  send({ type: "list_artifacts", client_id: latestListRequest });
}
/** Opening with an explicit mode is how a chosen opener reaches the preview;
 * a refresh omits it and keeps whatever reading the Owner selected. */
export function openArtifact(id: number, mode?: ArtifactPresentationMode) {
  if (mode) setPreviewMode(mode);
  setArtifactState({ preview: { status: "loading", id } });
  if (latestOpenRequest) finish(latestOpenRequest);
  latestOpenRequest = crypto.randomUUID();
  send({ type: "open_artifact", client_id: latestOpenRequest, artifact_id: id }, { artifactId: id });
}
/** The Download opener from a list row, where only the summary is loaded. */
export function downloadArtifactById(id: number) {
  setOpenerError(null);
  send({ type: "open_artifact", client_id: crypto.randomUUID(), artifact_id: id }, { artifactId: id, download: true });
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
      if (pending?.artifactId !== message.artifact.id) break;
      upsert(message.artifact);
      if (pending.download) { downloadArtifact(message.artifact); break; }
      if (previewedArtifactId() !== message.artifact.id) break;
      setArtifactState({ preview: { status: "ready", id: message.artifact.id, artifact: message.artifact } });
      break;
    }
    case "artifact_upsert":
      upsert(message.artifact);
      if (previewedArtifactId() === message.artifact.id) openArtifact(message.artifact.id);
      break;
    case "error":
      if (message.client_id) { const request = finish(message.client_id); if (request) requestFailed(request, message.detail); }
      break;
  }
  });
}

export function resetArtifacts(): void {
  resetDraftArtifacts(); resetShowcase();
  disconnectArtifacts(); latestOpenRequest = null; latestListRequest = null;
  setPreviewMode("rendered"); setOpenerError(null);
  setArtifactState({ summaries: [], preview: { status: "idle" }, inventory: { status: "idle" } });
}
