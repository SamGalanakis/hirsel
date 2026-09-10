import { flush } from "solid-js";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ChatMessage } from "../protocol";
import type { ThreadClientMessage, ThreadDetail } from "./types";
import { makeThread } from "./fixtures";
import { attachThreadTransport, createThread, disconnectThreads, handleThreadMessage, openThread, retryThreadMessage, sendThreadMessage, setThreadState, threadAction, threadState } from "./store";
const sent: ThreadClientMessage[] = [];
beforeEach(() => { const storage=new Map<string,string>();vi.stubGlobal("localStorage",{getItem:(key:string)=>storage.get(key)??null,setItem:(key:string,value:string)=>storage.set(key,value),removeItem:(key:string)=>storage.delete(key)}); sent.length = 0; flush(() => setThreadState(draft => { Object.assign(draft, { ready: true, linkError: null, threads: [], histories: {}, streams: {}, streamTurnIds: {}, turnDetails: {}, removedMessageIds: {}, pending: [], focusedId: 0, error: null }); })); attachThreadTransport(frame => sent.push(frame)); });
afterEach(() => { disconnectThreads(); vi.useRealTimers();vi.unstubAllGlobals(); });
const detail = (id: number): ThreadDetail => ({ brief: { text: "", artifact_ids: [] }, thread: makeThread(id), messages: [], turns: [], activities: [], related_items: [], has_more: false });
describe("thread transport projection", () => {
  it("creates a visible thread before any message and accepts its duplicate broadcast once", async () => {
    const promise = createThread("Buy groceries", null);
    const frame = sent[0];
    if (frame.type !== "create_thread") throw new Error("wrong command");
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
    flush(() => sendThreadMessage(1, "same", "send", [], [2], []));
    flush(() => sendThreadMessage(2, "same", "send", [], [1], []));
    const second = threadState.pending[1].clientId;
    flush(() => handleThreadMessage({ type: "msg", message: { id: 5, thread_id: 2, client_id: second, author: "owner", body: "same", ref: null, ts: "2026-09-09T10:00:00Z", mentions: [1] } }));
    expect(threadState.pending.map(p => p.threadId)).toEqual([1]);
    expect(threadState.histories[1]).toBeUndefined();
    expect(threadState.histories[2].messages).toHaveLength(1);
  });
  it("fails a missing acknowledgement and retries the same identity without duplicating content", () => {
    vi.useFakeTimers();
    flush(() => sendThreadMessage(1, "Buy groceries", "send", [], [2], []));
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
    expect(threadState.streams[1].map(row => row.event)).toEqual([{ kind: "prose", text: "new" }, { kind: "prose", text: " next" }]);
    expect(threadState.streamTurnIds[1]).toBe(11);
  });
  it("routes live timeline events only to their owning thread and binds instrument revision", () => {
    flush(() => handleThreadMessage({ type: "turn_event", thread_id: 2, turn_id: 1, seq: 1, event: { kind: "prose", text: "working" } }));
    expect(threadState.streams[1]).toBeUndefined();
    expect(threadState.streams[2]).toHaveLength(1);
    flush(() => threadAction(2, "choose", { choice: "a" }, 7));
    expect(sent[0]).toEqual({ type: "thread_action", thread_id: 2, action: "choose", data: { choice: "a" }, expected_revision: 7 });
  });
  it("resets sequence on a new turn and rejects late events or completion from the previous turn", () => {
    flush(() => handleThreadMessage({ type: "turn_event", thread_id: 1, turn_id: 10, seq: 1, event: { kind: "prose", text: "old" } }));
    flush(() => handleThreadMessage({ type: "turn_event", thread_id: 1, turn_id: 11, seq: 1, event: { kind: "prose", text: "new" } }));
    flush(() => handleThreadMessage({ type: "turn_event", thread_id: 1, turn_id: 10, seq: 2, event: { kind: "prose", text: "late" } }));
    flush(() => handleThreadMessage({ type: "thread_turn", turn: { requester_thread_id: null, requester_turn_id: null, id: 10, thread_id: 1, owner_message_id: 1, agent_message_id: 2, state: "completed", started_at: "2026-09-09T10:00:00Z", finished_at: "2026-09-09T10:00:01Z" } }));
    expect(threadState.streams[1].map(row => row.event)).toEqual([{ kind: "prose", text: "new" }]);
    expect(threadState.streamTurnIds[1]).toBe(11);
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
    flush(() => sendThreadMessage(1, "Cancelled before echo", "next_turn", [], [], []));
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
    flush(() => threadAction(1, "settle"));
    expect(threadState.error?.detail).toMatch(/Reconnect/);
  });
});

describe("running turn with a newer queued request", () => {
  it("admits the running turn, ignores queued cancellation, then moves to the next actual run", () => {
    const turn = (id: number, state: "running" | "queued" | "completed" | "cancelled", agent_message_id: number | null = null) => ({ requester_thread_id: null, requester_turn_id: null, id, thread_id: 1, owner_message_id: id, agent_message_id, state, started_at: "2026-09-09T10:00:00Z", finished_at: state === "completed" || state === "cancelled" ? "2026-09-09T10:00:01Z" : null });
    const stream = (turn_id:number,seq:number,text:string) => handleThreadMessage({type:"turn_event",thread_id:1,turn_id,seq,event:{kind:"prose",text}});
    flush(() => { handleThreadMessage({type:"thread_turn",turn:turn(10,"running")}); stream(10,1,"first"); handleThreadMessage({type:"thread_turn",turn:turn(11,"queued")}); stream(10,2,"second"); });
    expect(threadState.streams[1]).toHaveLength(2);
    flush(() => handleThreadMessage({type:"thread_turn",turn:turn(11,"cancelled")}));
    expect(threadState.streams[1]).toHaveLength(2);
    flush(() => handleThreadMessage({type:"thread_turn",turn:turn(10,"completed",20)}));
    expect(threadState.turnDetails[10]).toHaveLength(2);
    flush(() => { handleThreadMessage({type:"thread_turn",turn:turn(12,"running")}); stream(12,1,"new"); stream(10,3,"late"); });
    expect(threadState.streams[1].map(row=>row.event)).toEqual([{kind:"prose",text:"new"}]);
  });

  it.each(["failed", "cancelled", "interrupted"] as const)("retains the exact live timeline when a turn ends %s without a final message", state => {
    const turn = { requester_thread_id: null, requester_turn_id: null, id: 20, thread_id: 1, owner_message_id: 7, agent_message_id: null, state: "running" as const, started_at: "2026-09-09T10:00:00Z", finished_at: null };
    flush(() => {
      handleThreadMessage({ type: "thread_turn", turn });
      handleThreadMessage({ type: "turn_event", thread_id: 1, turn_id: 20, seq: 1, event: { kind: "reasoning", text: "Checking the page" } });
      handleThreadMessage({ type: "turn_event", thread_id: 1, turn_id: 20, seq: 2, event: { kind: "tool_done", id: "browser", name: "browser_check", ok: false, summary: "Page did not load" } });
    });

    flush(() => handleThreadMessage({ type: "thread_turn", turn: { ...turn, state, finished_at: "2026-09-09T10:00:02Z" } }));

    expect(threadState.turnDetails[20].map(row => row.event)).toEqual([
      { kind: "reasoning", text: "Checking the page" },
      { kind: "tool_done", id: "browser", name: "browser_check", ok: false, summary: "Page did not load" },
    ]);
    expect(threadState.streams[1]).toEqual([]);
    expect(threadState.streamTurnIds[1]).toBe(20);
  });
});

it("snapshots canonical artifact references through failure, retry and reconnect", () => {
  const references = [55,44,55];
  flush(() => sendThreadMessage(1, "Make this simpler", "send", [], [], references));
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
  flush(() => sendThreadMessage(1, "first", "send", [], [], [44]));
  flush(() => sendThreadMessage(1, "second", "send", [], [], [55]));
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
  flush(() => sendThreadMessage(1, "first", "send", [], [], [44]));
  const first = sent[0];
  if (first.type !== "send_thread_message") throw new Error("Missing send");
  flush(() => vi.advanceTimersByTime(10_000));
  flush(() => sendThreadMessage(1, "second", "send", [], [], [55]));
  flush(() => vi.advanceTimersByTime(10_000));
  expect(threadState.pending.map(row => [row.body,row.failed,row.artifactIds])).toEqual([["first",true,[44]],["second",false,[55]]]);
  flush(() => retryThreadMessage(first.client_id));
  expect(sent.at(-1)).toEqual(first);
  expect(threadState.pending.map(row => [row.body,row.failed,row.artifactIds])).toEqual([["first",false,[44]],["second",false,[55]]]);
});
