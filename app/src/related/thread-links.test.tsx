import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
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
vi.mock("../ws/client",async importOriginal=>({...await importOriginal<typeof import("../ws/client")>(),getClient:()=>({getBlobUrl:async(id:string)=>`https://example.test/blob/${id}`})}));
const historyId="ab123456-1234-5678-9abc-123456789abc";
const target={kind:"thread" as const,history_id:historyId,thread_id:2};
const frames:ThreadClientMessage[]=[];const clipboard=vi.fn();
beforeEach(()=>{frames.length=0;const storage=new Map<string,string>();vi.stubGlobal("localStorage",{getItem:(key:string)=>storage.get(key)??null,setItem:(key:string,value:string)=>storage.set(key,value),removeItem:(key:string)=>storage.delete(key)});clipboard.mockReset().mockResolvedValue(undefined);Object.defineProperty(navigator,"clipboard",{configurable:true,value:{writeText:clipboard}});flush(()=>{setHistoryId(historyId);resetRelated();setThreadState(draft=>{draft.ready=true;draft.threads=[makeThread(1),makeThread(2,{title:"Current project"})];draft.focusedId=1;});});attachRelatedTransport(frame=>frames.push(frame));});
afterEach(()=>{cleanup();disconnectRelated();disconnectThreads();vi.restoreAllMocks();vi.unstubAllGlobals();});
const mount=()=>render(()=><RelatedContext value={{historyId,threadId:1}}><Markdown>{threadUrl(target)}</Markdown></RelatedContext>);
describe("Thread rich links",()=>{
 it("reads as one chip carrying the Thread's own name, with the id only in its label",()=>{
  const {container}=mount();const link=screen.getByRole('link',{name:'Thread #2 · Current project'});
  // One object: avatar + name on a quiet ground. No second element beside it,
  // no underline, and the id is never drawn into the sentence.
  expect(link.querySelector('[data-slot="thread-chip-label"]')?.textContent).toBe('Current project');expect(link).toHaveAttribute('title','Thread #2 · Current project');
  expect(link.className).toContain('bg-muted/40');expect(link.className).toContain('no-underline');expect(link.className).not.toContain('decoration');
  expect(container.querySelectorAll('button')).toHaveLength(0);expect(container.querySelectorAll('[data-link-kind] > *')).toHaveLength(1);
  const modified=new MouseEvent('click',{bubbles:true,cancelable:true,ctrlKey:true});link.dispatchEvent(modified);expect(modified.defaultPrevented).toBe(false);expect(threadState.focusedId).toBe(1);
 });
 it("renders a bare #id written in prose as that same one chip",()=>{
  const {container}=render(()=><RelatedContext value={{historyId,threadId:1}}><Markdown>{"Working in #2 now."}</Markdown></RelatedContext>);
  // The agent writes the id; the sentence reads as the name, exactly once.
  expect(container.querySelectorAll('a')).toHaveLength(1);expect(container.querySelector('[data-slot="thread-chip-label"]')?.textContent).toBe('Current project');
  // Only the avatar's own glyph sits between the prose and the name.
  expect(container.textContent?.replace('C','')).toBe('Working in Current project now.');
  expect(screen.getByRole('link',{name:'Thread #2 · Current project'})).toHaveTextContent('Current project');
 });
 it("opens its actions from the chip itself, Open first",async()=>{
  mount();fireEvent.click(screen.getByRole('link',{name:'Thread #2 · Current project'}));
  expect((await screen.findAllByRole('menuitem')).map(item=>item.textContent)).toEqual(['Open','Open in new tab','Copy link','Copy reference','Add to Related']);
  fireEvent.click(screen.getByRole('menuitem',{name:'Open'}));expect(threadState.focusedId).toBe(2);expect(location.search).toBe(`?history=${historyId}`);
 });
 it("copies an ordinary Markdown reference and saves an explicit typed association to its origin",async()=>{
  mount();fireEvent.click(screen.getByRole('link',{name:/Thread #2/}));fireEvent.click(screen.getByRole('menuitem',{name:'Copy reference'}));expect(clipboard).toHaveBeenCalledWith(`[Thread #2](${threadUrl(target)})`);
  fireEvent.click(screen.getByRole('link',{name:/Thread #2/}));fireEvent.click(screen.getByRole('menuitem',{name:'Add to Related'}));
  expect(frames[0]).toMatchObject({type:'add_thread_related',history_id:historyId,thread_id:1,target,title:null});
  flush(()=>handleRelatedMessage({type:'thread_related_changed',client_id:(frames[0] as {client_id:string}).client_id,history_id:historyId,thread_id:1,revision:2,items:[{id:1,thread_id:1,target,title:null,created_at:'now'}]}));
 });
 it("shows an image icon as a cover-cropped avatar in the inline chip",async()=>{
  flush(()=>setThreadState(draft=>{draft.threads=[makeThread(1),makeThread(2,{title:"Current project",icon:{kind:"image",blob_id:"project-image"}})];}));
  mount();const avatar=screen.getByRole('link',{name:/Current project/}).querySelector('[data-thread-avatar="2"]')!;
  // The inline mark rides the sentence: an em-relative box, not the 16px list avatar.
  expect(avatar.className).toContain('size-[1.15em]');expect(avatar.className).toContain('overflow-hidden');
  const image=await waitFor(()=>{const node=avatar.querySelector('img');expect(node).not.toBeNull();return node!;});
  expect(image).toHaveAttribute('src','https://example.test/blob/project-image');expect(image.className).toContain('object-cover');expect(image).toHaveAttribute('alt','');
 });
 it("shows an emoji icon in the inline chip and the initial without one",()=>{
  flush(()=>setThreadState(draft=>{draft.threads=[makeThread(1),makeThread(2,{title:"Current project",icon:{kind:"emoji",value:"\u{1F331}"}})];}));
  mount();expect(screen.getByRole('link',{name:/Current project/}).querySelector('[data-thread-avatar="2"]')).toHaveTextContent("\u{1F331}");
  cleanup();flush(()=>setThreadState(draft=>{draft.threads=[makeThread(1),makeThread(2,{title:"Current project"})];}));
  mount();expect(screen.getByRole('link',{name:/Current project/}).querySelector('[data-thread-avatar="2"]')).toHaveTextContent("C");
 });
 it("never hydrates a cached title before hello or after a history replacement",()=>{
  mount();flush(disconnectThreads);expect(screen.queryByRole('link',{name:/Current project/})).toBeNull();
  // An unknown Thread degrades to the id the author typed; "unavailable" is
  // the chip's label, never words in the middle of the sentence.
  const link=screen.getByRole('link',{name:'Thread #2 · unavailable'});expect(link.textContent).toBe('#2');
  flush(()=>{setHistoryId('ab123456-1234-5678-9abc-123456789abd');setThreadState(draft=>{draft.ready=true;draft.threads=[makeThread(2,{title:'Unrelated private title'})];});});
  expect(screen.queryByText(/Unrelated private title/)).toBeNull();fireEvent.click(screen.getByRole('link',{name:/Thread #2/}));expect(screen.getByRole('menuitem',{name:'Thread unavailable'})).toBeDisabled();expect(frames).toEqual([]);
 });
});
