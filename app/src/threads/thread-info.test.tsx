import { flush } from "solid-js";
import { fireEvent, render, within } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { dispatch } from "../store/store";
import { setHistoryId } from "../lib/history";
import { ThreadShell } from "./ThreadShell";
import { makeThread } from "./fixtures";
import { closeThreadNavigation } from "./navigation";
import { closeThreadCreate } from "./create";
import { attachThreadTransport, disconnectThreads, handleThreadMessage, setThreadState } from "./store";
import type { SubagentModelCatalog } from "../protocol";
import type { Thread, ThreadClientMessage } from "./types";

vi.mock("../ws/client", () => ({ getClient: () => ({ cancelTurn: vi.fn(), getBlobUrl: async (id: string) => `https://example.test/blob/${id}` }), makeClientId: () => crypto.randomUUID() }));

const sent: ThreadClientMessage[] = [];
const CATALOG: SubagentModelCatalog = {
  providers: [{ provider: "claude", label: "Claude Code", models: [
    { id: "claude-opus-4-7", label: "Opus 4.7", variants: ["default", "high"], enabled_variants: ["default", "high"], enabled: true },
    { id: "claude-sonnet-4-7", label: "Sonnet 4.7", variants: ["default"], enabled_variants: ["default"], enabled: false },
  ] }],
  native_worker: { label: "Native worker", enabled: false, provider_id: null, eligible_provider_ids: [], model: "local-model", default_model: "local-model", model_override: null },
};
const space = (patch: Partial<Thread> = {}) => makeThread(4, { title: "Kitchen", kind: "space", description: "Everything about the kitchen.", read: true, revision: 3, created_at: "2026-09-09T10:00:00Z", updated_at: "2026-09-09T11:00:00Z", ...patch });

function stubMedia() {
  vi.stubGlobal("matchMedia", ((query: string) => ({ media: query, matches: false, onchange: null, addEventListener: () => {}, removeEventListener: () => {}, addListener: () => {}, removeListener: () => {}, dispatchEvent: () => false })) as unknown as typeof window.matchMedia);
}

function openInfo() {
  const view = render(() => <ThreadShell />);
  fireEvent.click(view.getByRole("button", { name: "Info" }));
  const pane = () => view.container.querySelector<HTMLElement>('[data-slot="thread-info"]')!;
  return { ...view, pane };
}

beforeEach(() => {
  sent.length = 0;
  stubMedia();
  const storage = new Map<string, string>();
  vi.stubGlobal("localStorage", { getItem: (key: string) => storage.get(key) ?? null, setItem: (key: string, value: string) => storage.set(key, value), removeItem: (key: string) => storage.delete(key) });
  flush(() => dispatch({ type: "connection_status", status: "connected" }));
  flush(() => dispatch({ type: "subagent_models_changed", catalog: CATALOG }));
  flush(() => dispatch({ type: "model_changed", model: { current: { id: "claude-opus-4-7", variant: "default" }, available: [], provider_id: "anthropic" } }));
  flush(() => setHistoryId("test-history"));
  flush(() => closeThreadNavigation());
  flush(() => closeThreadCreate());
  flush(() => setThreadState(draft => { Object.assign(draft, { ready: true, linkError: null, threads: [space(), makeThread(5, { kind: "task", title: "Fix the tap", parent_thread_id: 4, read: true })], histories: {}, turnDetails: {}, pending: [], focusedId: 4, error: null }); }));
  attachThreadTransport(frame => sent.push(frame));
});
afterEach(() => { disconnectThreads(); vi.useRealTimers(); vi.unstubAllGlobals(); });

describe("thread info pane", () => {
  it("replaces the conversation with the Thread's own facts and hands it back", () => {
    const view = render(() => <ThreadShell />);
    expect(view.container.querySelector('[data-slot="thread-info"]')).toBeNull();
    const info = view.getByRole("button", { name: "Info" });
    expect(info).toHaveAttribute("aria-pressed", "false");

    fireEvent.click(info);
    expect(info).toHaveAttribute("aria-pressed", "true");
    expect(view.getByRole("button", { name: "Conversation" })).toHaveAttribute("aria-pressed", "false");
    expect(view.container.querySelector('[data-slot="thread-scroll"] [data-author]')).toBeNull();
    const pane = view.container.querySelector<HTMLElement>('[data-slot="thread-info"]')!;
    expect(within(pane).getByRole("heading", { name: "Kitchen" })).toBeVisible();
    expect(pane).toHaveTextContent("Everything about the kitchen.");
    expect(pane.querySelector('[data-fact="Parent"]')).toHaveTextContent("Top level");
    expect(pane.querySelector('[data-fact="Children"]')).toHaveTextContent("Fix the tap");
    expect(pane.querySelector('[data-fact="Created"]')).toBeTruthy();
    expect(pane.querySelector('[data-fact="Updated"]')).toBeTruthy();
    // Facts the client does not hold are not invented.
    expect(pane.querySelector('[data-fact="Current brief"]')).toBeNull();
    expect(pane.querySelector('[data-fact="Related"]')).toBeNull();

    fireEvent.click(view.getByRole("button", { name: "Conversation" }));
    expect(view.container.querySelector('[data-slot="thread-info"]')).toBeNull();
    expect(info).toHaveAttribute("aria-pressed", "false");
  });

  it("renames and describes the Thread with revision-fenced actions, keeping a refused draft", () => {
    const view = openInfo();
    fireEvent.click(view.getByRole("button", { name: "Rename thread" }));
    const title = view.getByLabelText("Thread title") as HTMLInputElement;
    fireEvent.input(title, { target: { value: "  Kitchen garden  " } });
    fireEvent.keyDown(title, { key: "Enter" });
    expect(sent.at(-1)).toMatchObject({ type: "thread_action", thread_id: 4, action: "set_title", data: { title: "Kitchen garden" }, expected_revision: 3 });

    // The Host's new revision is what closes the editor — nothing is optimistic.
    flush(() => handleThreadMessage({ type: "thread_upsert", thread: space({ title: "Kitchen garden", revision: 4 }) }));
    expect(view.queryByLabelText("Thread title")).toBeNull();
    expect(view.getByRole("heading", { name: "Kitchen garden" })).toBeVisible();

    fireEvent.click(view.getByRole("button", { name: "Edit description" }));
    fireEvent.input(view.getByLabelText("Thread description"), { target: { value: "## Scope\n\nThe kitchen." } });
    fireEvent.click(view.getByRole("button", { name: "Save description" }));
    expect(sent.at(-1)).toMatchObject({ action: "set_description", data: { description: "## Scope\n\nThe kitchen." }, expected_revision: 4 });

    // A refusal keeps the editor open on the draft, with the reason beside it.
    flush(() => setThreadState(draft => { draft.error = { operation: "request", detail: "thread changed; reload before updating it", threadId: 4 }; }));
    expect(within(view.pane()).getByRole("alert")).toHaveTextContent("thread changed");
    expect(view.getByLabelText("Thread description")).toHaveValue("## Scope\n\nThe kitchen.");

    fireEvent.click(view.getByRole("button", { name: "Cancel description edit" }));
    expect(view.queryByLabelText("Thread description")).toBeNull();
  });

  it("shows an empty description as an invitation rather than a blank", () => {
    flush(() => handleThreadMessage({ type: "thread_upsert", thread: space({ description: "", revision: 4 }) }));
    const view = openInfo();
    expect(view.getByText("No description yet — add one")).toBeVisible();
  });

  it("names where the next turn runs and lets the Owner move it", () => {
    const view = openInfo();
    const runsOn = () => view.container.querySelector<HTMLElement>('[data-fact="Runs on"]')!;
    expect(runsOn()).toHaveTextContent("Default coordinator · anthropic · claude-opus-4-7");

    fireEvent.click(view.getByRole("button", { name: "Change where this Thread runs" }));
    fireEvent.change(view.getByLabelText("Where this Thread runs"), { target: { value: "cli:claude" } });
    // Only enabled catalog models are offered, exactly as delegation resolves them.
    const model = view.getByLabelText("This Thread model") as HTMLSelectElement;
    expect([...model.options].map(option => option.value)).toEqual(["claude-opus-4-7"]);
    fireEvent.change(view.getByLabelText("This Thread reasoning variant"), { target: { value: "high" } });
    fireEvent.click(view.getByRole("button", { name: "Save where this Thread runs" }));
    expect(sent.at(-1)).toMatchObject({ action: "set_execution", expected_revision: 3, data: { execution: { kind: "cli", agent: "claude", model: "claude-opus-4-7", variant: "high" } } });

    // A rejected target leaves the Owner's choice on screen with the reason.
    flush(() => setThreadState(draft => { draft.error = { operation: "request", detail: "model `claude-opus-4-7` is unavailable", threadId: 4 }; }));
    expect(within(view.pane()).getByRole("alert")).toHaveTextContent("unavailable");
    expect(view.getByLabelText("Where this Thread runs")).toHaveValue("cli:claude");
    fireEvent.click(view.getByRole("button", { name: "Cancel execution change" }));

    flush(() => handleThreadMessage({ type: "thread_upsert", thread: space({ revision: 4, execution: { kind: "cli", agent: "claude", model: "claude-opus-4-7", variant: "high" } }) }));
    expect(runsOn()).toHaveTextContent("Claude Code · claude-opus-4-7 · High");

    // Back to the default coordinator is one choice, not a special case.
    fireEvent.click(view.getByRole("button", { name: "Change where this Thread runs" }));
    fireEvent.change(view.getByLabelText("Where this Thread runs"), { target: { value: "default" } });
    fireEvent.click(view.getByRole("button", { name: "Save where this Thread runs" }));
    expect(sent.at(-1)).toMatchObject({ action: "set_execution", expected_revision: 4, data: { execution: null } });
    flush(() => handleThreadMessage({ type: "thread_upsert", thread: space({ revision: 5, execution: null }) }));
    expect(view.queryByLabelText("Where this Thread runs")).toBeNull();
    expect(runsOn()).toHaveTextContent("Default coordinator");
  });
});
