import { flush } from "solid-js";
import { fireEvent, render, within } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ThreadShell } from "../threads/ThreadShell";
import { makeThread } from "../threads/fixtures";
import { emptyHistory } from "../threads/model";
import { attachThreadTransport, disconnectThreads, focusThread, handleThreadMessage, resetThreads, setThreadState, threadState } from "../threads/store";
import type { ThreadClientMessage } from "../threads/types";
import { setHistoryId } from "../lib/history";
import { dispatch } from "../store/store";
import { closeThreadNavigation } from "../threads/navigation";
import { artifactState, attachArtifactTransport, closeArtifact, handleArtifactMessage, resetArtifacts, setArtifactState } from "./store";
import { draftArtifact, resetDraftArtifacts } from "./draft-context";
import type { ArtifactClientMessage, ArtifactSummary } from "./types";
vi.mock("../ws/client", () => ({ getClient: () => ({ cancelTurn: vi.fn() }), makeClientId: () => crypto.randomUUID() }));
const frames: ThreadClientMessage[] = [], artifactFrames: ArtifactClientMessage[] = [];
const artifact = (id: number, title: string): ArtifactSummary => ({ id, title, thread_ids: [2], kind: "file", mime: "text/plain", filename: "result.txt", created_at: "2026-09-10T10:00:00Z", updated_at: "2026-09-10T10:00:00Z" });
beforeEach(() => {
  frames.length = 0; artifactFrames.length = 0;
  const storage = new Map<string,string>();
  vi.stubGlobal("localStorage", { getItem: (key: string) => storage.get(key) ?? null, setItem: (key: string, value: string) => storage.set(key,value), removeItem: (key: string) => storage.delete(key), key: (i:number) => [...storage.keys()][i] ?? null, get length() { return storage.size; } });
  flush(() => { resetArtifacts(); resetDraftArtifacts(); setHistoryId("artifact-context-history"); closeThreadNavigation(); dispatch({type:"connection_status",status:"connected"}); setThreadState(draft => { Object.assign(draft, { ready:true, threads:[makeThread(1,{title:"A",read:true}),makeThread(2,{title:"B",read:true})], histories:{1:{...emptyHistory(),loaded:true},2:{...emptyHistory(),loaded:true}}, focusedId:1,pending:[],error:null }); }); setArtifactState({summaries:[artifact(44,"Review findings"),artifact(55,"Release notes")],listed:true}); });
  attachThreadTransport(frame => frames.push(frame));attachArtifactTransport(frame => artifactFrames.push(frame));
});
afterEach(() => { flush(() => { closeArtifact(); resetArtifacts(); }); disconnectThreads(); vi.unstubAllGlobals(); });
function preview(view: ReturnType<typeof render>, id: number) {
  fireEvent.click(view.container.querySelector(`[data-artifact-ref="${id}"]`)!);
  const request = artifactFrames.at(-1)!;
  if(request.type !== "open_artifact")throw new Error("Missing artifact request");
  flush(() => handleArtifactMessage({type:"artifact_opened",client_id:request.client_id,artifact:{...artifact(id,id===44?"Review findings":"Release notes"),content:"Artifact body"}}));
}
describe("explicit human artifact context", () => {
  it("stages from unrelated global preview, preserves views/close/recipient, then sends exact owner references", () => {
    const view=render(()=> <ThreadShell />);
    fireEvent.input(view.getByRole("textbox",{name:"Message A"}),{target:{value:"Make this simpler"}});
    fireEvent.click(view.getByRole("button",{name:"All artifacts"}));preview(view,44);
    expect(draftArtifact(1)).toEqual({id:44,title:"Review findings"});expect(frames).toEqual([]);
    expect(artifactState.summaries.find(row=>row.id===44)?.thread_ids).toEqual([2]);
    fireEvent.click(within(view.getByLabelText("Artifact preview")).getByRole("button",{name:"Back to conversation"}));
    expect(view.getByText("About Review findings")).toBeInTheDocument();expect(threadState.focusedId).toBe(1);
    fireEvent.click(view.getByRole("button",{name:"Conversation"}));
    expect(view.getByRole("textbox",{name:"Message A"})).toHaveValue("Make this simpler");
    fireEvent.click(view.getByRole("button",{name:"Send"}));
    const sent=frames.find(frame=>frame.type==="send_thread_message");expect(sent).toMatchObject({thread_id:1,body:"Make this simpler",artifact_ids:[44],attachments:[],mentions:[]});expect(draftArtifact(1)).toBeNull();
    if(sent?.type!=="send_thread_message")throw new Error("Missing send");
    flush(()=>handleThreadMessage({type:"msg",message:{id:88,thread_id:1,author:"owner",client_id:sent.client_id,body:sent.body,artifact_ids:[44],ref:null,ts:"2026-09-10T10:01:00Z"}}));
    expect(view.container.querySelector('[data-message-id="88"] [data-artifact-ref="44"]')).toBeInTheDocument();
  });
  it("replaces/removes context without transferring it across Threads or automatic preview refresh", () => {
    const view=render(()=> <ThreadShell />);fireEvent.click(view.getByRole("button",{name:"All artifacts"}));preview(view,44);
    flush(()=>focusThread(2));expect(draftArtifact(2)).toBeNull();expect(draftArtifact(1)?.id).toBe(44);
    fireEvent.click(view.getByRole("button",{name:"Use in message"}));expect(draftArtifact(2)?.id).toBe(44);
    flush(()=>{closeArtifact();focusThread(1);});preview(view,55);flush(closeArtifact);expect(draftArtifact(1)?.id).toBe(55);
    fireEvent.click(view.getByRole("button",{name:"Remove artifact context: Release notes"}));expect(draftArtifact(1)).toBeNull();
    flush(()=>handleArtifactMessage({type:"artifact_upsert",artifact:artifact(55,"Release notes")}));expect(draftArtifact(1)).toBeNull();
    fireEvent.input(view.getByRole("textbox",{name:"Message A"}),{target:{value:"Edit artifact44"}});fireEvent.click(view.getByRole("button",{name:"Send"}));expect(frames.find(frame=>frame.type==="send_thread_message")).toMatchObject({artifact_ids:[]});
  });
  it("stages no recipient from overview and discards reference IDs on history reset", () => {
    const view=render(()=> <ThreadShell />);fireEvent.click(view.getByRole("button",{name:"All artifacts"}));preview(view,44);flush(closeArtifact);
    expect(localStorage.getItem("hirsel.artifact-context.artifact-context-history:thread-1")).toContain("44");
    flush(()=>focusThread(null));preview(view,55);expect(draftArtifact(1)?.id).toBe(44);expect(view.queryByRole("button",{name:"Use in message"})).toBeNull();
    flush(()=>{setHistoryId("new-history");resetThreads();resetArtifacts();});expect(draftArtifact(1)).toBeNull();
    expect(localStorage.getItem("hirsel.artifact-context.artifact-context-history:thread-1")).toBeNull();
  });
});
