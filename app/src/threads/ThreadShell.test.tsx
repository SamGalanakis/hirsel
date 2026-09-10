import { dispatch } from "../store/store";
import { setHistoryId } from "../lib/history";
import { flush } from "solid-js";
import { fireEvent, render, waitFor, within } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ThreadShell } from "./ThreadShell";
import { makeThread } from "./fixtures";
import { installGlobalKeymap } from "../lib/keymap";
import { closeThreadNavigation } from "./navigation";
import { attachThreadTransport, disconnectThreads, focusThread, handleThreadMessage, openThread, sendThreadMessage, setThreadState, threadState } from "./store";
import type { ThreadClientMessage } from "./types";
vi.mock("../ws/client", () => ({ getClient: () => ({ cancelTurn: vi.fn() }), makeClientId: () => crypto.randomUUID() }));
const sent: ThreadClientMessage[] = [];
beforeEach(() => {
  sent.length = 0;
  flush(() => dispatch({ type: "connection_status", status: "connected" }));
  flush(() => setHistoryId("test-history"));
  flush(() => closeThreadNavigation());
  const storage = new Map<string, string>();
  vi.stubGlobal("localStorage", { getItem: (key: string) => storage.get(key) ?? null, setItem: (key: string, value: string) => storage.set(key, value), removeItem: (key: string) => storage.delete(key) });
  flush(() => setThreadState(draft => { Object.assign(draft, { ready: true, linkError: null, threads: [makeThread(0, { title: "Hirsel", read: true }), makeThread(1, { read: true }), makeThread(2, { title: "Holiday", read: true })], histories: {}, turnDetails: {}, pending: [], focusedId: 1, error: null }); }));
  attachThreadTransport(frame => sent.push(frame));
});
afterEach(() => { disconnectThreads(); vi.useRealTimers(); vi.unstubAllGlobals(); });
describe("thread workspace", () => {
  it("shows quiet work with no messages and switches independent conversations", async () => {
    flush(() => setThreadState(draft => { draft["histories"][1] = { brief: { text: "", artifact_ids: [] }, messages: [{ id: 1, thread_id: 1, author: "owner", body: "Groceries only", ref: null, ts: "2026-09-09T10:00:00Z" }], turns: [], activities: [], loaded: true, hasMore: false }; }));
    flush(() => setThreadState(draft => { draft["histories"][2] = { brief: { text: "", artifact_ids: [] }, messages: [{ id: 2, thread_id: 2, author: "owner", body: "Holiday only", mentions: [1], ref: null, ts: "2026-09-09T10:00:00Z" }], turns: [], activities: [], loaded: true, hasMore: false }; }));
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
    flush(() => handleThreadMessage({ type: "thread_turn", turn: { requester_thread_id: null, requester_turn_id: null, id: 90, thread_id: 1, state: "running", owner_message_id: null, agent_message_id: null, started_at: "2026-09-09T10:00:00Z", finished_at: null } }));
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
    flush(() => handleThreadMessage({ type: "thread_opened", client_id: retryRequest.client_id, detail: { brief: { text: "", artifact_ids: [] }, thread: makeThread(1), messages: [], activities: [], turns: [], turn_timelines: [], related_items: [], has_more: false } }));
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
    fireEvent.keyDown(row2, { key: "Home" }); expect(document.activeElement).toBe(view.container.querySelector('[data-thread-row="0"]'));
    row1.focus();
    fireEvent.keyDown(row1, { key: "ArrowDown" }); expect(document.activeElement).toBe(row2);
    expect(fireEvent.keyDown(row2, { key: "Tab" })).toBe(true);
    fireEvent.contextMenu(row2);
    const menu = await view.findByRole("menu", { name: "Actions for Holiday" });
    fireEvent.click(within(menu).getByRole("menuitem", { name: "Archive thread" }));
    flush(() => handleThreadMessage({ type: "thread_upsert", thread: makeThread(2, { title: "Holiday", archived_at: "2026-09-09T10:00:00Z", revision: 2 }) }));
    await waitFor(() => expect(document.activeElement).toBe(row1));
    expect(threadState.focusedId).toBe(1);
  });
  it("gives creation and browsing one deterministic initial focus owner", async () => {
    const view = render(() => <ThreadShell />);
    fireEvent.click(view.getByRole("button", { name: "New thread" }));
    const title = view.getByLabelText("New thread title");
    await new Promise<void>(resolve => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));
    expect(document.activeElement).toBe(title);
    fireEvent.input(title, { target: { value: "Immediate typing" } });
    fireEvent.click(view.getByRole("button", { name: "Close threads" }));
    fireEvent.click(view.getByRole("button", { name: "Threads" }));
    await waitFor(() => expect(document.activeElement).toBe(view.container.querySelector('[data-thread-row="1"]')));
    expect(title).toHaveValue("Immediate typing");
  });
  it("keeps attention visible on the closed rail and refreshes when a snooze expires", () => {
    vi.useFakeTimers(); const now = Date.parse("2026-09-10T10:00:00Z"); vi.setSystemTime(now);
    flush(() => setThreadState(draft => { draft.threads = [makeThread(0, { attention: "needs_owner" }), makeThread(1, { attention: "needs_owner", read: true }), makeThread(2, { attention: "needs_owner", snoozed_until: new Date(now + 1000).toISOString() }), makeThread(3, { attention: "needs_owner", archived_at: new Date(now).toISOString() }), makeThread(4, { read: false })]; }));
    const view = render(() => <ThreadShell />);
    const rail = view.getByRole("button", { name: "Threads" });
    expect(rail).toHaveAccessibleDescription("2 threads need your attention");
    flush(() => vi.advanceTimersByTime(1001));
    expect(rail).toHaveAccessibleDescription("3 threads need your attention");
    expect(threadState.focusedId).toBe(1); expect(threadState.threads[2].read).toBe(false);
    expect(view.queryByRole("dialog", { name: "Threads" })).toBeNull();
  });
  it("keeps filter selection in a compact keyboard menu across reopening", async () => {
    const view = render(() => <ThreadShell />);
    fireEvent.click(view.getByRole("button", { name: "Threads" }));
    const filter = view.getByRole("button", { name: "Filter threads: active" });
    fireEvent.keyDown(filter, { key: "ArrowDown" });
    const settled = await view.findByRole("menuitemradio", { name: "settled" });
    expect(view.getByRole("menuitemradio", { name: "active" })).toHaveAttribute("aria-checked", "true");
    fireEvent.click(settled);
    expect(view.getByRole("button", { name: "Filter threads: settled" })).toBeTruthy();
    fireEvent.click(view.getByRole("button", { name: "Close threads" }));
    fireEvent.click(view.getByRole("button", { name: "Threads" }));
    expect(view.getByRole("button", { name: "Filter threads: settled" })).toBeTruthy();
    expect(view.queryByRole("button", { name: "active" })).toBeNull();
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
    flush(() => handleThreadMessage({ type: "thread_activity", activity: { artifact_ids: [], id: 9, thread_id: 1, turn_id: null, kind: "plugin.build_finished", data: { plugin: "build", payload: { message: "All checks passed." } }, ts: "2026-09-09T10:00:00Z" } }));
    const screen = render(() => <ThreadShell />);
    fireEvent.click(screen.getByRole("button", { name: "Turn options" }));
    fireEvent.click(within(document.body).getByRole("menuitem", { name: "Technical details" }));
    expect(screen.getByText(/"message": "All checks passed\."/)).toBeInTheDocument();
    expect(threadState.threads).toHaveLength(3);
    expect(threadState.threads.find(t => t.id === 1)?.settled_at).toBeNull();
    flush(() => focusThread(2));
    expect(screen.queryByText(/"message": "All checks passed\."/)).toBeNull();
  });
  it("keeps owner-facing summaries inline and technical details behind the overflow", () => {
    flush(() => setThreadState(draft => { draft.histories[1] = { brief: { text: "", artifact_ids: [] },
      messages: [], turns: [], loaded: true, hasMore: false,
      activities: [
        { artifact_ids: [], id: 1, thread_id: 1, turn_id: null, kind: "summary", ts: "2026-09-09T10:00:00Z", data: { content_md: "Your shopping list is ready." } },
        { artifact_ids: [], id: 2, thread_id: 1, turn_id: null, kind: "turn_error", ts: "2026-09-09T10:00:01Z", data: { message: "Execution diagnostics" } },
      ],
    }; }));
    const screen = render(() => <ThreadShell />);
    expect(screen.getByText("Your shopping list is ready.").closest("details")).toBeNull();
    expect(screen.queryByText(/"message": "Execution diagnostics"/)).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Turn options" }));
    fireEvent.click(within(document.body).getByRole("menuitem", { name: "Technical details" }));
    expect(screen.getByText(/"message": "Execution diagnostics"/).closest('[role="region"]')).toHaveAttribute("data-slot", "work-diagnostics");
    expect(screen.getByRole("textbox", { name: "Message Buy groceries" })).toBeInTheDocument();
  });
  it("keeps retained zero ordinary and the overview unaddressed", () => {
    flush(() => focusThread(0));
    const screen = render(() => <ThreadShell />);
    expect(location.pathname).toBe("/t/0");
    expect(screen.container.querySelector('[data-thread-id="0"]')).toHaveClass("thread-focus-frame");
    expect(screen.getByRole("heading", { level: 1 })).toHaveTextContent("Hirsel");
    fireEvent.click(screen.getByRole("button", { name: "Thread overview" }));
    expect(threadState.focusedId).toBeNull();
    expect(location.pathname).toBe("/");
    expect(screen.container.querySelector("textarea")).toBeNull();
    expect(screen.queryByRole("dialog", { name: "Threads" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Threads" }));
    fireEvent.click(screen.container.querySelector('[data-thread-row="1"]')!);
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
      screen.getByRole("button", { name: "Thread overview" }).focus();
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
      draft.histories[1] = { brief: { text: "", artifact_ids: [] }, messages: [], activities: [], loaded: true, hasMore: false, turns: [{ requester_thread_id: null, requester_turn_id: null, id: 4, thread_id: 1, owner_message_id: null, agent_message_id: null, state: "running", started_at: "2026-09-09T10:00:00Z", finished_at: null }] };
    }));
    flush(() => setThreadState(draft => { draft.threads[1].running_turn = draft.histories[1].turns[0]; }));
    const screen = render(() => <ThreadShell />);
    fireEvent.click(screen.getByRole("button", { name: "Threads" }));
    const row = within(screen.container.querySelector<HTMLElement>('[data-thread-row="1"]')!);
    expect(row.getByRole("img", { name: "Unread" })).toBeInTheDocument();
    expect(row.getByText("Needs you")).toBeInTheDocument();
    expect(row.getByText(/Working/)).toBeInTheDocument();
    for (const [section, id] of [["settled", 2], ["snoozed", 3], ["archived", 4]] as const) {
      fireEvent.click(screen.getByRole("button", { name: /Filter threads:/ }));
      fireEvent.click(screen.getByRole("menuitemradio", { name: section }));
      expect(screen.container.querySelector(`[data-thread-row="${id}"]`)).toBeInTheDocument();
      expect(screen.container.querySelector('[data-thread-row="1"]')).toBeNull();
    }
    fireEvent.click(screen.getByRole("button", { name: /Filter threads:/ }));
    fireEvent.click(screen.getByRole("menuitemradio", { name: "active" }));
    expect(screen.container.querySelector('[data-thread-row="1"]')).toBeInTheDocument();
  });

  it("settles only through explicit action, preserving read as independent state", () => {
    const screen = render(() => <ThreadShell />);
    fireEvent.click(screen.getByRole("button", { name: "Thread actions" }));
    fireEvent.click(within(document.body).getByRole("menuitem", { name: "Settle thread" }));
    expect(sent).toContainEqual(expect.objectContaining({ type: "thread_action", history_id: "test-history", thread_id: 1, action: "settle", data: {}, expected_revision: undefined }));
    expect(threadState.threads.find(t => t.id === 1)?.settled_at).toBeNull();
    flush(() => handleThreadMessage({ type: "thread_upsert", thread: makeThread(1, { settled_at: "2026-09-09T10:00:00Z", revision: 2 }) }));
    fireEvent.click(screen.getByRole("button", { name: "Thread actions" }));
    expect(within(document.body).getByRole("menuitem", { name: "Reopen thread" })).toBeInTheDocument();
  });
});

it("preserves the exact inline tool rows across message/activity updates", async () => {
  flush(() => setThreadState(draft => { draft.histories[1] = { brief: { text: "", artifact_ids: [] }, messages: [{ id:1,thread_id:1,author:"agent",body:"Answer",ref:null,ts:"2026-09-09T10:00:00Z",tool_calls:[{id:"call-a",name:"read_file",ok:true},{id:"call-b",name:"read_file",ok:true}] }], turns:[{ requester_thread_id: null, requester_turn_id: null,id:1,thread_id:1,owner_message_id:null,agent_message_id:1,state:"completed",started_at:"2026-09-09T09:59:00Z",finished_at:"2026-09-09T10:00:00Z"}],activities:[],loaded:true,hasMore:false }; }));
  const view=render(()=> <ThreadShell />);
  const first=view.container.querySelector('[data-message-id="1"] [data-tool-call-id="call-a"]')!;
  flush(()=>handleThreadMessage({type:"thread_activity",activity:{ artifact_ids: [],id:1,thread_id:1,turn_id:1,kind:"tool_completed",data:{id:"call-a",name:"read_file",ok:true},ts:"2026-09-09T10:00:01Z"}}));
  flush(()=>handleThreadMessage({type:"msg",message:{id:2,thread_id:1,author:"owner",body:"Next",ref:null,ts:"2026-09-09T10:00:02Z"}}));
  expect(view.container.querySelector('[data-message-id="1"] [data-tool-call-id="call-a"]')).toBe(first);
  expect(view.container.querySelectorAll('[data-message-id="1"] [data-slot="timeline-tool"]')).toHaveLength(2);
  expect(view.container.querySelector('[data-message-id="1"]')?.textContent).not.toContain('tool completed');
});

it("renders the exact persisted timeline from a fresh open_thread snapshot", async () => {
  const opening = openThread(1);
  const frame = sent.findLast(message => message.type === "open_thread");
  if (frame?.type !== "open_thread") throw new Error("Missing open");
  const owner = { id: 70, thread_id: 1, author: "owner" as const, body: "Inspect it", ref: null, ts: "2026-09-10T10:00:00Z" };
  const agent = { id: 71, thread_id: 1, author: "agent" as const, body: "Inspection complete", ref: 70, ts: "2026-09-10T10:00:02Z", tool_calls: [{ id: "shell-1", name: "shell_run", ok: true }] };
  const turn = { requester_thread_id: null, requester_turn_id: null, id: 72, thread_id: 1, owner_message_id: 70, agent_message_id: 71, state: "completed" as const, started_at: owner.ts, finished_at: agent.ts };
  flush(() => handleThreadMessage({ type: "thread_opened", client_id: frame.client_id, detail: {
    ...({ brief: { text: "", artifact_ids: [] }, thread: makeThread(1), activities: [], related_items: [], has_more: false }),
    messages: [owner, agent], turns: [turn],
    turn_timelines: [{ turn_id: 72, events: [
      { seq: 1, event: { kind: "reasoning", text: "Checking storage first." } },
      { seq: 2, event: { kind: "tool_start", id: "shell-1", name: "shell_run", summary: "cmd: inspect", input: { text: "{\n  \"cmd\": \"inspect\"\n}", truncated: false } } },
      { seq: 3, event: { kind: "tool_done", id: "shell-1", name: "shell_run", ok: true, summary: "ok status 0", result: { text: "{\n  \"stdout\": \"durable\"\n}", truncated: false } } },
      { seq: 4, event: { kind: "prose", text: "Inspection complete" } },
    ] }],
  } }));
  await opening;
  const view = render(() => <ThreadShell />);
  const article = view.container.querySelector('[data-message-id="71"]')!;
  expect(article.querySelectorAll('[data-slot="timeline"] > li')).toHaveLength(2);
  expect(article.textContent).toContain("Checking storage first.");
  expect(article.textContent).toContain("Inspection complete");
  fireEvent.click(within(article as HTMLElement).getByRole("button", { name: "shell_run — show result" }));
  expect(article.textContent).toContain('"cmd": "inspect"');
  expect(article.textContent).toContain('"stdout": "durable"');
});

it("keeps the same expanded tool result and focus when the live turn becomes its final message", async () => {
  const turn = { requester_thread_id: null, requester_turn_id: null, id: 91, thread_id: 1, owner_message_id: 90, agent_message_id: null, state: "running" as const, started_at: "2026-09-09T10:00:00Z", finished_at: null };
  flush(() => {
    handleThreadMessage({ type: "msg", message: { id: 90, thread_id: 1, author: "owner", body: "Check this file", ref: null, ts: turn.started_at } });
    handleThreadMessage({ type: "thread_turn", turn });
    handleThreadMessage({ type: "turn_event", thread_id: 1, turn_id: 91, seq: 1, event: { kind: "reasoning", text: "Checking the requested file." } });
    handleThreadMessage({ type: "turn_event", thread_id: 1, turn_id: 91, seq: 2, event: { kind: "tool_done", id: "call-a", name: "read_file", ok: true, summary: "Exact file contents", result: null } });
    handleThreadMessage({ type: "turn_event", thread_id: 1, turn_id: 91, seq: 3, event: { kind: "prose", text: "File checked" } });
  });
  const view = render(() => <ThreadShell />);
  const result = view.getByRole("button", { name: "read_file — show result" });
  fireEvent.click(result); result.focus();
  // A queued owner message can arrive while this response is still running.
  flush(() => handleThreadMessage({ type: "msg", message: { id: 92, thread_id: 1, author: "owner", body: "Then check the next file", ref: null, ts: "2026-09-09T10:00:01Z" } }));
  flush(() => handleThreadMessage({ type: "msg", message: { id: 93, thread_id: 1, author: "agent", body: "File checked", ref: 90, ts: "2026-09-09T10:00:02Z", tool_calls: [{ id: "call-a", name: "read_file", ok: true }] } }));
  flush(() => handleThreadMessage({ type: "thread_turn", turn: { ...turn, agent_message_id: 93, state: "completed", finished_at: "2026-09-09T10:00:02Z" } }));
  expect(view.getByRole("button", { name: "read_file — hide result" })).toBe(result);
  await waitFor(() => expect(document.activeElement).toBe(result));
  expect(view.getAllByText("Exact file contents")).toHaveLength(1);
  expect(view.getAllByText("File checked")).toHaveLength(1);
  const timeline = view.container.querySelector('[data-message-id="93"] [data-slot="timeline"]')!;
  const reply = view.getByText("File checked");
  expect([...timeline.children].map(row => row.getAttribute("data-slot"))).toEqual(["timeline-reasoning", "timeline-tool"]);
  expect(timeline.compareDocumentPosition(reply) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
});

describe("nested Thread workspace", () => {
  it("creates a child under the selected row even when the conversation changes", async () => {
    const view = render(() => <ThreadShell />);
    fireEvent.click(view.getByRole("button", { name: "Threads" }));
    fireEvent.click(view.getByRole("button", { name: "Actions for Holiday" }));
    fireEvent.click(view.getByRole("menuitem", { name: "New child thread" }));
    flush(() => focusThread(0));
    fireEvent.input(view.getByLabelText("New thread title"), { target: { value: "Review" } });
    fireEvent.click(view.getByRole("button", { name: "Create thread" }));
    const frame = sent.findLast(frame => frame.type === "create_thread");
    expect(frame).toMatchObject({ parent_thread_id: 2, title: "Review" });
    if (frame?.type !== "create_thread") throw new Error("Missing create");
    flush(() => handleThreadMessage({ type: "thread_created", client_id: frame.client_id, thread: makeThread(9, { title: "Review", parent_thread_id: 2 }) }));
    await waitFor(() => expect(threadState.focusedId).toBe(9));
    expect(view.getByRole("navigation", { name: "Thread ancestry" })).toHaveTextContent("#2 Holiday");
    expect(view.getByRole("textbox", { name: "Message Review" })).toBeInTheDocument();
  });
  it("pins a top-level Thread first once within its lifecycle filter", async () => {
    const view = render(() => <ThreadShell />);
    fireEvent.click(view.getByRole("button", { name: "Thread actions" }));
    fireEvent.click(within(document.body).getByRole("menuitem", { name: "Pin thread" }));
    expect(sent).toContainEqual(expect.objectContaining({ type: "thread_action", history_id: "test-history", thread_id: 1, action: "pin", data: {}, expected_revision: 1 }));
    flush(() => handleThreadMessage({ type: "thread_upsert", thread: makeThread(1, { read: true, pinned_at: "2026-09-10T10:00:00Z", revision: 2 }) }));
    fireEvent.click(view.getByRole("button", { name: "Threads" }));
    expect(view.container.querySelectorAll('[data-thread-row="1"]')).toHaveLength(1);
    expect(view.container.querySelector("[data-thread-row]")).toHaveAttribute("data-thread-row", "1");
    fireEvent.click(view.getByRole("button", { name: "Filter threads: active" }));
    fireEvent.click(view.getByRole("menuitemradio", { name: "archived" }));
    expect(view.container.querySelector('[data-row-key="pin:1"]')).toBeNull();
    expect(view.container.querySelector('[data-row-key="tree:1"]')).toBeNull();
    expect(threadState.focusedId).toBe(1);
    expect(threadState.threads.find(thread => thread.id === 1)?.settled_at).toBeNull();
  });
  it("keeps child pin controls absent and retains its parentage even with a legacy pin", async () => {
    flush(() => setThreadState(draft => { draft.threads[1].parent_thread_id = 0; draft.threads[1].pinned_at = "2026-09-10T10:00:00Z"; }));
    const view = render(() => <ThreadShell />);
    fireEvent.click(view.getByRole("button", { name: "Thread actions" }));
    expect(view.queryByRole("menuitem", { name: "Pin thread" })).toBeNull();
    expect(view.queryByRole("menuitem", { name: "Unpin thread" })).toBeNull();
    fireEvent.keyDown(window, { key: "Escape" });
    fireEvent.click(view.getByRole("button", { name: "Threads" }));
    const rows = view.container.querySelectorAll('[data-thread-row]');
    expect(Array.from(rows).map(row => row.getAttribute("data-thread-row"))).toEqual(["0", "1", "2"]);
    expect(view.container.querySelector('[data-thread-row="1"]')).not.toHaveTextContent("Pinned");
    expect(view.container.querySelector('[data-slot="thread-branch-guide"]')).toBeInTheDocument();
    expect(view.getByRole("dialog", { name: "Threads" })).not.toHaveTextContent("No turns yet");
  });
  it("expands selected ancestry and navigates a tree without changing the composer", async () => {
    flush(() => setThreadState(draft => { draft.threads = [makeThread(1, { title: "Website" }), makeThread(2, { title: "Review", parent_thread_id: 1 }), makeThread(3, { title: "Review", parent_thread_id: 2 })]; draft.focusedId = 3; }));
    const view = render(() => <ThreadShell />);
    fireEvent.click(view.getByRole("button", { name: "Threads" }));
    const row = view.container.querySelector<HTMLButtonElement>('[data-row-key="tree:3"]')!;
    await waitFor(() => expect(row).toHaveFocus());
    fireEvent.keyDown(row, { key: "ArrowLeft" });
    const parent = view.container.querySelector<HTMLButtonElement>('[data-row-key="tree:2"]')!;
    expect(parent).toHaveFocus();
    fireEvent.keyDown(parent, { key: "ArrowLeft" });
    expect(view.container.querySelector('[data-row-key="tree:3"]')).toBeNull();
    expect(threadState.focusedId).toBe(3);
    fireEvent.keyDown(parent, { key: "ArrowRight" });
    expect(view.container.querySelector('[data-row-key="tree:3"]')).toBeInTheDocument();
  });
  it("renders child reports chronologically once with exact links and artifact references", () => {
    flush(() => setThreadState(draft => {
      draft.threads[2].parent_thread_id = 1;
      draft.histories[1] = { brief: { text: "", artifact_ids: [] }, loaded: true, hasMore: false, turns: [], messages: [{ id: 20, thread_id: 1, author: "owner", body: "Please review", ref: null, ts: "2026-09-10T10:00:00Z" }], activities: [] };
    }));
    const report = { artifact_ids: [44], id: 33, thread_id: 1, turn_id: null, kind: "child_report", data: { child_thread_id: 2, child_turn_id: 90, requester_turn_id: 80, report_seq: 1, status: "completed", summary: "Review complete. Two issues fixed." }, ts: "2026-09-10T10:01:00Z" };
    flush(() => { handleThreadMessage({ type: "thread_activity", activity: report }); handleThreadMessage({ type: "thread_activity", activity: report }); });
    const view = render(() => <ThreadShell />);
    const card = view.container.querySelector('[data-activity-id="33"]')!;
    expect(card).toHaveTextContent("#2 Holiday");
    expect(card).toHaveTextContent("completed");
    expect(card).toHaveTextContent("Turn 90");
    expect(card).toHaveTextContent("Review complete. Two issues fixed.");
    expect(view.container.querySelectorAll('[data-activity-id="33"]')).toHaveLength(1);
    expect(card.querySelector('[data-slot="work-details"]')).toBeNull();
    expect(within(card as HTMLElement).getByRole("button", { name: /Artifact #44/ })).toBeInTheDocument();
    fireEvent.click(within(card as HTMLElement).getByRole("link", { name: "#2 Holiday" }));
    expect(threadState.focusedId).toBe(2);
    expect(view.queryByText("Review complete. Two issues fixed.")).toBeNull();
  });
  it("does not expose a composer for missing explicit destinations", () => {
    flush(() => focusThread(99));
    const view = render(() => <ThreadShell />);
    expect(view.getByRole("heading", { name: "Thread #99 is unavailable" })).toBeInTheDocument();
    expect(view.container.querySelector("textarea")).toBeNull();
    expect(threadState.focusedId).toBe(99);
  });
});

it("keeps current brief reachable beyond the visible history page without moving its assignment", async () => {
  const view = render(() => <ThreadShell />);
  flush(() => focusThread(1));
  const frame = sent.findLast(frame => frame.type === "open_thread");
  if (frame?.type !== "open_thread") throw new Error("Missing open");
  const assignment = { id: 10, thread_id: 1, turn_id: 9, kind: "delegation_received", artifact_ids: [44], data: { requester_thread_id: 0, requester_turn_id: null, brief: "Review only the keyboard flow." }, ts: "2026-09-01T10:00:00Z" };
  flush(() => handleThreadMessage({ type: "thread_opened", client_id: frame.client_id, detail: {
    thread: makeThread(1, { parent_thread_id: 0 }), brief: { text: "Review only the keyboard flow.", artifact_ids: [44] }, related_items: [], has_more: true,
    messages: [{ id: 100, thread_id: 1, author: "owner", body: "Latest conversation", ref: null, ts: "2026-09-10T10:00:00Z" }], turns: [], turn_timelines: [], activities: [assignment],
  } }));
  expect(view.container.querySelector('[data-activity-id="10"]')).toBeNull();
  expect(view.container.querySelector('[data-slot="thread-brief"]')).toBeInTheDocument();
  fireEvent.click(view.getByText("Current brief", { exact: true }));
  const brief = view.container.querySelector('[data-slot="thread-brief"]')!;
  expect(brief).toHaveTextContent("Review only the keyboard flow.");
  expect(brief.querySelector('[data-artifact-ref="44"]')).toBeInTheDocument();
  expect(view.getByText("Latest conversation")).toBeInTheDocument();
  expect(threadState.histories[1].activities[0].ts).toBe(assignment.ts);
  flush(() => handleThreadMessage({ type: "thread_activity", activity: { ...assignment, id: 11, data: { ...assignment.data, brief: "Now review the phone flow." }, ts: "2026-09-10T10:01:00Z" } }));
  expect(brief).toHaveTextContent("Now review the phone flow.");
  expect(threadState.histories[1].activities[0].data).toEqual(assignment.data);
});


describe("contextual Thread errors", () => {
  it("keeps send recovery in its addressed conversation while the drawer is open", () => {
    const view = render(() => <ThreadShell />);
    flush(() => sendThreadMessage("test-history", 1, "Keep this failed draft", "send", [], [], [44]));
    const outgoing = sent.findLast(frame => frame.type === "send_thread_message")!;
    if (outgoing.type !== "send_thread_message") throw new Error("Missing send");
    flush(() => handleThreadMessage({type:"error",client_id:outgoing.client_id,detail:"Send unavailable"}));
    const main = view.container.querySelector("main")!;
    expect(within(main).getByRole("alert")).toHaveTextContent("Your message wasn’t confirmed");
    fireEvent.click(view.getByRole("button", {name:"Threads"}));
    const drawer = view.getByRole("dialog", {name:"Threads"});
    expect(within(drawer).queryByRole("alert")).toBeNull();
    expect(within(drawer).queryByText(/Use Retry beside/)).toBeNull();
    expect(threadState.pending[0].artifactIds).toEqual([44]);
    fireEvent.click(within(drawer).getByRole("button", {name:"Close threads"}));
    fireEvent.click(within(main).getByRole("button", {name:"Retry"}));
    expect(sent.at(-1)).toEqual(outgoing);
    flush(() => handleThreadMessage({type:"msg",message:{id:40,thread_id:1,author:"owner",body:outgoing.body,client_id:outgoing.client_id,artifact_ids:[44],ref:null,ts:"2026-09-10T10:00:00Z"}}));
    expect(within(main).queryByRole("alert")).toBeNull();
    flush(() => sendThreadMessage("test-history", 2, "Other Thread failure", "send", [], [], []));
    const other = sent.findLast(frame => frame.type === "send_thread_message")!;
    if (other.type !== "send_thread_message") throw new Error("Missing other send");
    flush(() => handleThreadMessage({type:"error",client_id:other.client_id,detail:"Other Thread unavailable"}));
    expect(within(main).queryByRole("alert")).toBeNull();
    expect(within(main).queryByText("Other Thread failure")).toBeNull();
  });

  it("shows a failed drawer action without attributing it to the current conversation", async () => {
    const view = render(() => <ThreadShell />);
    fireEvent.click(view.getByRole("button", {name:"Threads"}));
    const drawer = view.getByRole("dialog", {name:"Threads"});
    fireEvent.click(within(drawer).getByRole("button", {name:"Actions for Holiday"}));
    fireEvent.click(within(await view.findByRole("menu", {name:"Actions for Holiday"})).getByRole("menuitem", {name:"Archive thread"}));
    const action = sent.findLast(frame => frame.type === "thread_action");
    if (action?.type !== "thread_action") throw new Error("Missing action");
    expect(action).toMatchObject({history_id:"test-history",thread_id:2,action:"archive"});
    flush(() => handleThreadMessage({type:"error",client_id:action.client_id,detail:"Archive rejected"}));
    await waitFor(() => expect(within(drawer).getByRole("alert")).toHaveTextContent("Hirsel couldn’t complete that request"));
    expect(threadState.error).toMatchObject({operation:"request",threadId:2,clientId:action.client_id,detail:"Archive rejected"});
    expect(within(view.container.querySelector("main")!).queryByRole("alert")).toBeNull();
    fireEvent.click(within(drawer).getByRole("button", {name:"Dismiss"}));
    expect(within(drawer).queryByRole("alert")).toBeNull();
    expect(threadState.focusedId).toBe(1);
  });
});

describe("retry keyboard focus", () => {
  async function retryFixture() {
    const view = render(() => <ThreadShell />);
    flush(() => sendThreadMessage("test-history", 1, "Keyboard recovery", "send", [], [], [44]));
    const frame = sent.findLast(frame => frame.type === "send_thread_message")!;
    if (frame.type !== "send_thread_message") throw new Error("Missing send");
    flush(() => handleThreadMessage({type:"error",client_id:frame.client_id,detail:"Temporary failure"}));
    const retry = within(view.container.querySelector("main")!).getByRole("button", {name:"Retry"});
    retry.focus(); fireEvent.click(retry);
    expect(sent.at(-1)).toEqual(frame);
    const accept = () => flush(() => handleThreadMessage({type:"msg",message:{id:50,thread_id:1,author:"owner",body:frame.body,client_id:frame.client_id,artifact_ids:[44],ref:null,ts:"2026-09-10T10:00:00Z"}}));
    return {view,accept};
  }
  it("returns an accepted focused Retry to its addressed composer", async () => {
    const {view,accept} = await retryFixture();
    accept();
    await waitFor(() => expect(view.container.querySelector('[data-thread-id="1"] textarea')).toHaveFocus());
    expect(threadState.pending).toEqual([]);
  });
  it.each(["control", "drawer", "thread", "history"] as const)("does not reclaim focus after the user moves to %s", async destination => {
    const {view,accept} = await retryFixture();
    let target: HTMLElement | null = null;
    if (destination === "control") { target = view.getByRole("button", {name:"Thread actions"}); target.focus(); }
    if (destination === "drawer") {
      fireEvent.click(view.getByRole("button", {name:"Threads"}));
      const drawer = view.getByRole("dialog", {name:"Threads"});
      await waitFor(() => expect(drawer.contains(document.activeElement)).toBe(true));
      target = document.activeElement as HTMLElement;
    }
    if (destination === "thread") { flush(() => focusThread(2)); target = view.container.querySelector('[data-thread-id="2"] textarea'); target!.focus(); }
    if (destination === "history") flush(() => setHistoryId("new-history"));
    accept();
    await new Promise<void>(resolve => queueMicrotask(resolve));
    if (target) expect(target).toHaveFocus();
    else expect(view.container.querySelector('[data-thread-id="1"] textarea')).not.toHaveFocus();
  });
});
