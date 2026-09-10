import { describe, expect, it } from "vitest";
import { conversationEntries } from "./conversation";
import { emptyHistory } from "./model";
import type { ChatMessage } from "../protocol";
import type { ThreadTurn } from "./types";
const ts = (minute: number) => `2026-09-10T12:${String(minute).padStart(2, "0")}:00Z`;
const message = (id: number, author: "owner" | "agent"): ChatMessage => ({ id, thread_id: 1, author, body: `Message ${id}`, ref: null, ts: ts(id) });
const turn = (id: number, owner: number | null, agent: number | null): ThreadTurn => ({ requester_thread_id: null, requester_turn_id: null, id, thread_id: 1, owner_message_id: owner, agent_message_id: agent, state: agent ? "completed" : "running", started_at: ts(owner ?? id), finished_at: agent ? ts(agent) : null });
describe("authoritative conversation chronology", () => {
  it("joins final messages exactly, leaves artifact publications independent, positions unfinished turns by their owner", () => {
    const entries = conversationEntries({ ...emptyHistory(), messages: [message(1,"owner"), message(2,"agent"), message(3,"agent"), message(4,"owner")], turns: [turn(10,1,3),turn(11,4,null)] });
    expect(entries.map(row => row.key)).toEqual(["message-1","message-2","turn-10","message-4","turn-11"]);
    expect(entries.filter(row=>row.kind==="message").map(row=>row.turn?.id)).toEqual([undefined,undefined,10,undefined]);
  });
  it("does not dump all historic turns or linked activities when their messages are outside the loaded page", () => {
    const history = { ...emptyHistory(), hasMore: true, messages: [message(5,"owner"),message(6,"agent")], turns: [turn(1,1,2),turn(2,3,null),turn(3,5,6)], activities: [{ artifact_ids: [], id:1,thread_id:1,turn_id:1,kind:"info",data:{description:"old"},ts:ts(1)}] };
    expect(conversationEntries(history).map(row=>row.key)).toEqual(["message-5","turn-3"]);
  });
  it("places ownerless background execution and turnless facts by actual time without assigning ownership", () => {
    const history = { ...emptyHistory(), messages:[message(1,"owner"),message(4,"agent")], turns:[turn(2,null,null)], activities:[{ artifact_ids: [],id:1,thread_id:1,turn_id:null,kind:"session_rotated",data:{generation:2},ts:ts(3)}] };
    expect(conversationEntries(history).map(row=>row.key)).toEqual(["message-1","turn-2","activity-1","message-4"]);
  });
});

it("orders sub-millisecond facts without losing timezone or page-floor precision", () => {
  const messages = [{ ...message(2,"agent"), ts:"2026-09-09T22:00:00.000900Z" }];
  const activity = { artifact_ids: [], id:1,thread_id:1,turn_id:null,kind:"session_rotated",data:{},ts:"2026-09-10T00:00:00.000100+02:00" };
  expect(conversationEntries({ ...emptyHistory(),messages,activities:[activity] }).map(row=>row.key)).toEqual(["activity-1","message-2"]);
  expect(conversationEntries({ ...emptyHistory(),messages,activities:[activity],hasMore:true }).map(row=>row.key)).toEqual(["message-2"]);
});

it("renders the exact current scheduled digest payload as an owner-facing chronological result", async () => {
  const { activityText, ownerFacingActivity } = await import('./conversation');
  const activity={ artifact_ids: [],id:9,thread_id:0,turn_id:null,kind:'scheduled_digest',data:{job_id:'morning',text:'Your morning digest',status:'completed'},ts:ts(2)};
  expect(ownerFacingActivity(activity)).toBe(true);expect(activityText(activity)).toBe('Your morning digest');
  expect(conversationEntries({...emptyHistory(),activities:[activity]}).map(row=>row.key)).toEqual(['activity-9']);
});
