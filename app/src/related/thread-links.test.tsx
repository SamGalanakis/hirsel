import { cleanup, fireEvent, render, screen } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { flush } from "solid-js";
import { Markdown } from "../components/Markdown";
import { setHistoryId } from "../lib/history";
import { threadUrl } from "../lib/thread-url";
import { makeThread } from "../threads/fixtures";
import { disconnectThreads, setThreadState, threadState } from "../threads/store";
import { RelatedContext } from "./context";
import { attachRelatedTransport, disconnectRelated, handleRelatedMessage, resetRelated } from "./store";
import type { ThreadClientMessage } from "../threads/types";
const historyId="ab123456-1234-5678-9abc-123456789abc";
const target={kind:"thread" as const,history_id:historyId,thread_id:2};
const frames:ThreadClientMessage[]=[];const clipboard=vi.fn();
beforeEach(()=>{frames.length=0;const storage=new Map<string,string>();vi.stubGlobal("localStorage",{getItem:(key:string)=>storage.get(key)??null,setItem:(key:string,value:string)=>storage.set(key,value),removeItem:(key:string)=>storage.delete(key)});clipboard.mockReset().mockResolvedValue(undefined);Object.defineProperty(navigator,"clipboard",{configurable:true,value:{writeText:clipboard}});flush(()=>{setHistoryId(historyId);resetRelated();setThreadState(draft=>{draft.ready=true;draft.threads=[makeThread(1),makeThread(2,{title:"Current project"})];draft.focusedId=1;});});attachRelatedTransport(frame=>frames.push(frame));});
afterEach(()=>{cleanup();disconnectRelated();disconnectThreads();vi.restoreAllMocks();vi.unstubAllGlobals();});
const mount=()=>render(()=><RelatedContext value={{historyId,threadId:1}}><Markdown>{threadUrl(target)}</Markdown></RelatedContext>);
describe("Thread rich links",()=>{
 it("hydrates the current title, keeps modifier navigation native, and switches only on plain click",()=>{
  mount();const link=screen.getByRole('link',{name:'Thread #2 · Current project'});
  const modified=new MouseEvent('click',{bubbles:true,cancelable:true,ctrlKey:true});link.dispatchEvent(modified);expect(modified.defaultPrevented).toBe(false);expect(threadState.focusedId).toBe(1);
  fireEvent.click(link);expect(threadState.focusedId).toBe(2);expect(location.search).toBe(`?history=${historyId}`);
 });
 it("copies an ordinary Markdown reference and saves an explicit typed association to its origin",async()=>{
  mount();fireEvent.click(screen.getByRole('button',{name:/Link actions:/}));fireEvent.click(screen.getByRole('menuitem',{name:'Copy reference'}));expect(clipboard).toHaveBeenCalledWith(`[Thread #2](${threadUrl(target)})`);
  fireEvent.click(screen.getByRole('button',{name:/Link actions:/}));fireEvent.click(screen.getByRole('menuitem',{name:'Add to Related'}));
  expect(frames[0]).toMatchObject({type:'add_thread_related',history_id:historyId,thread_id:1,target,title:null});
  flush(()=>handleRelatedMessage({type:'thread_related_changed',client_id:(frames[0] as {client_id:string}).client_id,history_id:historyId,thread_id:1,revision:2,items:[{id:1,thread_id:1,target,title:null,created_at:'now'}]}));
 });
 it("never hydrates a cached title before hello or after a history replacement",()=>{
  mount();flush(disconnectThreads);expect(screen.queryByRole('link',{name:/Current project/})).toBeNull();
  const link=screen.getByRole('link',{name:'Thread #2 · unavailable'});const click=new MouseEvent('click',{bubbles:true,cancelable:true});link.dispatchEvent(click);expect(click.defaultPrevented).toBe(false);
  flush(()=>{setHistoryId('ab123456-1234-5678-9abc-123456789abd');setThreadState(draft=>{draft.ready=true;draft.threads=[makeThread(2,{title:'Unrelated private title'})];});});
  expect(screen.queryByText(/Unrelated private title/)).toBeNull();fireEvent.click(screen.getByRole('button',{name:/Link actions:/}));expect(screen.getByRole('menuitem',{name:'Thread unavailable'})).toBeDisabled();expect(frames).toEqual([]);
 });
});
