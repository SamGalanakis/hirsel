import { createStore } from "solid-js";
import { historyId } from "../lib/history";
import type { ServerMessage } from "../protocol";
import type { RelatedTarget, ThreadClientMessage, ThreadRelatedItem } from "../threads/types";
import { webLink } from "./url";
export interface RelatedOrigin { readonly historyId: string; readonly threadId: number }
interface RelatedList { items: ThreadRelatedItem[]; revision: number; loaded: boolean; loading: boolean; error: string | null }
const empty = (): RelatedList => ({ items: [], revision: -1, loaded: false, loading: false, error: null });
export const [relatedState, setRelatedState] = createStore<{ lists: Record<number, RelatedList> }>({ lists: {} });
let transport: ((frame: ThreadClientMessage) => void) | null = null;
interface Pending { origin: RelatedOrigin; load: boolean; resolve: () => void; reject: (error: Error) => void; timer: ReturnType<typeof setTimeout> }
const pending = new Map<string, Pending>();
const reads = new Map<string, { origin: RelatedOrigin; timer: ReturnType<typeof setTimeout> }>();
function forgetRead(id: string): void { const read = reads.get(id); if (read) clearTimeout(read.timer); reads.delete(id); }
export function trackRelatedRead(frame: ThreadClientMessage, currentHistory = historyId()): void {
  if (frame.type !== "open_thread" || !currentHistory) return;
  forgetRead(frame.client_id);
  reads.set(frame.client_id, { origin: { historyId: currentHistory, threadId: frame.thread_id }, timer: setTimeout(() => forgetRead(frame.client_id), 20_000) });
}
export function attachRelatedTransport(send: (frame: ThreadClientMessage) => void): void { transport = frame => { trackRelatedRead(frame); send(frame); }; }
function finish(id: string, error?: string): void {
  const request = pending.get(id); if (!request) return;
  clearTimeout(request.timer); pending.delete(id);
  if (request.load && request.origin.historyId === historyId()) setRelatedState(draft => {
    const list = draft.lists[request.origin.threadId] ??= empty(); list.loading = false; list.error = error ?? null;
  });
  if (error) request.reject(new Error(error)); else request.resolve();
}
export function disconnectRelated(): void {
  transport = null;
  for (const id of reads.keys()) forgetRead(id);
  for (const id of pending.keys()) finish(id, "Reconnect and try again.");
}
export function resetRelated(): void {
  for (const id of reads.keys()) forgetRead(id);
  for (const id of pending.keys()) finish(id, "History changed. Open the Thread again.");
  setRelatedState(draft => { draft.lists = {}; });
}
function request(origin: RelatedOrigin, frame: Extract<ThreadClientMessage, {client_id: string}>, load = false): Promise<void> {
  return new Promise((resolve, reject) => {
    if (origin.historyId !== historyId()) { reject(new Error("History changed. Open the Thread again.")); return; }
    if (!transport) { reject(new Error("Reconnect and try again.")); return; }
    pending.set(frame.client_id, { origin, load, resolve, reject, timer: setTimeout(() => finish(frame.client_id, "Request timed out. Try again."), 20_000) });
    transport(frame);
  });
}
export async function loadRelated(origin: RelatedOrigin): Promise<void> {
  if (origin.historyId !== historyId()) return;
  setRelatedState(draft => { const list = draft.lists[origin.threadId] ??= empty(); list.loading = true; list.error = null; });
  try { await request(origin, { type: "open_thread", client_id: crypto.randomUUID(), thread_id: origin.threadId, before_id: null }, true); }
  catch (error) { if (origin.historyId === historyId()) setRelatedState(draft => { const list = draft.lists[origin.threadId] ??= empty(); list.loading = false; list.error = error instanceof Error ? error.message : String(error); }); }
}
export function hasRelatedTarget(origin: RelatedOrigin, target: RelatedTarget): boolean {
  return origin.historyId === historyId() && (relatedState.lists[origin.threadId]?.items.some(item => targetKey(item.target) === targetKey(target)) ?? false);
}
export function targetKey(target: RelatedTarget): string | null {
  if (target.kind === "thread") return `thread:${target.history_id}:${target.thread_id}`;
  const url = webLink(target.url)?.url; return url ? `url:${url}` : null;
}
export async function addRelatedItem(origin: RelatedOrigin, target: RelatedTarget, title: string | null): Promise<void> {
  if (target.kind === "thread" && target.history_id !== origin.historyId) throw new Error("This Thread reference belongs to another history.");
  if (target.kind === "url") {
    const link = webLink(target.url); if (!link) throw new Error("Use a complete http or https link without a username or password.");
    target = { kind: "url", url: link.url };
  }
  await request(origin, { type: "add_thread_related", client_id: crypto.randomUUID(), history_id: origin.historyId, thread_id: origin.threadId, target, title });
}
export async function removeRelatedItem(origin: RelatedOrigin, itemId: number): Promise<void> {
  await request(origin, { type: "remove_thread_related", client_id: crypto.randomUUID(), history_id: origin.historyId, thread_id: origin.threadId, item_id: itemId });
}
function receive(threadId: number, revision: number, items: ThreadRelatedItem[]): void {
  const knownRevision = relatedState.lists[threadId]?.revision ?? -1;
  if (revision < knownRevision) return;
  setRelatedState(draft => { draft.lists[threadId] = { items, revision, loaded: true, loading: false, error: null }; });
}
export function handleRelatedMessage(message: ServerMessage): void {
  if (message.type === "thread_related_changed") {
    if (message.history_id !== historyId()) return;
    receive(message.thread_id, message.revision, message.items);
    if (message.client_id && pending.get(message.client_id)?.origin.threadId === message.thread_id) finish(message.client_id);
  } else if (message.type === "thread_opened") {
    const read = reads.get(message.client_id); forgetRead(message.client_id);
    if (!read || read.origin.historyId !== historyId() || read.origin.threadId !== message.detail.thread.id) return;
    receive(message.detail.thread.id, message.detail.thread.revision, message.detail.related_items);
    if (pending.get(message.client_id)?.origin.threadId === message.detail.thread.id) finish(message.client_id);
  } else if (message.type === "error" && message.client_id) finish(message.client_id, message.detail);
}
