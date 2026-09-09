import { flush } from "solid-js";
import { fireEvent, render, waitFor, within } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ThreadShell } from "./ThreadShell";
import { makeThread } from "./fixtures";
import { installGlobalKeymap } from "../lib/keymap";
import { setThreadNavigationOpen } from "./navigation";
import { attachThreadTransport, disconnectThreads, focusThread, handleThreadMessage, setThreadState, threadState } from "./store";
import type { ThreadClientMessage } from "./types";
vi.mock("../ws/client", () => ({ getClient: () => ({ cancelTurn: vi.fn() }), makeClientId: () => crypto.randomUUID() }));
const sent: ThreadClientMessage[] = [];
beforeEach(() => {
  sent.length = 0;
  flush(() => setThreadNavigationOpen(false));
  const storage = new Map<string, string>();
  vi.stubGlobal("localStorage", { getItem: (key: string) => storage.get(key) ?? null, setItem: (key: string, value: string) => storage.set(key, value), removeItem: (key: string) => storage.delete(key) });
  flush(() => setThreadState(draft => { Object.assign(draft, { threads: [makeThread(0, { title: "Hirsel", read: true }), makeThread(1, { read: true }), makeThread(2, { title: "Holiday", read: true })], histories: {}, streams: {}, streamTurnIds: {}, turnDetails: {}, pending: [], focusedId: 1, error: null }); }));
  attachThreadTransport(frame => sent.push(frame));
});
afterEach(() => { disconnectThreads(); vi.unstubAllGlobals(); });
describe("thread workspace", () => {
  it("shows quiet work with no messages and switches independent conversations", async () => {
    flush(() => setThreadState(draft => { draft["histories"][1] = { messages: [{ id: 1, thread_id: 1, author: "owner", body: "Groceries only", ref: null, ts: "2026-09-09T10:00:00Z" }], turns: [], activities: [], loaded: true, hasMore: false }; }));
    flush(() => setThreadState(draft => { draft["histories"][2] = { messages: [{ id: 2, thread_id: 2, author: "owner", body: "Holiday only", mentions: [1], ref: null, ts: "2026-09-09T10:00:00Z" }], turns: [], activities: [], loaded: true, hasMore: false }; }));
    const screen = render(() => <ThreadShell />);
    expect(screen.getByText("Groceries only")).toBeInTheDocument();
    expect(screen.queryByText("Holiday only")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Threads" }));
    fireEvent.click(screen.container.querySelector('[data-thread-row="2"]')!);
    expect(screen.getByText("Holiday only")).toBeInTheDocument();
    expect(screen.queryByText("Groceries only")).toBeNull();
    expect(threadState.focusedId).toBe(2);
  });
  it("opens the existing drawer from the full context title", async () => {
    const title = "Launch preparation and documentation for the west coast production workspace";
    flush(() => setThreadState(draft => { draft.threads[1].title = title; }));
    const view = render(() => <ThreadShell />);
    fireEvent.click(within(view.getByRole("heading", { level: 1 })).getByRole("button", { name: title }));
    const drawer = view.getByRole("dialog", { name: "Threads" });
    expect(drawer.querySelector('[data-thread-row="1"]')).toHaveTextContent(title);
  });
  it("does not invite starting an empty conversation while a turn is running", () => {
    flush(() => handleThreadMessage({ type: "thread_turn", turn: { id: 90, thread_id: 1, state: "running", owner_message_id: null, agent_message_id: null, started_at: "2026-09-09T10:00:00Z", finished_at: null } }));
    flush(() => setThreadState(draft => { draft.histories[1].loaded = true; }));
    const view = render(() => <ThreadShell />);
    expect(view.queryByText("Start the conversation for this thread.")).toBeNull();
    expect(view.getByText("Hirsel is working…")).toBeTruthy();
  });
  it("retries failed conversation reads without losing the addressed draft", async () => {
    const view = render(() => <ThreadShell />);
    fireEvent.input(view.container.querySelector("textarea")!, { target: { value: "Keep this draft" } });
    flush(() => focusThread(1));
    const request = sent.findLast(frame => frame.type === "open_thread")!;
    if (request.type !== "open_thread") throw new Error("missing open");
    flush(() => handleThreadMessage({ type: "error", client_id: request.client_id, detail: "storage unavailable" }));
    const retry = await view.findByRole("button", { name: "Retry loading conversation" });
    expect(view.queryByText("Loading conversation…")).toBeNull();
    fireEvent.click(retry);
    const retryRequest = sent.findLast(frame => frame.type === "open_thread")!;
    if (retryRequest.type !== "open_thread") throw new Error("missing retry");
    flush(() => handleThreadMessage({ type: "thread_opened", client_id: retryRequest.client_id, detail: { thread: makeThread(1), messages: [], activities: [], turns: [], has_more: false } }));
    await waitFor(() => expect(view.queryByRole("button", { name: "Retry loading conversation" })).toBeNull());
    expect(view.container.querySelector("textarea")!.value).toBe("Keep this draft");
    expect(threadState.focusedId).toBe(1);
  });
  it("opens row actions inside the modal without changing the addressed Thread", async () => {
    const view = render(() => <ThreadShell />);
    fireEvent.click(view.getByRole("button", { name: "Threads" }));
    const row = view.container.querySelector<HTMLButtonElement>('[data-thread-row="2"]')!;
    row.focus(); fireEvent.keyDown(row, { key: "F10", shiftKey: true });
    const menu = await view.findByRole("menu", { name: "Actions for Holiday" });
    expect(menu.closest("dialog")).toBe(view.getByRole("dialog", { name: "Threads" }));
    fireEvent.click(within(menu).getByRole("menuitem", { name: "Archive thread" }));
    expect(sent).toContainEqual(expect.objectContaining({ type: "thread_action", thread_id: 2, action: "archive" }));
    expect(threadState.focusedId).toBe(1);
    expect(row).toBeInTheDocument();
  });
  it("supports local row navigation and restores focus after a row leaves the filter", async () => {
    const view = render(() => <ThreadShell />);
    fireEvent.click(view.getByRole("button", { name: "Threads" }));
    const row1 = view.container.querySelector<HTMLButtonElement>('[data-thread-row="1"]')!;
    const row2 = view.container.querySelector<HTMLButtonElement>('[data-thread-row="2"]')!;
    row1.focus(); fireEvent.keyDown(row1, { key: "End" }); expect(document.activeElement).toBe(row2);
    fireEvent.keyDown(row2, { key: "Home" }); expect(document.activeElement).toBe(row1);
    fireEvent.keyDown(row1, { key: "ArrowDown" }); expect(document.activeElement).toBe(row2);
    expect(fireEvent.keyDown(row2, { key: "Tab" })).toBe(true);
    fireEvent.contextMenu(row2);
    const menu = await view.findByRole("menu", { name: "Actions for Holiday" });
    fireEvent.click(within(menu).getByRole("menuitem", { name: "Archive thread" }));
    flush(() => handleThreadMessage({ type: "thread_upsert", thread: makeThread(2, { title: "Holiday", archived_at: "2026-09-09T10:00:00Z", revision: 2 }) }));
    await waitFor(() => expect(document.activeElement).toBe(row1));
    expect(threadState.focusedId).toBe(1);
  });
  it("creates through the correlated host contract and focuses its visible row", async () => {
    const screen = render(() => <ThreadShell />);
    fireEvent.click(screen.getByRole("button", { name: "Threads" }));
    fireEvent.input(screen.getByLabelText("New thread title"), { target: { value: "Buy milk" } });
    fireEvent.click(screen.getByRole("button", { name: "Create thread" }));
    const frame = sent.find(f => f.type === "create_thread");
    if (!frame || frame.type !== "create_thread") throw new Error("missing create");
    expect(frame.title).toBe("Buy milk");
    flush(() => handleThreadMessage({ type: "thread_created", client_id: frame.client_id, thread: makeThread(3, { title: "Buy milk", read: true }) }));
    await waitFor(() => expect(threadState.focusedId).toBe(3));
    expect(screen.container.querySelector('[data-thread-id="3"]')).toBeInTheDocument();
    expect(screen.queryByRole("dialog", { name: "Threads" })).toBeNull();
  });
  it("keeps drafts separate and sends to explicit thread ownership", async () => {
    const screen = render(() => <ThreadShell />);
    const input = screen.container.querySelector("textarea")!;
    fireEvent.input(input, { target: { value: "groceries draft" } });
    flush(() => focusThread(2));
    const holiday = screen.container.querySelector("textarea")!;
    expect(holiday.value).toBe("");
    fireEvent.input(holiday, { target: { value: "holiday draft" } });
    flush(() => focusThread(1));
    const groceries = screen.container.querySelector("textarea")!;
    expect(groceries.value).toBe("groceries draft");
    fireEvent.keyDown(groceries, { key: "Enter" });
    await waitFor(() => expect(sent.some(f => f.type === "send_thread_message" && f.thread_id === 1 && f.body === "groceries draft")).toBe(true));
  });
  it("shows informational activity content within its owning thread without creating work", () => {
    flush(() => handleThreadMessage({ type: "thread_activity", activity: { id: 9, thread_id: 1, turn_id: null, kind: "plugin.build_finished", data: { plugin: "build", payload: { message: "All checks passed." } }, ts: "2026-09-09T10:00:00Z" } }));
    const screen = render(() => <ThreadShell />);
    expect(screen.getByText("All checks passed.")).toBeInTheDocument();
    expect(threadState.threads).toHaveLength(3);
    expect(threadState.threads.find(t => t.id === 1)?.settled_at).toBeNull();
    flush(() => focusThread(2));
    expect(screen.queryByText("All checks passed.")).toBeNull();
  });
  it("keeps owner-facing summaries inline and execution details behind disclosure", () => {
    flush(() => setThreadState(draft => { draft.histories[1] = {
      messages: [], turns: [], loaded: true, hasMore: false,
      activities: [
        { id: 1, thread_id: 1, turn_id: null, kind: "summary", ts: "2026-09-09T10:00:00Z", data: { content_md: "Your shopping list is ready." } },
        { id: 2, thread_id: 1, turn_id: null, kind: "turn_error", ts: "2026-09-09T10:00:01Z", data: { message: "Execution diagnostics" } },
      ],
    }; }));
    const screen = render(() => <ThreadShell />);
    expect(screen.getByText("Your shopping list is ready.").closest("details")).toBeNull();
    const inspector = screen.getByText("Inspect execution").closest("details")!;
    expect(inspector.open).toBe(false);
    expect(screen.getByText("Execution diagnostics").closest("details")).toBe(inspector);
    expect(screen.getByRole("textbox", { name: "Message Buy groceries" })).toBeInTheDocument();
  });
  it("distinguishes titleless Home from a framed Thread and keeps inventory closed until requested", () => {
    const screen = render(() => <ThreadShell />);
    expect(screen.container.querySelector('[data-thread-id="1"]')).toHaveClass("thread-focus-frame");
    expect(screen.getByRole("button", { name: "Threads" })).toHaveAttribute("aria-expanded", "false");
    expect(screen.queryByRole("dialog", { name: "Threads" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Home" }));
    expect(screen.container.querySelector('[data-thread-id="0"]')).not.toHaveClass("thread-focus-frame");
    expect(screen.queryByRole("heading", { level: 1 })).toBeNull();
    expect(screen.getByRole("button", { name: "Home" })).toHaveAttribute("aria-pressed", "true");
    fireEvent.click(screen.getByRole("button", { name: "Threads" }));
    expect(screen.getByRole("dialog", { name: "Threads" })).toBeInTheDocument();
    fireEvent.click(screen.container.querySelector('[data-thread-row="1"]')!);
    expect(screen.queryByRole("dialog", { name: "Threads" })).toBeNull();
    expect(screen.container.querySelector('[data-thread-id="1"]')).toHaveClass("thread-focus-frame");
  });
  it("keeps the visible destination and addressed send while browsing all artifacts", () => {
    const screen = render(() => <ThreadShell />);
    fireEvent.click(screen.getByRole("button", { name: "All artifacts" }));
    expect(screen.getByRole("heading", { level: 1 })).toHaveTextContent("Buy groceries");
    expect(screen.getAllByRole("heading", { name: "All artifacts" })).toHaveLength(1);
    const input = screen.getByRole("textbox", { name: "Message Buy groceries" });
    fireEvent.input(input, { target: { value: "Follow up in groceries" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    expect(sent).toContainEqual(expect.objectContaining({ type: "send_thread_message", thread_id: 1, body: "Follow up in groceries", mode: "send" }));
    fireEvent.click(screen.getByRole("button", { name: "Thread actions" }));
    fireEvent.click(within(document.body).getByRole("menuitem", { name: "Settle thread" }));
    expect(sent).toContainEqual(expect.objectContaining({ type: "thread_action", thread_id: 1, action: "settle" }));
  });

  it("opens the actual Thread drawer with g then t and focuses its selected Thread", async () => {
    const screen = render(() => <ThreadShell />);
    const dispose = installGlobalKeymap();
    try {
      screen.getByRole("button", { name: "Home" }).focus();
      fireEvent.keyDown(window, { key: "g" });
      fireEvent.keyDown(window, { key: "t" });
      expect(screen.getByRole("dialog", { name: "Threads" })).toBeInTheDocument();
      await waitFor(() => expect(screen.container.querySelector('[data-thread-row="1"]')).toHaveFocus());
    } finally { dispose(); }
  });

  it("shows independent attention, unread and execution signals without changing inventory placement", () => {
    flush(() => setThreadState(draft => {
      draft.threads = [makeThread(0), makeThread(1, { read: false, attention: "needs_owner" }),
        makeThread(2, { settled_at: "2026-09-09T10:00:00Z" }),
        makeThread(3, { snoozed_until: "2099-01-01T00:00:00Z" }),
        makeThread(4, { archived_at: "2026-09-09T10:00:00Z" })];
      draft.histories[1] = { messages: [], activities: [], loaded: true, hasMore: false, turns: [{ id: 4, thread_id: 1, owner_message_id: null, agent_message_id: null, state: "running", started_at: "2026-09-09T10:00:00Z", finished_at: null }] };
    }));
    flush(() => setThreadState(draft => { draft.threads[1].running_turn = draft.histories[1].turns[0]; }));
    const screen = render(() => <ThreadShell />);
    fireEvent.click(screen.getByRole("button", { name: "Threads" }));
    const row = within(screen.container.querySelector<HTMLElement>('[data-thread-row="1"]')!);
    expect(row.getByRole("img", { name: "Unread" })).toBeInTheDocument();
    expect(row.getByText("Needs you")).toBeInTheDocument();
    expect(row.getByText(/Working/)).toBeInTheDocument();
    for (const [section, id] of [["settled", 2], ["snoozed", 3], ["archived", 4]] as const) {
      fireEvent.click(screen.getByRole("button", { name: section }));
      expect(screen.container.querySelector(`[data-thread-row="${id}"]`)).toBeInTheDocument();
      expect(screen.container.querySelector('[data-thread-row="1"]')).toBeNull();
    }
    fireEvent.click(screen.getByRole("button", { name: "active" }));
    expect(screen.container.querySelector('[data-thread-row="1"]')).toBeInTheDocument();
  });

  it("settles only through explicit action, preserving read as independent state", () => {
    const screen = render(() => <ThreadShell />);
    fireEvent.click(screen.getByRole("button", { name: "Thread actions" }));
    fireEvent.click(within(document.body).getByRole("menuitem", { name: "Settle thread" }));
    expect(sent).toContainEqual({ type: "thread_action", thread_id: 1, action: "settle", data: {}, expected_revision: undefined });
    expect(threadState.threads.find(t => t.id === 1)?.settled_at).toBeNull();
    flush(() => handleThreadMessage({ type: "thread_upsert", thread: makeThread(1, { settled_at: "2026-09-09T10:00:00Z", revision: 2 }) }));
    fireEvent.click(screen.getByRole("button", { name: "Thread actions" }));
    expect(within(document.body).getByRole("menuitem", { name: "Reopen thread" })).toBeInTheDocument();
  });
});
