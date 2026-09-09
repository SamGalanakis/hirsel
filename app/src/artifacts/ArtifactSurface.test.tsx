import { cleanup, fireEvent, render, screen } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";
import { flush } from "solid-js";
import { ArtifactCard, ArtifactList } from "./ArtifactSurface";
import { artifactState, attachArtifactTransport, closeArtifact, disconnectArtifacts, setArtifactState, handleArtifactMessage, listArtifacts } from "./store";
import { setThreadState, threadState } from "../threads/store";
import type { ArtifactSummary } from "./types";
const artifact: ArtifactSummary = { id: 4, title: "Architecture", kind: "solid", mime: "text/jsx", thread_ids: [2, 5], created_at: "a", updated_at: "b" };
afterEach(() => { cleanup(); disconnectArtifacts(); closeArtifact(); });
describe("artifact navigation", () => {
  it("opens a referenced artifact without changing the addressed Thread", () => {
    setArtifactState({ summaries: [{ ...artifact, thread_ids: [...artifact.thread_ids] }] });
    setThreadState(draft => { draft.focusedId = 5; });
    const send = vi.fn(); attachArtifactTransport(send);
    render(() => <ArtifactCard id={4} />);
    fireEvent.click(screen.getByRole("button", { name: /Architecture/ })); flush();
    expect(artifactState.selectedId).toBe(4);
    expect(threadState.focusedId).toBe(5);
    expect(send).toHaveBeenCalledWith(expect.objectContaining({ type: "open_artifact", artifact_id: 4 }));
  });
  it("keeps inventory buttons mounted through open responses and list refreshes", () => {
    setArtifactState({ summaries: [{ ...artifact, thread_ids: [...artifact.thread_ids] }] });
    const send = vi.fn(); attachArtifactTransport(send);
    render(() => <ArtifactList />);
    const trigger = screen.getByRole("button", { name: /Architecture/ });
    trigger.focus(); fireEvent.click(trigger); flush();
    handleArtifactMessage({ type: "artifact_opened", client_id: send.mock.calls.at(-1)![0].client_id, artifact: { ...artifact, content: "preview" } }); flush();
    expect(screen.getByRole("button", { name: /Architecture/ })).toBe(trigger);
    listArtifacts();
    handleArtifactMessage({ type: "artifacts_listed", client_id: send.mock.calls.at(-1)![0].client_id, artifacts: [{ ...artifact, title: "Updated architecture", updated_at: "c" }] }); flush();
    expect(screen.getByRole("button", { name: /Updated architecture/ })).toBe(trigger);
    expect(trigger.isConnected).toBe(true);
  });
  it("names orchestrator backlinks Home", () => {
    setArtifactState({ summaries: [{ ...artifact, thread_ids: [0] }] });
    render(() => <ArtifactList />);
    expect(screen.getByRole("button", { name: "Home" })).toBeTruthy();
  });
  it("distinguishes unavailable inventory from an empty result and offers a read-only retry", () => {
    setArtifactState({ summaries: [], listing: true, listed: false, listError: null });
    const send = vi.fn(); attachArtifactTransport(send);
    render(() => <ArtifactList />);
    expect(screen.getByRole("status")).toHaveTextContent("Loading artifacts");
    expect(screen.queryByText(/Artifacts Hirsel creates/)).toBeNull();
    setArtifactState({ listing: false, listError: "Storage unavailable" }); flush();
    expect(screen.getByRole("alert")).toHaveTextContent("Couldn’t load the artifact list");
    expect(screen.queryByText(/Artifacts Hirsel creates/)).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Retry loading artifacts" }));
    expect(send).toHaveBeenCalledWith(expect.objectContaining({ type: "list_artifacts" }));
    handleArtifactMessage({ type: "artifacts_listed", client_id: send.mock.calls.at(-1)![0].client_id, artifacts: [] }); flush();
    expect(screen.queryByRole("alert")).toBeNull();
    expect(screen.getByText(/Artifacts Hirsel creates/)).toBeTruthy();
  });
  it("lists the same global artifact in every referencing Thread", () => {
    setArtifactState({ summaries: [{ ...artifact, thread_ids: [...artifact.thread_ids] }] });
    const first = render(() => <ArtifactList threadId={2} />);
    expect(screen.getByRole("button", { name: /Architecture/ })).toBeTruthy();
    first.unmount();
    const second = render(() => <ArtifactList threadId={5} />);
    expect(screen.getByRole("button", { name: /Architecture/ })).toBeTruthy();
    second.unmount();
    render(() => <ArtifactList threadId={8} />);
    expect(screen.queryByRole("button", { name: /Architecture/ })).toBeNull();
  });
});
