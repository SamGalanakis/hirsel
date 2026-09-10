import { flush } from "solid-js";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { setHistoryId } from "../lib/history";
import type { ThreadClientMessage } from "./types";
import { makeThread } from "./fixtures";
import { attachThreadTransport, disconnectThreads, focusThread, handleThreadMessage, resetThreads, routeThreadId, setThreadState, threadState, followThreadLocation, sendThreadMessage } from "./store";
const frames: ThreadClientMessage[] = [];
const hello = (ids: number[]) => flush(() => handleThreadMessage({ type: "hello_ok", history_id: "ab123456-1234-5678-9abc-123456789abc", threads: ids.map(id => makeThread(id)), processes: [], views: [], host_version: "test", model: null, subagent_models: null, prompts: null, providers: null }));
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
  it("opens no recipient from an empty or populated forest without a saved selection", () => {
    hello([]); expect(threadState.focusedId).toBeNull(); expect(frames).toEqual([]);
    hello([0,1,2]); expect(threadState.focusedId).toBeNull(); expect(frames).toEqual([]);
  });
  it("prioritizes an explicit ordinary zero route over saved and focused IDs", () => {
    localStorage.setItem("hirsel.last-thread.ab123456-1234-5678-9abc-123456789abc", "2");
    history.replaceState(null, "", "/t/0?history=ab123456-1234-5678-9abc-123456789abc");
    hello([0,1,2]);
    expect(threadState.focusedId).toBe(0);
    expect(frames).toContainEqual(expect.objectContaining({ type: "open_thread", thread_id: 0 }));
    expect(routeThreadId("/t/9007199254740993")).toBeNull();
  });
  it("restores only a valid selection from this history", () => {
    localStorage.setItem("hirsel.last-thread.another-history", "1");
    localStorage.setItem("hirsel.last-thread.ab123456-1234-5678-9abc-123456789abc", "99");
    hello([1,2]); expect(threadState.focusedId).toBeNull();
    localStorage.setItem("hirsel.last-thread.ab123456-1234-5678-9abc-123456789abc", "2");
    hello([1,2]); expect(threadState.focusedId).toBe(2); expect(location.pathname).toBe("/t/2");
  });
  it("never redirects a missing explicit destination to saved or pinned work", () => {
    localStorage.setItem("hirsel.last-thread.ab123456-1234-5678-9abc-123456789abc", "1");
    history.replaceState(null, "", "/t/99?history=ab123456-1234-5678-9abc-123456789abc");
    hello([1,2]); expect(threadState.focusedId).toBeNull(); expect(threadState.linkError).toContain("unavailable"); expect(location.pathname).toBe("/t/99");
  });
  it("keeps an explicitly unselected overview unaddressed after reconnect", () => {
    hello([1]); flush(() => focusThread(1)); flush(() => focusThread(null));
    frames.length = 0;
    hello([1]); expect(threadState.focusedId).toBeNull(); expect(frames).toEqual([]);
    flush(() => setThreadState(draft => { draft.focusedId = 1; }));
    flush(() => resetThreads()); expect(threadState.focusedId).toBeNull();
  });
  it("preserves route intent across reset and refuses reused IDs in a new history", () => {
    history.replaceState(null,"", "/t/1?history=ab123456-1234-5678-9abc-123456789abd");
    hello([1]); expect(threadState.focusedId).toBeNull(); expect(threadState.linkError).toContain("another Hirsel history"); expect(frames).toEqual([]);
    flush(resetThreads); expect(location.search).toContain("123456789abd");
    hello([1]); expect(threadState.focusedId).toBeNull(); expect(frames).toEqual([]);
  });
  it("leaves unqualified routes unaddressed even with a saved Thread",()=>{
    localStorage.setItem("hirsel.last-thread.ab123456-1234-5678-9abc-123456789abc","1");
    history.replaceState(null,"","/t/1"); hello([1]);
    expect(threadState.focusedId).toBeNull(); expect(threadState.linkError).toContain("incomplete"); expect(frames).toEqual([]);
  });
  it("waits for a fresh hello before navigating cached IDs or sending",()=>{
    hello([1,2]); flush(()=>focusThread(1)); flush(disconnectThreads); frames.length=0;
    attachThreadTransport(frame=>frames.push(frame)); history.replaceState(null,"","/t/2?history=ab123456-1234-5678-9abc-123456789abc");
    flush(followThreadLocation); flush(()=>focusThread(2));
    expect(threadState.focusedId).toBeNull(); expect(frames).toEqual([]);
    expect(()=>sendThreadMessage(2,"not yet","send",[],[],[])).toThrow("Reconnect");
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

});
