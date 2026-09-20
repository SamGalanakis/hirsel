import { flush } from "solid-js";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { setHistoryId } from "../lib/history";
import type { ThreadClientMessage } from "./types";
import { makeThread } from "./fixtures";
import { spaceState } from "../spaces/store";
import { attachThreadTransport, disconnectThreads, focusThread, handleThreadMessage, resetThreads, routeThreadId, setThreadState, threadState, followThreadLocation, sendThreadMessage } from "./store";
const frames: ThreadClientMessage[] = [];
const helloThreads = (threads: ReturnType<typeof makeThread>[]) => flush(() => handleThreadMessage({ type: "hello_ok", history_id: "ab123456-1234-5678-9abc-123456789abc", threads, processes: [], views: [], host_version: "test", model: null, subagent_models: null, prompts: null, providers: null }));
const hello = (ids: number[]) => helloThreads(ids.map(id => makeThread(id)));
beforeEach(() => {
  frames.length = 0;
  const storage = new Map<string, string>();
  vi.stubGlobal("localStorage", { getItem: (key: string) => storage.get(key) ?? null, setItem: (key: string, value: string) => storage.set(key, value), removeItem: (key: string) => storage.delete(key) });
  history.replaceState(null, "", "/");
  flush(() => { setHistoryId("ab123456-1234-5678-9abc-123456789abc"); resetThreads(); });
  attachThreadTransport(frame => frames.push(frame));
});
afterEach(() => { disconnectThreads(); vi.unstubAllGlobals(); });
describe("explicit Thread selection", () => {
  it("creates Home only for an empty forest and otherwise opens the first Space", () => {
    hello([]); expect(threadState.focusedId).toBeNull(); expect(frames).toContainEqual(expect.objectContaining({type:"create_thread", title:"Home", kind:"space", parent_thread_id:null}));
    frames.length = 0;
    hello([0,1,2]); expect(threadState.focusedId).toBe(0); expect(frames).toContainEqual(expect.objectContaining({type:"open_thread", thread_id:0}));
  });
  it("keeps a newer selection when delayed Home bootstrap completes", async () => {
    helloThreads([]);
    const request = frames.find(frame => frame.type === "create_thread");
    if (request?.type !== "create_thread") throw new Error("missing Home create");
    const chosen = makeThread(2, { title: "Chosen Space", kind: "space", parent_thread_id: null });
    flush(() => handleThreadMessage({ type: "thread_upsert", thread: chosen }));
    flush(() => focusThread(2));
    const home = makeThread(3, { title: "Home", kind: "space", parent_thread_id: null });
    flush(() => handleThreadMessage({ type: "thread_created", client_id: request.client_id, thread: home }));
    await Promise.resolve();
    await Promise.resolve();
    expect(threadState.threads).toContainEqual(home);
    expect(threadState.focusedId).toBe(2);
    expect(location.pathname).toBe("/t/2");
  });
  it("addresses a nested Space as a Space chat instead of pairing it as a worker", () => {
    const home = makeThread(1, { title: "Home", kind: "space", parent_thread_id: null });
    const nested = makeThread(2, { title: "Planning", kind: "space", parent_thread_id: home.id });
    helloThreads([home, nested]);

    flush(() => focusThread(nested.id));

    expect(threadState.focusedId).toBe(nested.id);
    expect(spaceState).toMatchObject({ spaceRecipientId: nested.id, workerPairingId: null });
    expect(localStorage.getItem("hirsel.last-space.ab123456-1234-5678-9abc-123456789abc")).toBe(String(home.id));
  });
  it("pairs a Task with its nearest containing Space chat", () => {
    const home = makeThread(1, { title: "Home", kind: "space", parent_thread_id: null });
    const nested = makeThread(2, { title: "Planning", kind: "space", parent_thread_id: home.id });
    const task = makeThread(3, { title: "Draft", kind: "task", parent_thread_id: nested.id });
    helloThreads([home, nested, task]);

    flush(() => focusThread(task.id));

    expect(threadState.focusedId).toBe(task.id);
    expect(spaceState).toMatchObject({ spaceRecipientId: nested.id, workerPairingId: task.id });
    expect(localStorage.getItem("hirsel.last-space.ab123456-1234-5678-9abc-123456789abc")).toBe(String(home.id));
  });
  it("prioritizes an explicit ordinary zero route over saved and focused IDs", () => {
    localStorage.setItem("hirsel.last-space.ab123456-1234-5678-9abc-123456789abc", "2");
    history.replaceState(null, "", "/t/0?history=ab123456-1234-5678-9abc-123456789abc");
    hello([0,1,2]);
    expect(threadState.focusedId).toBe(0);
    expect(frames).toContainEqual(expect.objectContaining({ type: "open_thread", thread_id: 0 }));
    expect(routeThreadId("/t/9007199254740993")).toBeNull();
  });
  it("restores only a valid selection from this history", () => {
    localStorage.setItem("hirsel.last-space.another-history", "1");
    localStorage.setItem("hirsel.last-space.ab123456-1234-5678-9abc-123456789abc", "99");
    hello([1,2]); expect(threadState.focusedId).toBe(1); expect(frames).toContainEqual(expect.objectContaining({type:"open_thread",thread_id:1}));
    history.replaceState(null, "", "/");
    localStorage.setItem("hirsel.last-space.ab123456-1234-5678-9abc-123456789abc", "2");
    hello([1,2]); expect(threadState.focusedId).toBe(2); expect(location.pathname).toBe("/t/2");
  });
  it("never redirects a missing explicit destination to saved or pinned work", () => {
    localStorage.setItem("hirsel.last-space.ab123456-1234-5678-9abc-123456789abc", "1");
    history.replaceState(null, "", "/t/99?history=ab123456-1234-5678-9abc-123456789abc");
    hello([1,2]); expect(threadState.focusedId).toBeNull(); expect(threadState.linkError).toContain("unavailable"); expect(location.pathname).toBe("/t/99");
  });
  it("restores the last Space after returning to the route-free entry", () => {
    hello([1]); flush(() => focusThread(1)); flush(() => focusThread(null));
    frames.length = 0;
    hello([1]); expect(threadState.focusedId).toBe(1); expect(frames).toContainEqual(expect.objectContaining({type:"open_thread",thread_id:1}));
    flush(() => setThreadState(draft => { draft.focusedId = 1; }));
    flush(() => resetThreads()); expect(threadState.focusedId).toBeNull();
  });
  it("preserves route intent across reset and refuses reused IDs in a new history", () => {
    history.replaceState(null,"", "/t/1?history=ab123456-1234-5678-9abc-123456789abd");
    hello([1]); expect(threadState.focusedId).toBeNull(); expect(threadState.linkError).toContain("another Hirsel history"); expect(frames).toEqual([]);
    flush(resetThreads); expect(location.search).toContain("123456789abd");
    hello([1]); expect(threadState.focusedId).toBeNull(); expect(frames).toEqual([]);
  });
  it("resolves an unqualified route against the connected history and still refuses unknown IDs",()=>{
    localStorage.setItem("hirsel.last-space.ab123456-1234-5678-9abc-123456789abc","1");
    history.replaceState(null,"","/t/1"); hello([1]);
    expect(threadState.focusedId).toBe(1); expect(threadState.linkError).toBeNull();
    expect(location.search).toContain("ab123456-1234-5678-9abc-123456789abc");
  });
  it("refuses an unqualified route to a Thread this history does not have",()=>{
    history.replaceState(null,"","/t/9"); hello([1]);
    expect(threadState.focusedId).toBeNull(); expect(threadState.linkError).toContain("#9 is unavailable"); expect(frames).toEqual([]);
  });
  it("waits for a fresh hello before navigating cached IDs or sending",()=>{
    hello([1,2]); flush(()=>focusThread(1)); flush(disconnectThreads); frames.length=0;
    attachThreadTransport(frame=>frames.push(frame)); history.replaceState(null,"","/t/2?history=ab123456-1234-5678-9abc-123456789abc");
    flush(followThreadLocation); flush(()=>focusThread(2));
    expect(threadState.focusedId).toBeNull(); expect(frames).toEqual([]);
    expect(()=>sendThreadMessage("test-history",2,"not yet","send",[],[],[])).toThrow("Reconnect");
    hello([1,2]); expect(threadState.focusedId).toBe(2); expect(frames).toContainEqual(expect.objectContaining({type:"open_thread",thread_id:2}));
  });
  it("validates the first hello against its payload while the history signal is still pending",()=>{
    flush(()=>setHistoryId(null));
    history.replaceState(null,"","/t/2?history=ab123456-1234-5678-9abc-123456789abc");
    // The real WS path accepts history and projects hello in one synchronous callback.
    setHistoryId("ab123456-1234-5678-9abc-123456789abc");
    handleThreadMessage({type:"hello_ok",history_id:"ab123456-1234-5678-9abc-123456789abc",threads:[makeThread(2)],processes:[],views:[],host_version:"test",model:null,subagent_models:null,prompts:null,providers:null});
    flush();
    expect(threadState.linkError).toBeNull();expect(threadState.focusedId).toBe(2);
    expect(frames).toContainEqual(expect.objectContaining({type:"open_thread",thread_id:2}));
  });

  it("does not restore an archived selection from an authoritative snapshot", () => {
    localStorage.setItem("hirsel.last-space.ab123456-1234-5678-9abc-123456789abc", "2");
    helloThreads([makeThread(1), makeThread(2, { archived_at: "2026-09-10T10:00:00Z" })]);
    expect(threadState.focusedId).toBe(1);
    expect(localStorage.getItem("hirsel.last-space.ab123456-1234-5678-9abc-123456789abc")).toBe("1");
    expect(location.pathname).toBe("/t/1");
  });

  it("clears a previously focused Thread when a route-free snapshot archives it", () => {
    hello([2]);
    flush(() => focusThread(2));
    history.replaceState(null, "", "/");
    helloThreads([makeThread(2, { archived_at: "2026-09-10T10:00:00Z", revision: 2 })]);
    expect(threadState.focusedId).toBeNull();
    expect(localStorage.getItem("hirsel.last-space.ab123456-1234-5678-9abc-123456789abc")).toBeNull();
    expect(frames).toContainEqual(expect.objectContaining({ type: "create_thread", title: "Home" }));
  });

  it("keeps an explicit archived Thread route selectable", () => {
    history.replaceState(null, "", "/t/2?history=ab123456-1234-5678-9abc-123456789abc");
    helloThreads([makeThread(2, { archived_at: "2026-09-10T10:00:00Z" })]);
    expect(threadState.focusedId).toBe(2);
    expect(location.pathname).toBe("/t/2");
    expect(frames).toContainEqual(expect.objectContaining({ type: "open_thread", thread_id: 2 }));
  });

});
