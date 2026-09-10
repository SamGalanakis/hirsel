import { cleanup, fireEvent, render, within, screen } from "@solidjs/testing-library";
import { flush } from "solid-js";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { dispatch } from "../store/store";
import { setHistoryId } from "../lib/history";
import { RelatedContext } from "../related/context";
import { makeThread } from "../threads/fixtures";
import { attachThreadTransport, disconnectThreads, setThreadState } from "../threads/store";
import { ArtifactCard, ArtifactSurface } from "./ArtifactSurface";
import { ShowcaseSurface } from "./ShowcaseSurface";
import { artifactState, attachArtifactTransport, closeArtifact, disconnectArtifacts, handleArtifactMessage, listArtifacts, openArtifact, resetArtifacts, setArtifactState } from "./store";
import { refreshShowcase, showcaseState, selectShowcase } from "./showcase-store";
import { captureShowcaseOrigin, setPhoneShowcase, setShowcasePicker, setThreadShowcase } from "./showcase-actions";
import type { Artifact, ArtifactClientMessage } from "./types";
import type { ThreadClientMessage } from "../threads/types";
vi.mock("./ArtifactPreview", () => ({ ArtifactPreview: (props: { artifact: Artifact }) => <div data-preview={props.artifact.id}>{props.artifact.content}</div> }));
const artifact: Artifact = { id: 4, title: "Architecture", kind: "file", mime: "text/plain", content: "Plan content", thread_ids: [1], created_at: "a", updated_at: "b" };
const frames: ArtifactClientMessage[] = []; const actions: ThreadClientMessage[] = [];
beforeEach(() => {
  frames.length = 0; actions.length = 0;
  flush(() => { resetArtifacts(); setHistoryId("history-a"); setPhoneShowcase(null); setShowcasePicker(null); dispatch({ type: "connection_status", status: "connected" }); setThreadState(draft => Object.assign(draft, { ready: true, threads: [makeThread(1, { title: "Garden" }), makeThread(2)], focusedId: 1, error: null })); setArtifactState({ summaries: [artifact, { ...artifact, id: 5, title: "Checklist" }] }); });
  attachArtifactTransport(frame => frames.push(frame)); attachThreadTransport(frame => actions.push(frame));
});
afterEach(() => { cleanup(); resetArtifacts(); disconnectThreads(); });
function reply(id: number, content: string) { const frame = frames.filter(frame => frame.type === "open_artifact" && frame.artifact_id === id).at(-1)!; flush(() => handleArtifactMessage({ type: "artifact_opened", client_id: frame.client_id, artifact: { ...artifact, id, content } })); }
describe("persistent Thread showcase", () => {
  it("promotes a specific card with its captured origin and no About staging or preview", async () => {
    const view = render(() => <RelatedContext value={{ historyId: "history-a", threadId: 1 }}><ArtifactCard id={4} /></RelatedContext>);
    fireEvent.click(view.getByRole("button", { name: "Artifact actions" }));
    flush(() => setThreadState(draft => { draft.focusedId = 2; }));
    fireEvent.click(await screen.findByRole("menuitem", { name: "Showcase in this thread" }));
    expect(actions).toContainEqual({ type: "thread_action", thread_id: 1, action: "set_showcase", expected_revision: 1, data: { artifact_id: 4, history_id: "history-a" } });
    expect(artifactState.selectedId).toBeNull();
  });
  it("rejects stale menu revisions and reused IDs in a new history", async () => {
    const view = render(() => <RelatedContext value={{ historyId: "history-a", threadId: 1 }}><ArtifactCard id={4} /></RelatedContext>);
    fireEvent.click(view.getByRole("button", { name: "Artifact actions" }));
    flush(() => setThreadState(draft => { draft.threads[0].revision = 2; }));
    fireEvent.click(await screen.findByRole("menuitem", { name: "Showcase in this thread" }));
    expect(view.getByRole("alert")).toHaveTextContent("thread changed"); expect(actions).toEqual([]);
    const target = captureShowcaseOrigin({ historyId: "history-a", threadId: 1 })!;
    flush(() => setHistoryId("history-b"));
    expect(() => setThreadShowcase(target, 4)).toThrow(/history/); expect(actions).toEqual([]);
  });
  it("keeps showcase content separate from temporary preview and follows the persisted pointer", () => {
    flush(() => setThreadState(draft => { draft.threads[0].showcased_artifact_id = 4; }));
    const view = render(() => <><ShowcaseSurface /><ArtifactSurface /></>); flush(); reply(4, "Persistent result");
    const pane = view.getByRole("complementary", { name: "Thread showcase" });
    expect(pane).toHaveTextContent("Persistent result");
    flush(() => openArtifact(5)); reply(5, "Temporary result");
    expect(view.queryByRole("complementary", { name: "Thread showcase" })).toBeNull();
    expect(showcaseState.artifact?.content).toBe("Persistent result");
    flush(closeArtifact); expect(view.getByRole("complementary", { name: "Thread showcase" })).toBe(pane);
    flush(() => setThreadState(draft => { draft.focusedId = 2; }));
    expect(view.queryByRole("complementary", { name: "Thread showcase" })).toBeNull();
    expect(showcaseState.artifact).toBeNull();
  });
  it("replaces and removes through explicit revision-checked controls", async () => {
    flush(() => setThreadState(draft => { draft.threads[0].showcased_artifact_id = 4; }));
    const view = render(() => <ShowcaseSurface />); flush(); reply(4, "result");
    fireEvent.click(view.getByRole("button", { name: "Showcase actions" }));
    fireEvent.click(await screen.findByRole("menuitem", { name: "Replace showcase" }));
    const picker = view.getByRole("dialog", { name: "Choose showcase" });
    fireEvent.click(within(picker).getByRole("button", { name: "Choose Checklist as showcase" }));
    expect(actions.at(-1)).toMatchObject({ thread_id: 1, action: "set_showcase", expected_revision: 1, data: { artifact_id: 5, history_id: "history-a" } });
    fireEvent.click(view.getByRole("button", { name: "Showcase actions" }));
    fireEvent.click(await screen.findByRole("menuitem", { name: "Remove showcase" }));
    expect(actions.at(-1)).toMatchObject({ data: { artifact_id: null, history_id: "history-a" } });
  });
  it("ignores stale reads after thread/history changes and unrelated host errors", () => {
    flush(() => selectShowcase("history-a", 1, 4)); const old = frames.at(-1)!;
    flush(() => selectShowcase("history-b", 1, 5));
    flush(() => handleArtifactMessage({ type: "artifact_opened", client_id: old.client_id, artifact }));
    expect(showcaseState.artifact).toBeNull(); reply(5, "new history");
    flush(() => handleArtifactMessage({ type: "error", detail: "unrelated" }));
    expect(showcaseState.error).toBeNull(); expect(showcaseState.artifact?.content).toBe("new history");
  });
  it("recovers the showcase after a disconnect without accepting the abandoned read", () => {
    flush(() => selectShowcase("history-a", 1, 4)); const abandoned = frames.at(-1)!;
    flush(() => disconnectArtifacts());
    expect(showcaseState.loading).toBe(false); expect(showcaseState.error).toMatch(/Reconnect/);
    flush(() => handleArtifactMessage({ type: "artifact_opened", client_id: abandoned.client_id, artifact }));
    expect(showcaseState.artifact).toBeNull();
    attachArtifactTransport(frame => frames.push(frame)); flush(refreshShowcase); reply(4, "reconnected");
    expect(showcaseState.artifact?.content).toBe("reconnected"); expect(showcaseState.error).toBeNull();
  });
  it("removes obsolete showcase-only backlinks using the newest summary", () => {
    const original = { ...artifact, updated_at: "2026-09-10T10:00:00Z" };
    flush(() => { setArtifactState({ summaries: [original] }); handleArtifactMessage({ type: "artifact_upsert", artifact: { ...original, updated_at: "2026-09-10T10:00:00.000000001Z", thread_ids: [] } }); });
    expect(artifactState.summaries.find(row => row.id === 4)?.thread_ids).toEqual([]);
    flush(() => handleArtifactMessage({ type: "artifact_upsert", artifact: original }));
    expect(artifactState.summaries.find(row => row.id === 4)?.thread_ids).toEqual([]);
  });
  it("sorts RFC3339 timestamps by exact instant across offsets and nanoseconds", () => {
    const rows = [
      { ...artifact, id: 4, updated_at: "2026-09-10T10:00:00Z" },
      { ...artifact, id: 5, updated_at: "2026-09-10T12:00:00.000000001+02:00" },
      { ...artifact, id: 6, updated_at: "2026-09-10T09:59:59.999999999Z" },
    ];
    flush(() => { setArtifactState({ summaries: [] }); listArtifacts(); handleArtifactMessage({ type: "artifacts_listed", client_id: frames.at(-1)!.client_id, artifacts: rows }); });
    expect(artifactState.summaries.map(row => row.id)).toEqual([5, 4, 6]);
  });
});
