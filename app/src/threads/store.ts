import { parseThreadLink, threadPath } from "../lib/thread-url";
import { historyId, preservePendingDrafts } from "../lib/history";
import { createStore, reconcile } from "solid-js";

import type { Blob, ChatMessage, SendMode, ServerMessage } from "../protocol";
import type { TimelineEvent } from "../store/types";
import { emptyHistory, mergeById, mergeDetail, mergeTurns, upsertThread, type ThreadHistory } from "./model";
import type { Thread, ThreadClientMessage, ThreadKind } from "./types";

interface PendingMessage {
  clientId: string;
  historyId: string;
  threadId: number;
  body: string;
  attachments: Blob[];
  mentions: number[];
  artifactIds: number[];
  mode: SendMode;
  failed: boolean;
}
export interface ThreadFailure { operation: "load" | "send" | "request"; detail: string; threadId?: number; beforeId?: number | null; clientId?: string }
interface ThreadState {
  threads: Thread[];
  histories: Record<number, ThreadHistory>;
  /** The one live-and-replayed timeline projection, keyed by durable turn ID. */
  turnDetails: Record<number, TimelineEvent[]>;
  removedMessageIds: Record<number, true>;
  pending: PendingMessage[];
  focusedId: number | null;
  error: ThreadFailure | null;
  linkError: string | null;
  ready: boolean;
}
export function routeThreadId(path = location.pathname + location.search): number | null {
  const link = parseThreadLink(path);
  return link?.kind === "thread" && link.target.history_id === historyId() ? link.target.thread_id : null;
}
/** Resolve a copied link only against the authoritative connected history. */
export function followThreadLocation(authoritativeHistory: string | null = historyId()): void {
  const link = parseThreadLink(location.pathname + location.search);
  if (!link) { focusThread(null, false); return; }
  if (!threadState.ready) { setThreadState(draft => { draft.focusedId = null; draft.linkError = null; }); return; }
  let problem: string | null = null;
  if (link.kind !== "thread") problem = link.kind === "incomplete" ? "This Thread link is incomplete. Copy a new link from its Thread." : "This Thread link is invalid.";
  else if (link.target.history_id !== authoritativeHistory) problem = "This link belongs to another Hirsel history.";
  else if (!threadState.threads.some(thread => thread.id === link.target.thread_id)) problem = `Thread #${link.target.thread_id} is unavailable.`;
  if (problem) {
    setThreadState(draft => { draft.focusedId = null; draft.linkError = problem; draft.error = null; });
    return;
  }
  if (link.kind === "thread") focusThread(link.target.thread_id, false, authoritativeHistory);
}
function rememberSelection(id: number, currentHistory = historyId()): void {
  const key = currentHistory ? `hirsel.last-thread.${currentHistory}` : null; if (key) localStorage.setItem(key, String(id));
}
function forgetSelection(currentHistory = historyId()): void {
  const key = currentHistory ? `hirsel.last-thread.${currentHistory}` : null;
  if (key) localStorage.removeItem(key);
}
function restoredSelection(threads: Thread[], currentHistory = historyId()): number | null {
  const key = currentHistory ? `hirsel.last-thread.${currentHistory}` : null; const saved = key ? localStorage.getItem(key) : null;
  if (saved === null || !/^\d+$/.test(saved)) return null;
  const id = Number(saved);
  const thread = threads.find(candidate => candidate.id === id);
  if (thread?.archived_at) { forgetSelection(currentHistory); return null; }
  return thread ? id : null;
}
export const [threadState, setThreadState] = createStore<ThreadState>({
  threads: [], histories: {}, turnDetails: {}, removedMessageIds: {}, pending: [], focusedId: null, error: null, linkError: null, ready: false,
});
let historyGeneration = 0;
let selectionGeneration = 0;
let sendFrame: ((frame: ThreadClientMessage) => void) | null = null;
const messageTimers = new Map<string, ReturnType<typeof setTimeout>>();
const MESSAGE_ACK_TIMEOUT_MS = 20_000;
type RequestKind = "create" | "open" | "action";
interface PendingRequest {
  kind: RequestKind;
  action?: string;
  selectionGeneration?: number;
  historyId?: string;
  threadId?: number;
  beforeId: number | null;
  onFailure?: (detail: string) => void;
  resolve: (result: unknown) => void;
  reject: (error: Error) => void;
  timer: ReturnType<typeof setTimeout>;
}
const requests = new Map<string, PendingRequest>();
export function attachThreadTransport(send: (frame: ThreadClientMessage) => void): void { sendFrame = send; }
function failRequest(id: string, detail: string): void {
  const request = requests.get(id);
  if (!request) return;
  clearTimeout(request.timer);
  requests.delete(id);
  request.onFailure?.(detail);
  request.reject(new Error(detail));
}
export function disconnectThreads(): void {
  setThreadState(draft => { draft.ready = false; });
  sendFrame = null;
  for (const id of requests.keys()) failRequest(id, "Connection interrupted. Please try again.");
  for (const timer of messageTimers.values()) clearTimeout(timer);
  messageTimers.clear();
}
function request(frame: Extract<ThreadClientMessage, { client_id: string }>, kind: RequestKind, threadId?: number, beforeId: number | null = null, history?: string, onFailure?: (detail: string) => void, action?: string, selection?: number): Promise<unknown> {
  return new Promise((resolve, reject) => {
    if (!sendFrame) { reject(new Error("Not connected")); return; }
    const timer = setTimeout(() => failRequest(frame.client_id, "Thread request timed out"), 20_000);
    requests.set(frame.client_id, { kind, action, selectionGeneration: selection, historyId: history, threadId, beforeId, onFailure, resolve, reject, timer });
    sendFrame(frame);
  });
}
export async function createThread(expectedHistory: string, title: string, kind: ThreadKind, parentId: number | null): Promise<Thread> {
  if (!threadState.ready) throw new Error("Reconnect before creating a Thread.");
  if (historyId() !== expectedHistory) throw new Error("History changed. Reopen this control and try again.");
  return await request({ type: "create_thread", client_id: crypto.randomUUID(), history_id: expectedHistory, title, kind, parent_thread_id: parentId }, "create", undefined, null, expectedHistory) as Thread;
}
export async function openThread(id: number, beforeId: number | null = null): Promise<void> {
  const generation = historyGeneration;
  try {
    await request({ type: "open_thread", client_id: crypto.randomUUID(), thread_id: id, before_id: beforeId }, "open", id, beforeId);
    setThreadState(draft => { if (draft.error?.operation === "load" && draft.error.threadId === id) draft.error = null; });
  } catch (error) {
    if (generation === historyGeneration && threadState.focusedId === id) setThreadState(draft => { draft.error = { operation: "load", detail: error instanceof Error ? error.message : String(error), threadId: id, beforeId }; });
    throw error;
  }
}
export function focusThread(id: number | null, updateUrl = true, currentHistory = historyId()): void {
  if (id !== null && (!threadState.ready || !currentHistory)) return;
  selectionGeneration++;
  setThreadState(draft => { draft["focusedId"] = id; });
  setThreadState(draft => { draft["error"] = null; draft.linkError = null; });
  if (updateUrl) history.pushState(null, "", id === null ? "/" : threadPath({kind:"thread", history_id:currentHistory!, thread_id:id}));
  if (id === null) forgetSelection();
  if (id !== null) {
    if (threadState.threads.some(thread => thread.id === id)) rememberSelection(id, currentHistory);
    if (sendFrame) void openThread(id).catch(() => {});
  }
}
function clearArchivedFocus(id: number, currentHistory = historyId()): void {
  if (!currentHistory || historyId() !== currentHistory || threadState.focusedId !== id) return;
  selectionGeneration++;
  setThreadState(draft => { draft.focusedId = null; draft.error = null; draft.linkError = null; });
  forgetSelection(currentHistory);
  history.replaceState(null, "", "/");
}
export function threadAction(expectedHistory: string, id: number, action: string, data: unknown = {}, expectedRevision?: number): void {
  if (!sendFrame) { setThreadState(draft => { draft["error"] = { operation: "request", detail: "Reconnect before changing this thread.", threadId: id }; }); return; }
  if (!threadState.ready || historyId() !== expectedHistory) { setThreadState(draft => { draft["error"] = { operation: "request", detail: "History changed. Reopen this control and try again.", threadId: id }; }); return; }
  const clientId = crypto.randomUUID();
  const generation = historyGeneration;
  const onFailure = (detail: string) => {
    if (generation === historyGeneration && historyId() === expectedHistory) setThreadState(draft => {
      draft.error = { operation: "request", detail, threadId: id, clientId };
    });
  };
  const selection = action === "archive" && threadState.focusedId === id ? selectionGeneration : undefined;
  void request({ type: "thread_action", client_id: clientId, history_id: expectedHistory, thread_id: id, action, data, expected_revision: expectedRevision }, "action", id, null, expectedHistory, onFailure, action, selection).catch(() => {});
}
function pendingFrame(pending: PendingMessage): ThreadClientMessage {
  return { type: "send_thread_message", client_id: pending.clientId, history_id: pending.historyId, thread_id: pending.threadId,
    body: pending.body, attachments: pending.attachments.map(b => b.id), mentions: pending.mentions, artifact_ids: [...pending.artifactIds], mode: pending.mode };
}
function clearMessageTimer(clientId: string): void {
  clearTimeout(messageTimers.get(clientId));
  messageTimers.delete(clientId);
}
function acknowledgeMessage(clientId: string): void {
  clearMessageTimer(clientId);
  setThreadState(draft => { draft["pending"] = (rows => rows.filter(p => p.clientId !== clientId))(draft["pending"]);
    if (draft.error?.operation === "send" && draft.error.clientId === clientId) draft.error = null; });
}
function transmitMessage(pending: PendingMessage): void {
  if (!sendFrame) return;
  clearMessageTimer(pending.clientId);
  messageTimers.set(pending.clientId, setTimeout(() => {
    messageTimers.delete(pending.clientId);
    setThreadState(draft => { draft.pending = draft.pending.map(item => ({ ...item, failed: item.clientId === pending.clientId ? true : item.failed })); });
  }, MESSAGE_ACK_TIMEOUT_MS));
  sendFrame(pendingFrame(pending));
}
export function sendThreadMessage(expectedHistory: string, threadId: number, body: string, mode: SendMode, attachments: Blob[], mentions: number[], artifactIds: number[]): void {
  if (!threadState.ready) throw new Error("Reconnect before sending to a Thread.");
  if (historyId() !== expectedHistory) throw new Error("History changed. Reopen this Thread before sending.");
  const references = [...new Set(artifactIds)].sort((a,b) => a-b);
  if (references.length > 16 || references.some(id => !Number.isSafeInteger(id) || id < 0)) throw new Error("A message supports at most 16 valid artifact references.");
  const pending: PendingMessage = { clientId: crypto.randomUUID(), historyId: expectedHistory, threadId, body, attachments, mentions, artifactIds: references, mode, failed: false };
  setThreadState(draft => { draft["pending"] = (rows => [...rows, pending])(draft["pending"]); });
  transmitMessage(pending);
}
export function retryThreadMessage(clientId: string): void {
  const pending = threadState.pending.find(p => p.clientId === clientId);
  if (!pending) return;
  setThreadState(draft => { draft.pending = draft.pending.map(item => ({ ...item, failed: item.clientId === clientId ? false : item.failed })); });
  transmitMessage(pending);
}
function receiveMessage(message: ChatMessage): void {
  // A delayed echo still acknowledges its outgoing request, but cancellation
  // is authoritative even when that echo or a snapshot arrives afterwards.
  if (message.author === "owner" && message.client_id) acknowledgeMessage(message.client_id);
  if (threadState.removedMessageIds[message.id]) return;
  const threadId = message.thread_id;
  const prior = threadState.histories[threadId] ?? emptyHistory();
  setThreadState(draft => { draft["histories"][threadId] = { ...prior, messages: mergeById(prior.messages, [message]) }; });

}
function removeMessage(id: number): void {
  const removedTurnIds = new Set(Object.values(threadState.histories).flatMap(history => history.turns.filter(turn => turn.owner_message_id === id || turn.agent_message_id === id).map(turn => turn.id)));
  setThreadState(draft => { draft["removedMessageIds"][id] = true; });
  for (const [threadId, history] of Object.entries(threadState.histories)) {
    const removed = history.messages.find(message => message.id === id);
    if (!removed) continue;
    if (removed.client_id) acknowledgeMessage(removed.client_id);
    setThreadState(draft => { draft["histories"][Number(threadId)]["messages"] = (rows => rows.filter(message => message.id !== id))(draft["histories"][Number(threadId)]["messages"]); });
  }
  setThreadState(draft => { reconcile(Object.fromEntries(Object.entries(threadState.turnDetails).filter(([turnId]) => !removedTurnIds.has(Number(turnId)))))(draft["turnDetails"]); });
}

function mergeTimelineEvents(
  turnId: number,
  threadId: number,
  incoming: TimelineEvent[],
): void {
  const prior = threadState.turnDetails[turnId] ?? [];
  const bySequence = new Map(prior.map(row => [row.seq, row]));
  for (const row of incoming) {
    const existing = bySequence.get(row.seq);
    if (existing) {
      if (JSON.stringify(existing.event) !== JSON.stringify(row.event)) {
        setThreadState(draft => { draft.error = { operation: "load", detail: `Conflicting timeline event for turn ${turnId}, sequence ${row.seq}.`, threadId }; });
      }
      continue;
    }
    bySequence.set(row.seq, row);
  }
  setThreadState(draft => { draft.turnDetails[turnId] = [...bySequence.values()].sort((a, b) => a.seq - b.seq); });
}
export function handleThreadMessage(message: ServerMessage): void {
  // Several protocol records can arrive before Solid commits its microtask.
  // A single draft scope reads its own writes, including nested message helpers.
  setThreadState(() => {
  switch (message.type) {
    case "hello_ok": {
      setThreadState(draft => { reconcile(message.threads, "id")(draft["threads"]); draft.ready = true; });
      for (const pending of threadState.pending) if (!pending.failed) transmitMessage(pending);
      const route = parseThreadLink(location.pathname + location.search);
      if (route) { followThreadLocation(message.history_id); break; }
      const prior = threadState.focusedId;
      const selected = prior !== null && message.threads.some(thread => thread.id === prior && !thread.archived_at) ? prior : restoredSelection(message.threads, message.history_id);
      if (selected !== prior) selectionGeneration++;
      setThreadState(draft => { draft.focusedId = selected; draft.linkError = null; });
      if (selected !== null) {
        rememberSelection(selected, message.history_id);
        history.replaceState(null, "", threadPath({kind:"thread",history_id:message.history_id,thread_id:selected}));
        void openThread(selected).catch(() => {});
      }
      break;
    }
    case "thread_upsert":
    case "thread_created": {
      const priorThread = threadState.threads.find(thread => thread.id === message.thread.id);
      const archivedFocusedThread = message.type === "thread_upsert"
        && message.thread.id === threadState.focusedId
        && priorThread !== undefined
        && priorThread.revision <= message.thread.revision
        && !priorThread.archived_at
        && Boolean(message.thread.archived_at);
      setThreadState(draft => { reconcile(upsertThread(threadState.threads, message.thread), "id")(draft["threads"]); });
      if (message.type === "thread_created") {
        const pending = requests.get(message.client_id);
        if (pending?.kind === "create") { clearTimeout(pending.timer); requests.delete(message.client_id); pending.resolve(message.thread); }
      }
      if (archivedFocusedThread) clearArchivedFocus(message.thread.id);
      break;
    }
    case "thread_action_applied": {
      const pending = requests.get(message.client_id);
      if (pending?.kind !== "action" || pending.historyId !== message.history_id || pending.threadId !== message.thread_id) break;
      clearTimeout(pending.timer); requests.delete(message.client_id);
      setThreadState(draft => {
        if (draft.error?.operation === "request" && draft.error.clientId === message.client_id) draft.error = null;
      });
      if (pending.action === "archive" && pending.selectionGeneration === selectionGeneration) clearArchivedFocus(message.thread_id, message.history_id);
      pending.resolve(undefined);
      break;
    }
    case "thread_opened": {
      const pending = requests.get(message.client_id);
      if (pending?.kind !== "open" || pending.threadId !== message.detail.thread.id) break;
      clearTimeout(pending.timer); requests.delete(message.client_id);
      const id = message.detail.thread.id;
      setThreadState(draft => { reconcile(upsertThread(threadState.threads, message.detail.thread), "id")(draft["threads"]); });
      const detail = { ...message.detail, messages: message.detail.messages.filter(row => !threadState.removedMessageIds[row.id]) };
      setThreadState(draft => { draft["histories"][id] = mergeDetail(threadState.histories[id] ?? emptyHistory(), detail, pending.beforeId !== null); });
      for (const timeline of detail.turn_timelines) {
        const turn = detail.turns.find(turn => turn.id === timeline.turn_id);
        if (!turn || turn.thread_id !== id) {
          setThreadState(draft => { draft.error = { operation: "load", detail: `Timeline turn ${timeline.turn_id} does not belong to Thread #${id}.`, threadId: id }; });
          continue;
        }
        mergeTimelineEvents(timeline.turn_id, id, timeline.events);
      }
      for (const row of message.detail.messages) if (row.client_id) acknowledgeMessage(row.client_id);
      pending.resolve(detail);
      break;
    }
    case "msg": receiveMessage(message.message); break;
    case "msg_removed": removeMessage(message.id); break;
    case "thread_turn": {
      const id = message.turn.thread_id;
      const prior = threadState.histories[id] ?? emptyHistory();
      setThreadState(draft => { draft["histories"][id] = { ...prior, turns: mergeTurns(prior.turns, [message.turn]) }; });
      break;
    }
    case "thread_activity": {
      const id = message.activity.thread_id;
      const prior = threadState.histories[id] ?? emptyHistory();
      setThreadState(draft => {
        const assignment = message.activity.kind === "delegation_received" && typeof message.activity.data === "object" && message.activity.data !== null ? message.activity.data as { brief: string } : null;
        const newest = !prior.activities.some(activity => activity.kind === "delegation_received" && activity.id > message.activity.id);
        draft.histories[id] = { ...prior, activities: mergeById(prior.activities, [message.activity]),
          brief: assignment && newest ? { text: assignment.brief, artifact_ids: message.activity.artifact_ids } : prior.brief };
      });
      break;
    }
    case "turn_event":
      mergeTimelineEvents(message.turn_id, message.thread_id, [{ seq: message.seq, event: message.event }]);
      break;
    case "error": {
      if (message.client_id) {
        if (requests.has(message.client_id)) { failRequest(message.client_id, message.detail); break; }
        const pending = threadState.pending.find(item => item.clientId === message.client_id);
        if (!pending) break;
        clearMessageTimer(message.client_id);
        setThreadState(draft => { draft.pending = draft.pending.map(item => ({ ...item, failed: item.clientId === message.client_id ? true : item.failed }));
          draft.error = { operation: "send", detail: message.detail, threadId: pending.threadId, clientId: message.client_id }; });
      } else setThreadState(draft => { draft.error = { operation: "request", detail: message.detail }; });
      break;
    }
  }
  });
}

export function focusedThreadRunning(): boolean {
  const id = threadState.focusedId;
  return id !== null && (threadState.histories[id]?.turns.some(turn => turn.state === "running") ?? false);
}

export function resetThreads(): void {
  historyGeneration++;
  selectionGeneration++;
  preservePendingDrafts(threadState.pending);
  disconnectThreads();
  setThreadState(draft => { Object.assign(draft, { threads: [], histories: {}, turnDetails: {}, removedMessageIds: {}, pending: [], focusedId: null, error: null, linkError: null, ready: false }); });
  // Keep an incoming qualified destination until the next hello validates its history.
}
