import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { flush } from "solid-js";
import { setHistoryId } from "../lib/history";
import { makeThread } from "../threads/fixtures";
import { handleThreadMessage, setThreadState } from "../threads/store";
import type { ThreadClientMessage, ThreadRelatedItem } from "../threads/types";
import { addRelatedItem, attachRelatedTransport, disconnectRelated, handleRelatedMessage, hasRelatedTarget, loadRelated, relatedState, removeRelatedItem, resetRelated, trackRelatedRead } from "./store";
const origin = { historyId: "history-a", threadId: 1 };
const link: ThreadRelatedItem = { id: 7, thread_id: 1, target: {kind:"url",url: "https://github.com/a/b/pull/3"}, title: null, created_at: "2026-09-10T10:00:00Z" };
const frames: ThreadClientMessage[] = [];
const receive = (revision: number, items: ThreadRelatedItem[], clientId: string | null = null, history = origin.historyId) => flush(() => handleRelatedMessage({ type: "thread_related_changed", history_id: history, thread_id: 1, revision, items, client_id: clientId }));
beforeEach(() => { frames.length = 0; flush(() => { setHistoryId(origin.historyId); resetRelated(); setThreadState(draft => { draft.threads = [makeThread(1),makeThread(2)]; draft.focusedId = 1; }); }); attachRelatedTransport(frame => frames.push(frame)); });
afterEach(disconnectRelated);
describe("Related request identity and snapshots", () => {
  it("keeps the captured Thread when selection changes and waits for authoritative snapshots", async () => {
    const saved = addRelatedItem(origin, {kind:"url",url:"https://github.com/a/b/pull/3"}, "Review");
    flush(() => setThreadState(draft => { draft.focusedId = 2; }));
    expect(frames[0]).toMatchObject({type:"add_thread_related",history_id:"history-a",thread_id:1,title:"Review"});
    expect(relatedState.lists[1]).toBeUndefined();
    receive(2,[link],(frames[0] as {client_id:string}).client_id);await saved;expect(hasRelatedTarget(origin,link.target)).toBe(true);
    const removed=removeRelatedItem(origin,7);receive(3,[],(frames[1] as {client_id:string}).client_id);await removed;expect(relatedState.lists[1].items).toEqual([]);
  });
  it("stores a typed Thread target and recognizes duplicate identity without copying its title", async () => {
    const target = { kind: "thread" as const, history_id: origin.historyId, thread_id: 2 };
    const saved = addRelatedItem(origin,target,null);
    expect(frames[0]).toMatchObject({type:"add_thread_related",thread_id:1,target,title:null});
    receive(2,[{id:8,thread_id:1,target,title:null,created_at:link.created_at}],(frames[0] as {client_id:string}).client_id);await saved;
    expect(hasRelatedTarget(origin,target)).toBe(true);
    expect(hasRelatedTarget(origin,{...target,history_id:"other-history"})).toBe(false);
  });
  it("uses link snapshot revision independently of newer Thread metadata", () => {
    receive(1,[]);
    flush(() => handleThreadMessage({type:"thread_upsert",thread:makeThread(1,{revision:3})}));
    receive(2,[link]);expect(relatedState.lists[1].items).toEqual([link]);
    receive(1,[]);expect(relatedState.lists[1].items).toEqual([link]);
    receive(2,[link]);expect(relatedState.lists[1].revision).toBe(2);
    trackRelatedRead({type:"open_thread",client_id:"read",thread_id:1,before_id:null});
    flush(() => handleRelatedMessage({type:"thread_opened",client_id:"read",detail:{thread:makeThread(1,{revision:4}),related_items:[],brief:{text:"",artifact_ids:[]},messages:[],turns:[],activities:[],has_more:false}}));
    expect(relatedState.lists[1].items).toEqual([]);expect(relatedState.lists[1].revision).toBe(4);
  });
  it("rejects old-history actions, broadcasts and delayed detail responses after reused IDs", async () => {
    trackRelatedRead({type:"open_thread",client_id:"old-read",thread_id:1,before_id:null});
    const saved=addRelatedItem(origin,link.target,null);const rejected=expect(saved).rejects.toThrow("History changed");
    flush(()=>{setHistoryId("history-b");resetRelated();});await rejected;
    await expect(addRelatedItem(origin,link.target,null)).rejects.toThrow("History changed");
    receive(30,[link]);
    flush(()=>handleRelatedMessage({type:"thread_opened",client_id:"old-read",detail:{thread:makeThread(1,{revision:30}),related_items:[link],brief:{text:"",artifact_ids:[]},messages:[],turns:[],activities:[],has_more:false}}));
    expect(relatedState.lists[1]).toBeUndefined();expect(frames).toHaveLength(1);
  });
  it("correlates errors and retries a load without losing existing links", async () => {
    receive(2,[link]);const loading=loadRelated(origin);flush();
    flush(()=>handleRelatedMessage({type:"error",client_id:(frames[0] as {client_id:string}).client_id,detail:"Temporary read failure"}));await loading;flush();
    expect(relatedState.lists[1].items).toEqual([link]);expect(relatedState.lists[1].error).toContain("Temporary");
    const retry=loadRelated(origin);const id=(frames[1] as {client_id:string}).client_id;
    flush(()=>handleRelatedMessage({type:"thread_opened",client_id:id,detail:{thread:makeThread(1,{revision:3}),related_items:[link],brief:{text:"",artifact_ids:[]},messages:[],turns:[],activities:[],has_more:false}}));await retry;flush();expect(relatedState.lists[1].error).toBeNull();
  });
  it("captures the authoritative hello history for a read before the history signal commits",()=>{
    flush(()=>setHistoryId(null));setHistoryId(origin.historyId);
    trackRelatedRead({type:"open_thread",client_id:"first-read",thread_id:1,before_id:null},origin.historyId);
    flush();
    flush(()=>handleRelatedMessage({type:"thread_opened",client_id:"first-read",detail:{thread:makeThread(1,{revision:2}),related_items:[link],brief:{text:"",artifact_ids:[]},messages:[],turns:[],activities:[],has_more:false}}));
    expect(relatedState.lists[1].items).toEqual([link]);
  });

});
