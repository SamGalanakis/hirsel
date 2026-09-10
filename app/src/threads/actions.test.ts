import { afterEach, describe, expect, it, vi } from "vitest";
import { flush } from "solid-js";
import { setHistoryId } from "../lib/history";
import { toast } from "../lib/toast";
import { threadActions } from "./actions";
import { makeThread } from "./fixtures";
vi.mock("../lib/toast",()=>({toast:vi.fn()}));
vi.mock("../ws/client",()=>({getClient:()=>null}));
afterEach(()=>vi.restoreAllMocks());
describe("portable Thread action copying",()=>{
 it("captures history and uses a neutral ordinary Markdown label",async()=>{
  const writeText=vi.fn().mockResolvedValue(undefined);Object.defineProperty(navigator,"clipboard",{configurable:true,value:{writeText}});
  flush(()=>setHistoryId("ab123456-1234-5678-9abc-123456789abc"));const actions=threadActions(makeThread(2,{title:"Private title"}));
  flush(()=>setHistoryId("ab123456-1234-5678-9abc-123456789abd"));actions.find(action=>action.id==='copy-reference')!.run();await Promise.resolve();
  expect(writeText).toHaveBeenCalledWith(`[Thread #2](${location.origin}/t/2?history=ab123456-1234-5678-9abc-123456789abc)`);
 });
 it("reports an unavailable clipboard without an unhandled rejection",async()=>{
  Object.defineProperty(navigator,"clipboard",{configurable:true,value:undefined});
  threadActions(makeThread(2)).find(action=>action.id==='copy-reference')!.run();await Promise.resolve();
  expect(toast).toHaveBeenCalledWith("Couldn’t copy the reference.",{variant:"error"});
 });
});
