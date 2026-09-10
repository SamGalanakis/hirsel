import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { flush } from "solid-js";
import { Markdown } from "../components/Markdown";
import { setHistoryId } from "../lib/history";
import { RelatedContext } from "./context";
import { attachRelatedTransport, disconnectRelated, handleRelatedMessage, resetRelated } from "./store";
import type { ThreadClientMessage } from "../threads/types";
const url="https://github.com/owner/repo/pull/123?view=split#discussion";
const frames: ThreadClientMessage[]=[];
const clipboard=vi.fn();
beforeEach(()=>{frames.length=0;clipboard.mockReset().mockResolvedValue(undefined);Object.defineProperty(navigator,"clipboard",{configurable:true,value:{writeText:clipboard}});flush(()=>{setHistoryId("link-history");resetRelated();});attachRelatedTransport(frame=>frames.push(frame));});
afterEach(()=>{cleanup();disconnectRelated();vi.restoreAllMocks();});
const mount=(text:string)=>render(()=><RelatedContext value={{historyId:"link-history",threadId:1}}><Markdown>{text}</Markdown></RelatedContext>);
describe("rich Markdown links",()=>{
 it("uses one renderer for bare, authored and reference links, preserving labels and excluding code",()=>{
  const {container}=mount(`${url}\n\n[**Review** #123](${url})\n\n[Review definition][pr]\n\n[pr]: ${url}\n\n\`${url}\`\n\n\`\`\`txt\n${url}\n\`\`\``);
  expect(container.querySelectorAll('a')).toHaveLength(3);expect(screen.getByRole('link',{name:'Review #123'}).querySelector('strong')?.textContent).toBe('Review');expect(screen.getByRole('link',{name:'Review definition'}).getAttribute('href')).toBe(url);
  expect(container.querySelectorAll('code a, a button')).toHaveLength(0);expect(container.querySelectorAll('[data-thread-ref]')).toHaveLength(0);expect(frames).toEqual([]);
 });
 it("renders Linear ticket identity for bare links and preserves authored labels",()=>{
  const linear="https://linear.app/acme/issue/ENG-123/fix-the-parser?pane=activity#comment-42";
  const {container}=mount(`${linear}\n\n[Parser follow-up](${linear})`);const links=screen.getAllByRole('link');
  expect(links[0]).toHaveTextContent('ENG-123');expect(links[0].getAttribute('href')).toBe(linear);expect(links[0].closest('[data-link-kind]')?.getAttribute('data-link-kind')).toBe('issue');
  expect(links[1]).toHaveTextContent('Parser follow-up');expect(links[1].getAttribute('href')).toBe(linear);expect(container.querySelectorAll('a svg')).toHaveLength(2);
 });
 it("leaves unsafe URLs inert and gives lookalikes only a neutral identity",()=>{
  const {container}=mount(`[bad](javascript:alert%281%29) [fake](https://user:pass@github.com/a/b) [generic](https://github.com.evil.test/a/b/pull/1)`);
  expect(container.querySelectorAll('a')).toHaveLength(1);expect(container.querySelector('[data-link-kind]')?.getAttribute('data-link-kind')).toBe('web');
 });
 it("preserves linked images and resolves image definitions without adding an icon",()=>{
  const {container}=mount(`[![Diagram][picture]](${url})\n\n[picture]: https://example.com/image.png`);
  expect(container.querySelector('a img')?.getAttribute('alt')).toBe('Diagram');expect(container.querySelector('a svg')).toBeNull();
 });
 it("keeps modifier clicks, middle-click and native contextmenu unprevented",()=>{
  mount(`[Review](${url})`);const link=screen.getByRole('link',{name:'Review'});const observed:boolean[]=[];
  const observe=(event:Event)=>{observed.push(event.defaultPrevented);event.preventDefault();};document.addEventListener('click',observe);document.addEventListener('auxclick',observe);document.addEventListener('contextmenu',observe);
  for(const data of [{ctrlKey:true},{metaKey:true},{shiftKey:true},{altKey:true}])link.dispatchEvent(new MouseEvent('click',{bubbles:true,cancelable:true,...data}));
  link.dispatchEvent(new MouseEvent('auxclick',{bubbles:true,cancelable:true,button:1}));link.dispatchEvent(new MouseEvent('contextmenu',{bubbles:true,cancelable:true}));
  document.removeEventListener('click',observe);document.removeEventListener('auxclick',observe);document.removeEventListener('contextmenu',observe);expect(observed).toEqual([false,false,false,false,false,false]);expect(screen.queryByRole('menu')).toBeNull();
 });
 it("copies exact destinations and surfaces clipboard rejection without writes",async()=>{
  mount(`[Review](${url})`);fireEvent.click(screen.getByRole('button',{name:'Link actions: Review'}));fireEvent.click(await screen.findByRole('menuitem',{name:'Copy link'}));expect(clipboard).toHaveBeenCalledWith(url);
  clipboard.mockRejectedValueOnce(new Error('denied'));fireEvent.click(screen.getByRole('button',{name:'Link actions: Review'}));fireEvent.click(await screen.findByRole('menuitem',{name:'Copy link'}));expect(await screen.findByRole('alert')).toHaveTextContent('Couldn’t copy');expect(frames).toEqual([]);
 });
 it("saves only on explicit action and reflects a duplicate from the authoritative snapshot",async()=>{
  mount(`[Review](${url})`);expect(frames).toEqual([]);fireEvent.click(screen.getByRole('button',{name:'Link actions: Review'}));fireEvent.click(await screen.findByRole('menuitem',{name:'Add to Related'}));
  expect(frames[0]).toMatchObject({type:'add_thread_related',history_id:'link-history',thread_id:1,target:{kind:"url",url},title:'Review'});
  flush(()=>handleRelatedMessage({type:'thread_related_changed',client_id:(frames[0] as {client_id:string}).client_id,history_id:'link-history',thread_id:1,revision:2,items:[{id:1,thread_id:1,target:{kind:"url",url},title:'Review',created_at:'2026-09-10T10:00:00Z'}]}));
  await waitFor(()=>expect(screen.queryByRole('alert')).toBeNull());fireEvent.click(screen.getByRole('button',{name:'Link actions: Review'}));expect(await screen.findByRole('menuitem',{name:'Already in Related'})).toBeDisabled();expect(frames).toHaveLength(1);
 });
 it("rejects an action from a rendered old-history message",async()=>{
  mount(`[Review](${url})`);flush(()=>setHistoryId('replacement'));fireEvent.click(screen.getByRole('button',{name:'Link actions: Review'}));fireEvent.click(await screen.findByRole('menuitem',{name:'Add to Related'}));expect(await screen.findByRole('alert')).toHaveTextContent('History changed');expect(frames).toEqual([]);
 });
});
