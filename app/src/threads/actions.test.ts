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
describe("Space and Task actions", () => {
  it("offers revision-bound conversion and completion only where valid", () => {
    const space = threadActions(makeThread(2, { kind: "space", revision: 7 }));
    expect(space.map(action => action.label)).toContain("Change to Task");
    expect(space.map(action => action.label)).not.toContain("Mark task done");

    const task = threadActions(makeThread(2, { kind: "task", revision: 8 }));
    expect(task.map(action => action.label)).toEqual(expect.arrayContaining(["New child task", "Mark task done", "Change to Space"]));

    const done = threadActions(makeThread(2, { kind: "task", settled_at: "2026-09-10T10:00:00Z" }));
    expect(done.map(action => action.label)).toContain("Reopen task");
    expect(done.map(action => action.label)).not.toContain("Change to Space");
  });
});
