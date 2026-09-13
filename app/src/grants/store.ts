import { createStore } from "solid-js";
import { historyId } from "../lib/history";
import type { ServerMessage } from "../protocol";
import type { ReachTarget, ThreadClientMessage, ThreadGrant } from "../threads/types";

export interface GrantOrigin { readonly historyId: string; readonly threadId: number }
interface ReachList { grants: ThreadGrant[]; revision: number; loaded: boolean }
export const [grantState, setGrantState] = createStore<{ lists: Record<number, ReachList> }>({ lists: {} });
let transport: ((frame: ThreadClientMessage) => void) | null = null;
interface Pending { origin: GrantOrigin; resolve: () => void; reject: (error: Error) => void; timer: ReturnType<typeof setTimeout> }
const pending = new Map<string, Pending>();

export function attachGrantTransport(send: (frame: ThreadClientMessage) => void): void { transport = send; }
function finish(id: string, error?: string): void {
  const request = pending.get(id); if (!request) return;
  clearTimeout(request.timer); pending.delete(id);
  if (error) request.reject(new Error(error)); else request.resolve();
}
export function disconnectGrants(): void {
  transport = null;
  for (const id of pending.keys()) finish(id, "Reconnect and try again.");
}
export function resetGrants(): void {
  for (const id of pending.keys()) finish(id, "History changed. Open the Thread again.");
  setGrantState(draft => { draft.lists = {}; });
}
/** True when this Thread holds reach over everything, now and later. */
export function holdsRoot(threadId: number): boolean {
  return threadGrants(threadId).some(grant => grant.target.kind === "root");
}
/** What a Thread can address, in the one line the Agent reads in its own context. */
export function reachSummary(threadId: number): string {
  if (holdsRoot(threadId)) return "everything (root)";
  const grants = grantState.lists[threadId]?.grants ?? [];
  return ["self + subtree", ...grants.flatMap(grant => grant.target.kind === "thread" ? [`+Thread ${grant.target.thread_id} '${grant.target.title}'`] : [])].join(" · ");
}
export function threadGrants(threadId: number): ThreadGrant[] { return grantState.lists[threadId]?.grants ?? []; }
function request(origin: GrantOrigin, frame: Extract<ThreadClientMessage, {client_id: string}>): Promise<void> {
  return new Promise((resolve, reject) => {
    if (origin.historyId !== historyId()) { reject(new Error("History changed. Open the Thread again.")); return; }
    if (!transport) { reject(new Error("Reconnect and try again.")); return; }
    pending.set(frame.client_id, { origin, resolve, reject, timer: setTimeout(() => finish(frame.client_id, "Request timed out. Try again."), 20_000) });
    transport(frame);
  });
}
export async function grantReach(origin: GrantOrigin, target: ReachTarget, note: string | null): Promise<void> {
  await request(origin, { type: "grant_thread_reach", client_id: crypto.randomUUID(), history_id: origin.historyId, thread_id: origin.threadId, target, note });
}
export async function revokeReach(origin: GrantOrigin, target: ReachTarget): Promise<void> {
  await request(origin, { type: "revoke_thread_reach", client_id: crypto.randomUUID(), history_id: origin.historyId, thread_id: origin.threadId, target });
}
/** Reach snapshots order only against prior reach snapshots: Thread metadata
 * may already be newer and must not roll a grant list back. */
function receive(threadId: number, revision: number, grants: ThreadGrant[]): void {
  if (revision < (grantState.lists[threadId]?.revision ?? -1)) return;
  setGrantState(draft => { draft.lists[threadId] = { grants, revision, loaded: true }; });
}
export function handleGrantMessage(message: ServerMessage): void {
  if (message.type === "thread_grants_changed") {
    if (message.history_id !== historyId()) return;
    receive(message.thread_id, message.revision, message.grants);
    if (message.client_id) finish(message.client_id);
  } else if (message.type === "thread_opened") {
    receive(message.detail.thread.id, message.detail.thread.revision, message.detail.grants);
  } else if (message.type === "error" && message.client_id) finish(message.client_id, message.detail);
}
