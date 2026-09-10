import { cleanup, fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";
import { flush } from "solid-js";
import { ArtifactCard, ArtifactList, ArtifactSurface } from "./ArtifactSurface";
import { artifactState, attachArtifactTransport, closeArtifact, disconnectArtifacts, setArtifactState, handleArtifactMessage, listArtifacts } from "./store";
import { setThreadState, threadState } from "../threads/store";
import { makeThread } from "../threads/fixtures";
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
  it("names retained zero backlinks from the actual Thread title", () => {
    setThreadState(draft => { draft.threads = [makeThread(0, { title: "General" })]; });
    setArtifactState({ summaries: [{ ...artifact, thread_ids: [0] }] });
    render(() => <ArtifactList />);
    expect(screen.getByRole("button", { name: "General" })).toBeTruthy();
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
describe("artifact presentation", () => {
  const opened = (id: number, patch: Partial<ArtifactSummary & { content: string }> = {}) => ({
    ...artifact, id, kind: "html" as const, mime: "text/html", content: "<p>Rendered</p>", ...patch,
  });
  it("defaults to Rendered, keeps Source through same-artifact refresh, and resets for a different artifact or reopened viewer", async () => {
    setArtifactState({ selectedId: 4, opened: opened(4), loading: false });
    const view = render(() => <ArtifactSurface />);
    let panel = view.getByRole("complementary", { name: "Artifact preview" });
    const source = within(panel).getByRole("button", { name: "Source" });
    expect(within(panel).getByRole("button", { name: "Rendered" })).toHaveAttribute("aria-pressed", "true");
    await waitFor(() => expect(panel.querySelector("iframe")).not.toBeNull());

    source.focus(); fireEvent.click(source);
    expect(document.activeElement).toBe(source);
    expect(panel.querySelector('[data-slot="artifact-source"]')).toHaveTextContent("<p>Rendered</p>");
    flush(() => setArtifactState({ opened: opened(4, { content: "  <em>Updated</em>\n" }) }));
    expect(source).toHaveAttribute("aria-pressed", "true");
    expect(panel.querySelector('[data-slot="artifact-source"]')?.textContent).toBe("  <em>Updated</em>\n");

    flush(() => setArtifactState({ selectedId: 5, opened: opened(5, { mime: "text/markdown", kind: "file", filename: "notes.md", content: "# Notes" }) }));
    expect(within(panel).getByRole("button", { name: "Rendered" })).toHaveAttribute("aria-pressed", "true");
    flush(closeArtifact);
    flush(() => setArtifactState({ selectedId: 5, opened: opened(5, { mime: "text/markdown", kind: "file", filename: "notes.md", content: "# Notes" }) }));
    panel = view.getByRole("complementary", { name: "Artifact preview" });
    expect(within(panel).getByRole("button", { name: "Rendered" })).toHaveAttribute("aria-pressed", "true");
  });
});
