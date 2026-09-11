import { flush } from "solid-js";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ChatMessage } from "../protocol";
import type { ThreadClientMessage, ThreadDetail } from "./types";
import { makeThread } from "./fixtures";
import { attachThreadTransport, createThread, disconnectThreads, focusThread, handleThreadMessage, openThread, resetThreads, retryThreadMessage, sendThreadMessage, setThreadState, threadAction, threadState } from "./store";
import { setHistoryId } from "../lib/history";
const sent: ThreadClientMessage[] = [];
beforeEach(() => { const storage=new Map<string,string>();vi.stubGlobal("localStorage",{getItem:(key:string)=>storage.get(key)??null,setItem:(key:string,value:string)=>storage.set(key,value),removeItem:(key:string)=>storage.delete(key)}); sent.length = 0; history.replaceState(null, "", "/"); setHistoryId("test-history"); flush(() => setThreadState(draft => { Object.assign(draft, { ready: true, linkError: null, threads: [], histories: {}, turnDetails: {}, removedMessageIds: {}, pending: [], focusedId: 0, error: null }); })); attachThreadTransport(frame => sent.push(frame)); });
afterEach(() => { disconnectThreads(); vi.useRealTimers();vi.unstubAllGlobals(); });
const detail = (id: number): ThreadDetail => ({ brief: { text: "", artifact_ids: [] }, thread: makeThread(id), messages: [], turns: [], turn_timelines: [], activities: [], related_items: [], has_more: false });
describe("thread transport projection", () => {
  it("clears an archived focused Thread only after its correlated action is accepted, preserving history and draft", async () => {
    const retained = { brief: { text: "", artifact_ids: [] }, messages: [{ id: 1, thread_id: 1, author: "owner" as const, body: "Keep this conversation", ref: null, ts: "2026-09-10T10:00:00Z" }], turns: [], activities: [], loaded: true, hasMore: false };
    flush(() => setThreadState(draft => { draft.threads = [makeThread(1), makeThread(2)]; draft.histories[1] = retained; }));
    localStorage.setItem("hirsel.draft.test-history:thread-1", "Keep this draft");
    flush(() => focusThread(1));
    flush(() => threadAction("test-history", 1, "archive"));
    const archive = sent.findLast(frame => frame.type === "thread_action");
    if (archive?.type !== "thread_action") throw new Error("Missing archive action");

    expect(threadState.focusedId).toBe(1);
    flush(() => handleThreadMessage({ type: "thread_action_applied", client_id: archive.client_id, history_id: archive.history_id, thread_id: archive.thread_id }));
    await Promise.resolve();

    expect(threadState.focusedId).toBeNull();
    expect(location.pathname).toBe("/");
    expect(localStorage.getItem("hirsel.last-thread.test-history")).toBeNull();
    expect(threadState.histories[1]).toEqual(retained);
    expect(localStorage.getItem("hirsel.draft.test-history:thread-1")).toBe("Keep this draft");
  });

  it("does not let a delayed archive acceptance clear a selection that moved away and back", async () => {
    flush(() => setThreadState(draft => { draft.threads = [makeThread(1), makeThread(2)]; }));
    flush(() => focusThread(1));
    flush(() => threadAction("test-history", 1, "archive"));
    const archive = sent.findLast(frame => frame.type === "thread_action");
    if (archive?.type !== "thread_action") throw new Error("Missing archive action");
    flush(() => focusThread(2));
    flush(() => focusThread(1));

    flush(() => handleThreadMessage({ type: "thread_action_applied", client_id: archive.client_id, history_id: archive.history_id, thread_id: archive.thread_id }));
    await Promise.resolve();

    expect(threadState.focusedId).toBe(1);
    expect(location.pathname).toBe("/t/1");
  });

  it("clears only a focused Thread archived by an authoritative upsert", () => {
    flush(() => setThreadState(draft => { draft.threads = [makeThread(1), makeThread(2)]; }));
    flush(() => focusThread(1));
    flush(() => handleThreadMessage({ type: "thread_upsert", thread: makeThread(2, { archived_at: "2026-09-10T10:00:00Z", revision: 2 }) }));
    expect(threadState.focusedId).toBe(1);

    flush(() => handleThreadMessage({ type: "thread_upsert", thread: makeThread(1, { archived_at: "2026-09-10T10:00:00Z", revision: 2 }) }));
    expect(threadState.focusedId).toBeNull();
    expect(location.pathname).toBe("/");
  });

  it("keeps an explicitly selected archived Thread focused across later archived upserts", () => {
    flush(() => setThreadState(draft => { draft.threads = [makeThread(1, { archived_at: "2026-09-10T10:00:00Z" })]; }));
    flush(() => focusThread(1));
    flush(() => handleThreadMessage({ type: "thread_upsert", thread: makeThread(1, { archived_at: "2026-09-10T10:00:00Z", read: true, revision: 2 }) }));
    expect(threadState.focusedId).toBe(1);
    expect(location.pathname).toBe("/t/1");
  });

  it("ignores an older archived upsert when the focused Thread has a newer active revision", () => {
    flush(() => setThreadState(draft => { draft.threads = [makeThread(1, { revision: 3 })]; }));
    flush(() => focusThread(1));
    flush(() => handleThreadMessage({ type: "thread_upsert", thread: makeThread(1, { archived_at: "2026-09-10T10:00:00Z", revision: 2 }) }));
    expect(threadState.focusedId).toBe(1);
    expect(threadState.threads[0].archived_at).toBeNull();
    expect(location.pathname).toBe("/t/1");
  });

  it("rejects delayed mutations captured from the history before an ID was reused", async () => {
    const capturedHistory = "test-history";
    flush(() => setHistoryId("replacement-history"));

    expect(() => sendThreadMessage(capturedHistory, 1, "stale", "send", [], [], [])).toThrow("History changed");
    await expect(createThread(capturedHistory, "Stale child", "task", 1)).rejects.toThrow("History changed");
    flush(() => threadAction(capturedHistory, 1, "archive"));

    expect(sent).toEqual([]);
    expect(threadState.pending).toEqual([]);
    expect(threadState.error?.detail).toContain("History changed");
  });
  it("creates a visible thread before any message and accepts its duplicate broadcast once", async () => {
    const promise = createThread("test-history", "Buy groceries", "task", null);
    const frame = sent[0];
    if (frame.type !== "create_thread") throw new Error("wrong command");
    expect(frame).toMatchObject({ title: "Buy groceries", kind: "task", parent_thread_id: null, history_id: "test-history" });
    flush(() => handleThreadMessage({ type: "thread_created", client_id: frame.client_id, thread: makeThread() }));
    await expect(promise).resolves.toMatchObject({ id: 1 });
    flush(() => handleThreadMessage({ type: "thread_upsert", thread: makeThread() }));
    expect(threadState.threads).toHaveLength(1);
  });
  it("isolates late open responses from the currently selected thread", async () => {
    const first = openThread(1);
    const second = openThread(2);
    flush(() => setThreadState(draft => { draft["focusedId"] = 2; }));
    const frames = sent.filter(f => f.type === "open_thread");
    flush(() => handleThreadMessage({ type: "thread_opened", client_id: frames[1].client_id, detail: detail(2) }));
    flush(() => handleThreadMessage({ type: "thread_opened", client_id: frames[0].client_id, detail: detail(1) }));
    await Promise.all([first, second]);
    expect(threadState.focusedId).toBe(2);
    expect(threadState.histories[1].loaded).toBe(true);
    expect(threadState.histories[2].loaded).toBe(true);
  });
  it("does not let a stale older page regress a newer live terminal turn", async () => {
    const page = openThread(1, 100);
    const frame = sent.at(-1);
    if (frame?.type !== "open_thread") throw new Error("expected open command");
    const completed = { requester_thread_id: null, requester_turn_id: null, id: 10, thread_id: 1, owner_message_id: 9, agent_message_id: 11, state: "completed" as const, started_at: "2026-09-10T10:00:00Z", finished_at: "2026-09-10T10:00:05Z" };
    flush(() => handleThreadMessage({ type: "thread_turn", turn: completed }));
    const staleRunning = { ...completed, state: "running" as const, agent_message_id: null, finished_at: null };
    const older = { id: 8, thread_id: 1, author: "owner" as const, body: "Earlier context", ref: null, ts: "2026-09-10T09:59:00Z" };
    flush(() => handleThreadMessage({ type: "thread_opened", client_id: frame.client_id, detail: { ...detail(1), messages: [older], turns: [staleRunning], has_more: true } }));
    await page;
    expect(threadState.histories[1].turns).toEqual([completed]);
    expect(threadState.histories[1].messages).toEqual([older]);
  });
  it("correlates identical outgoing text by client id and preserves ownership across replay", () => {
    flush(() => sendThreadMessage("test-history", 1, "same", "send", [], [2], []));
    flush(() => sendThreadMessage("test-history", 2, "same", "send", [], [1], []));
    const second = threadState.pending[1].clientId;
    flush(() => handleThreadMessage({ type: "msg", message: { id: 5, thread_id: 2, client_id: second, author: "owner", body: "same", ref: null, ts: "2026-09-09T10:00:00Z", mentions: [1] } }));
    expect(threadState.pending.map(p => p.threadId)).toEqual([1]);
    expect(threadState.histories[1]).toBeUndefined();
    expect(threadState.histories[2].messages).toHaveLength(1);
  });
  it("fails a missing acknowledgement and retries the same identity without duplicating content", () => {
    vi.useFakeTimers();
    flush(() => sendThreadMessage("test-history", 1, "Buy groceries", "send", [], [2], []));
    const clientId = threadState.pending[0].clientId;
    flush(() => vi.advanceTimersByTime(20_000));
    expect(threadState.pending[0].failed).toBe(true);
    flush(() => retryThreadMessage(clientId));
    expect(threadState.pending[0].failed).toBe(false);
    expect(sent[1]).toEqual(sent[0]);
    flush(() => handleThreadMessage({ type: "msg", message: { id: 7, thread_id: 1, client_id: clientId, author: "owner", body: "Buy groceries", ref: null, ts: "2026-09-09T10:00:00Z" } }));
    flush(() => vi.advanceTimersByTime(20_000));
    expect(threadState.pending).toEqual([]);
    expect(threadState.histories[1].messages).toHaveLength(1);
  });
  it("preserves back-to-back protocol records before the reactive microtask commits", () => {
    handleThreadMessage({ type: "thread_upsert", thread: makeThread(1) });
    handleThreadMessage({ type: "thread_upsert", thread: makeThread(2) });
    handleThreadMessage({ type: "turn_event", thread_id: 1, turn_id: 10, seq: 1, event: { kind: "prose", text: "old" } });
    handleThreadMessage({ type: "turn_event", thread_id: 1, turn_id: 11, seq: 1, event: { kind: "prose", text: "new" } });
    handleThreadMessage({ type: "turn_event", thread_id: 1, turn_id: 11, seq: 2, event: { kind: "prose", text: " next" } });
    flush();
    expect(threadState.threads.map(thread => thread.id).sort((a, b) => a - b)).toEqual([1, 2]);
    expect(threadState.turnDetails[11].map(row => row.event)).toEqual([{ kind: "prose", text: "new" }, { kind: "prose", text: " next" }]);
  });
  it("merges a reconnect snapshot with newer live events by exact turn sequence", async () => {
    const opening = openThread(1);
    const frame = sent.at(-1);
    if (frame?.type !== "open_thread") throw new Error("expected open command");
    const turn = { requester_thread_id: null, requester_turn_id: null, id: 31, thread_id: 1, owner_message_id: 30, agent_message_id: null, state: "running" as const, started_at: "2026-09-10T10:00:00Z", finished_at: null };
    const second = { seq: 2, event: { kind: "prose" as const, text: "newer live" } };
    flush(() => handleThreadMessage({ type: "turn_event", thread_id: 1, turn_id: 31, ...second }));
    flush(() => handleThreadMessage({ type: "thread_opened", client_id: frame.client_id, detail: {
      ...detail(1),
      messages: [{ id: 30, thread_id: 1, author: "owner", body: "Reload", ref: null, ts: turn.started_at }],
      turns: [turn],
      turn_timelines: [{ turn_id: 31, events: [{ seq: 1, event: { kind: "reasoning", text: "persisted first" } }, second] }],
    } }));
    await opening;
    expect(threadState.turnDetails[31]).toEqual([
      { seq: 1, event: { kind: "reasoning", text: "persisted first" } },
      second,
    ]);
    flush(() => handleThreadMessage({ type: "turn_event", thread_id: 1, turn_id: 31, ...second }));
    expect(threadState.turnDetails[31]).toHaveLength(2);
    flush(() => handleThreadMessage({ type: "turn_event", thread_id: 1, turn_id: 31, seq: 2, event: { kind: "prose", text: "conflict" } }));
    expect(threadState.error?.detail).toContain("Conflicting timeline event");
    expect(threadState.turnDetails[31][1]).toEqual(second);
  });
  it("routes live timeline events only to their owning thread and binds instrument revision", () => {
    flush(() => handleThreadMessage({ type: "turn_event", thread_id: 2, turn_id: 1, seq: 1, event: { kind: "prose", text: "working" } }));
    expect(threadState.turnDetails[1]).toHaveLength(1);
    flush(() => threadAction("test-history", 2, "choose", { choice: "a" }, 7));
    expect(sent[0]).toMatchObject({ type: "thread_action", history_id: "test-history", thread_id: 2, action: "choose", data: { choice: "a" }, expected_revision: 7 });
  });
  it("settles only the exact correlated action on success, failure and timeout", async () => {
    vi.useFakeTimers();
    flush(() => threadAction("test-history", 2, "archive"));
    flush(() => threadAction("test-history", 2, "read"));
    const [archive, read] = sent.filter(frame => frame.type === "thread_action");
    if (archive?.type !== "thread_action" || read?.type !== "thread_action") throw new Error("Missing actions");
    expect(archive.client_id).not.toBe(read.client_id);

    flush(() => handleThreadMessage({ type: "error", client_id: archive.client_id, detail: "Archive rejected" }));
    await Promise.resolve();
    await Promise.resolve();
    expect(threadState.error).toMatchObject({ operation: "request", detail: "Archive rejected", threadId: 2, clientId: archive.client_id });

    flush(() => handleThreadMessage({ type: "thread_action_applied", client_id: read.client_id, history_id: read.history_id, thread_id: read.thread_id }));
    await Promise.resolve();
    await Promise.resolve();
    expect(threadState.error?.clientId).toBe(archive.client_id);

    flush(() => threadAction("test-history", 2, "settle"));
    const timedOut = sent.findLast(frame => frame.type === "thread_action");
    if (timedOut?.type !== "thread_action") throw new Error("Missing timeout action");
    await vi.advanceTimersByTimeAsync(20_000);
    expect(threadState.error).toMatchObject({ detail: "Thread request timed out", threadId: 2, clientId: timedOut.client_id });
    flush(() => handleThreadMessage({ type: "thread_action_applied", client_id: timedOut.client_id, history_id: timedOut.history_id, thread_id: timedOut.thread_id }));
    await Promise.resolve();
    await Promise.resolve();
    expect(threadState.error?.clientId).toBe(timedOut.client_id);
  });
  it("drops old-history and duplicate action results while preserving global errors", async () => {
    flush(() => threadAction("test-history", 2, "archive"));
    const old = sent.at(-1);
    if (old?.type !== "thread_action") throw new Error("Missing old action");
    flush(() => resetThreads());
    flush(() => setHistoryId("replacement-history"));
    flush(() => handleThreadMessage({ type: "thread_action_applied", client_id: old.client_id, history_id: old.history_id, thread_id: old.thread_id }));
    flush(() => handleThreadMessage({ type: "error", client_id: old.client_id, detail: "Late old failure" }));
    await Promise.resolve();
    await Promise.resolve();
    expect(threadState.error).toBeNull();

    flush(() => handleThreadMessage({ type: "error", detail: "Host storage unavailable" }));
    expect(threadState.error).toEqual({ operation: "request", detail: "Host storage unavailable" });
  });
  it("resets sequence on a new turn and rejects late events or completion from the previous turn", () => {
    flush(() => handleThreadMessage({ type: "turn_event", thread_id: 1, turn_id: 10, seq: 1, event: { kind: "prose", text: "old" } }));
    flush(() => handleThreadMessage({ type: "turn_event", thread_id: 1, turn_id: 11, seq: 1, event: { kind: "prose", text: "new" } }));
    flush(() => handleThreadMessage({ type: "turn_event", thread_id: 1, turn_id: 10, seq: 2, event: { kind: "prose", text: "late" } }));
    flush(() => handleThreadMessage({ type: "thread_turn", turn: { requester_thread_id: null, requester_turn_id: null, id: 10, thread_id: 1, owner_message_id: 1, agent_message_id: 2, state: "completed", started_at: "2026-09-09T10:00:00Z", finished_at: "2026-09-09T10:00:01Z" } }));
    expect(threadState.turnDetails[11].map(row => row.event)).toEqual([{ kind: "prose", text: "new" }]);
    expect(threadState.turnDetails[10].map(row => row.event)).toEqual([{ kind: "prose", text: "old" }, { kind: "prose", text: "late" }]);
  });
  it("removes cancelled messages and rejects delayed echoes, open pages and reconnect snapshots", async () => {
    const cancelled: ChatMessage = { id: 9, thread_id: 1, author: "owner", body: "Cancelled work", ref: null, ts: "2026-09-09T10:00:00Z" };
    const retained: ChatMessage = { ...cancelled, id: 10, thread_id: 2, body: "Other work" };
    flush(() => handleThreadMessage({ type: "msg", message: cancelled }));
    flush(() => handleThreadMessage({ type: "msg", message: retained }));
    flush(() => setThreadState(draft => {
      draft.histories[1].turns = [{ requester_thread_id: null, requester_turn_id: null, id: 90, thread_id: 1, owner_message_id: cancelled.id, agent_message_id: null, state: "cancelled", started_at: cancelled.ts, finished_at: cancelled.ts }];
      draft["turnDetails"][90] = [{ seq: 1, event: { kind: "prose", text: "old detail" } }];
    }));
    const opening = openThread(1);
    const oldOpen = sent.at(-1);
    if (oldOpen?.type !== "open_thread") throw new Error("expected open command");
    flush(() => handleThreadMessage({ type: "msg_removed", id: cancelled.id }));
    expect(threadState.histories[1].messages).toEqual([]);
    expect(threadState.histories[2].messages).toEqual([retained]);
    expect(threadState.turnDetails[90]).toBeUndefined();
    flush(() => handleThreadMessage({ type: "msg", message: cancelled }));
    flush(() => handleThreadMessage({ type: "thread_opened", client_id: oldOpen.client_id, detail: { ...detail(1), messages: [cancelled] } }));
    await opening;
    expect(threadState.histories[1].messages).toEqual([]);
    disconnectThreads();
    attachThreadTransport(frame => sent.push(frame));
    flush(() => setThreadState(draft => { draft["focusedId"] = 1; }));
    flush(() => handleThreadMessage({ type: "hello_ok", threads: [makeThread(1), makeThread(2)], history_id: "test-history", processes: [], views: [], host_version: "test", model: null, subagent_models: null, prompts: null, providers: null }));
    const reconnectOpen = sent.at(-1);
    if (reconnectOpen?.type !== "open_thread") throw new Error("expected reconnect open command");
    flush(() => handleThreadMessage({ type: "thread_opened", client_id: reconnectOpen.client_id, detail: { ...detail(1), messages: [cancelled] } }));
    expect(threadState.histories[1].messages).toEqual([]);
    expect(threadState.histories[2].messages).toEqual([retained]);
  });
  it("remembers a tombstone received before a message and acknowledges its delayed owner echo", () => {
    flush(() => sendThreadMessage("test-history", 1, "Cancelled before echo", "next_turn", [], [], []));
    const clientId = threadState.pending[0].clientId;
    flush(() => handleThreadMessage({ type: "msg_removed", id: 12 }));
    flush(() => handleThreadMessage({ type: "msg", message: { id: 12, thread_id: 1, client_id: clientId, author: "owner", body: "Cancelled before echo", ref: null, ts: "2026-09-09T10:00:00Z" } }));
    expect(threadState.histories[1]?.messages ?? []).toEqual([]);
    expect(threadState.pending).toEqual([]);
  });
  it("rejects disconnected requests and lets retry errors remain visible", async () => {
    const promise = openThread(1);
    disconnectThreads();
    await expect(promise).rejects.toThrow("Connection interrupted");
    flush(() => threadAction("test-history", 1, "settle"));
    expect(threadState.error?.detail).toMatch(/Reconnect/);
  });
});

describe("running turn with a newer queued request", () => {
  it("admits the running turn, ignores queued cancellation, then moves to the next actual run", () => {
    const turn = (id: number, state: "running" | "queued" | "completed" | "cancelled", agent_message_id: number | null = null) => ({ requester_thread_id: null, requester_turn_id: null, id, thread_id: 1, owner_message_id: id, agent_message_id, state, started_at: "2026-09-09T10:00:00Z", finished_at: state === "completed" || state === "cancelled" ? "2026-09-09T10:00:01Z" : null });
    const stream = (turn_id:number,seq:number,text:string) => handleThreadMessage({type:"turn_event",thread_id:1,turn_id,seq,event:{kind:"prose",text}});
    flush(() => { handleThreadMessage({type:"thread_turn",turn:turn(10,"running")}); stream(10,1,"first"); handleThreadMessage({type:"thread_turn",turn:turn(11,"queued")}); stream(10,2,"second"); });
    expect(threadState.turnDetails[10]).toHaveLength(2);
    flush(() => handleThreadMessage({type:"thread_turn",turn:turn(11,"cancelled")}));
    expect(threadState.turnDetails[10]).toHaveLength(2);
    flush(() => handleThreadMessage({type:"thread_turn",turn:turn(10,"completed",20)}));
    expect(threadState.turnDetails[10]).toHaveLength(2);
    flush(() => { handleThreadMessage({type:"thread_turn",turn:turn(12,"running")}); stream(12,1,"new"); stream(10,3,"late"); });
    expect(threadState.turnDetails[12].map(row=>row.event)).toEqual([{kind:"prose",text:"new"}]);
    expect(threadState.turnDetails[10]).toHaveLength(3);
  });

  it.each(["failed", "cancelled", "interrupted"] as const)("retains the exact live timeline when a turn ends %s without a final message", state => {
    const turn = { requester_thread_id: null, requester_turn_id: null, id: 20, thread_id: 1, owner_message_id: 7, agent_message_id: null, state: "running" as const, started_at: "2026-09-09T10:00:00Z", finished_at: null };
    flush(() => {
      handleThreadMessage({ type: "thread_turn", turn });
      handleThreadMessage({ type: "turn_event", thread_id: 1, turn_id: 20, seq: 1, event: { kind: "reasoning", text: "Checking the page" } });
      handleThreadMessage({ type: "turn_event", thread_id: 1, turn_id: 20, seq: 2, event: { kind: "tool_done", id: "browser", name: "browser_check", ok: false, summary: "Page did not load", result: { text: "Page did not load", truncated: false } } });
    });

    flush(() => handleThreadMessage({ type: "thread_turn", turn: { ...turn, state, finished_at: "2026-09-09T10:00:02Z" } }));

    expect(threadState.turnDetails[20].map(row => row.event)).toEqual([
      { kind: "reasoning", text: "Checking the page" },
      { kind: "tool_done", id: "browser", name: "browser_check", ok: false, summary: "Page did not load", result: { text: "Page did not load", truncated: false } },
    ]);
  });
});

it("snapshots canonical artifact references through failure, retry and reconnect", () => {
  const references = [55,44,55];
  flush(() => sendThreadMessage("test-history", 1, "Make this simpler", "send", [], [], references));
  references.splice(0,references.length,99);
  const frame = sent.find(frame => frame.type === "send_thread_message");
  if (frame?.type !== "send_thread_message") throw new Error("Missing send");
  expect(frame.artifact_ids).toEqual([44,55]);
  flush(() => handleThreadMessage({type:"error",client_id:frame.client_id,detail:"Temporary failure"}));
  expect(threadState.pending[0].artifactIds).toEqual([44,55]);
  flush(() => retryThreadMessage(frame.client_id));
  expect(sent.at(-1)).toEqual(frame);
  disconnectThreads(); attachThreadTransport(outgoing => sent.push(outgoing));
  flush(() => handleThreadMessage({type:"hello_ok",threads:[makeThread(1)],history_id:"test-history",processes:[],views:[],host_version:"test",model:null,subagent_models:null,prompts:null,providers:null}));
  expect(sent.filter(row=>row.type==="send_thread_message").at(-1)).toEqual(frame);
});

it("clears a send failure only when that exact message is accepted", () => {
  flush(() => sendThreadMessage("test-history", 1, "first", "send", [], [], [44]));
  flush(() => sendThreadMessage("test-history", 1, "second", "send", [], [], [55]));
  const [first, second] = sent.filter(row => row.type === "send_thread_message").map(row => row.client_id);
  expect(first).not.toBe(second);
  expect(threadState.pending).toHaveLength(2);
  flush(() => handleThreadMessage({type:"error",client_id:first,detail:"First failed"}));
  flush(() => handleThreadMessage({type:"error",client_id:second,detail:"Second failed"}));
  expect(threadState.error).toMatchObject({detail:"Second failed",clientId:second});
  const accept = (client_id: string, id: number) => handleThreadMessage({type:"msg",message:{id,thread_id:1,client_id,author:"owner",body:"Accepted",ref:null,ts:"2026-09-10T10:00:00Z"}});
  flush(() => accept(first, 40));
  expect(threadState.error?.detail).toBe("Second failed");
  flush(() => retryThreadMessage(second));
  flush(() => accept(second, 41));
  expect(threadState.pending).toEqual([]);
  expect(threadState.error).toBeNull();
});

it("preserves other pending messages when one times out and retries", () => {
  vi.useFakeTimers();
  flush(() => sendThreadMessage("test-history", 1, "first", "send", [], [], [44]));
  const first = sent[0];
  if (first.type !== "send_thread_message") throw new Error("Missing send");
  flush(() => vi.advanceTimersByTime(10_000));
  flush(() => sendThreadMessage("test-history", 1, "second", "send", [], [], [55]));
  flush(() => vi.advanceTimersByTime(10_000));
  expect(threadState.pending.map(row => [row.body,row.failed,row.artifactIds])).toEqual([["first",true,[44]],["second",false,[55]]]);
  flush(() => retryThreadMessage(first.client_id));
  expect(sent.at(-1)).toEqual(first);
  expect(threadState.pending.map(row => [row.body,row.failed,row.artifactIds])).toEqual([["first",false,[44]],["second",false,[55]]]);
});
