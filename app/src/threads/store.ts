import { createStore, reconcile } from "solid-js";

import type { Blob, ChatMessage, SendMode, ServerMessage } from "../protocol";
import type { TimelineEvent } from "../store/types";
import { emptyHistory, mergeById, mergeDetail, upsertThread, type ThreadHistory } from "./model";
import type { Thread, ThreadClientMessage, ThreadDetail } from "./types";

interface PendingMessage {
  clientId: string;
  threadId: number;
  body: string;
  attachments: Blob[];
  mentions: number[];
  mode: SendMode;
  failed: boolean;
}
export interface ThreadFailure { operation: "load" | "send" | "request"; detail: string; threadId?: number; beforeId?: number | null }
interface ThreadState {
  threads: Thread[];
  histories: Record<number, ThreadHistory>;
  streams: Record<number, TimelineEvent[]>;
  turnDetails: Record<number, TimelineEvent[]>;
  streamTurnIds: Record<number, number>;
  removedMessageIds: Record<number, true>;
  pending: PendingMessage[];
  focusedId: number;
  error: ThreadFailure | null;
}
function initialFocus(): number {
  const match = /^\/t\/(\d+)$/.exec(location.pathname);
  return match ? Number(match[1]) : 0;
}
export const [threadState, setThreadState] = createStore<ThreadState>({
  threads: [], histories: {}, streams: {}, turnDetails: {}, streamTurnIds: {}, removedMessageIds: {}, pending: [], focusedId: initialFocus(), error: null,
});
let sendFrame: ((frame: ThreadClientMessage) => void) | null = null;
const messageTimers = new Map<string, ReturnType<typeof setTimeout>>();
const MESSAGE_ACK_TIMEOUT_MS = 20_000;
const requests = new Map<string, { threadId?: number; beforeId: number | null; resolve: (result: Thread | ThreadDetail) => void; reject: (error: Error) => void; timer: ReturnType<typeof setTimeout> }>();
export function attachThreadTransport(send: (frame: ThreadClientMessage) => void): void { sendFrame = send; }
function failRequest(id: string, detail: string): void {
  const request = requests.get(id);
  if (!request) return;
  clearTimeout(request.timer);
  requests.delete(id);
  request.reject(new Error(detail));
}
export function disconnectThreads(): void {
  sendFrame = null;
  for (const id of requests.keys()) failRequest(id, "Connection interrupted. Please try again.");
  for (const timer of messageTimers.values()) clearTimeout(timer);
  messageTimers.clear();
}
function request(frame: Extract<ThreadClientMessage, { client_id: string }>, threadId?: number, beforeId: number | null = null): Promise<Thread | ThreadDetail> {
  return new Promise((resolve, reject) => {
    if (!sendFrame) { reject(new Error("Not connected")); return; }
    const timer = setTimeout(() => failRequest(frame.client_id, "Thread request timed out"), 20_000);
    requests.set(frame.client_id, { threadId, beforeId, resolve, reject, timer });
    sendFrame(frame);
  });
}
export async function createThread(title: string): Promise<Thread> {
  return await request({ type: "create_thread", client_id: crypto.randomUUID(), title }) as Thread;
}
export async function openThread(id: number, beforeId: number | null = null): Promise<void> {
  try {
    await request({ type: "open_thread", client_id: crypto.randomUUID(), thread_id: id, before_id: beforeId }, id, beforeId);
    setThreadState(draft => { if (draft.error?.operation === "load" && draft.error.threadId === id) draft.error = null; });
  } catch (error) {
    if (threadState.focusedId === id) setThreadState(draft => { draft.error = { operation: "load", detail: error instanceof Error ? error.message : String(error), threadId: id, beforeId }; });
    throw error;
  }
}
export function focusThread(id: number, updateUrl = true): void {
  setThreadState(draft => { draft["focusedId"] = id; });
  setThreadState(draft => { draft["error"] = null; });
  if (updateUrl) history.pushState(null, "", id === 0 ? "/" : `/t/${id}`);
  if (sendFrame) void openThread(id).catch(() => {});
}
export function threadAction(id: number, action: string, data: unknown = {}, expectedRevision?: number): void {
  if (!sendFrame) { setThreadState(draft => { draft["error"] = { operation: "request", detail: "Reconnect before changing this thread.", threadId: id }; }); return; }
  sendFrame({ type: "thread_action", thread_id: id, action, data, expected_revision: expectedRevision });
}
function pendingFrame(pending: PendingMessage): ThreadClientMessage {
  return { type: "send_thread_message", client_id: pending.clientId, thread_id: pending.threadId,
    body: pending.body, attachments: pending.attachments.map(b => b.id), mentions: pending.mentions, mode: pending.mode };
}
function clearMessageTimer(clientId: string): void {
  clearTimeout(messageTimers.get(clientId));
  messageTimers.delete(clientId);
}
function acknowledgeMessage(clientId: string): void {
  clearMessageTimer(clientId);
  setThreadState(draft => { draft["pending"] = (rows => rows.filter(p => p.clientId !== clientId))(draft["pending"]); });
}
function transmitMessage(pending: PendingMessage): void {
  if (!sendFrame) return;
  clearMessageTimer(pending.clientId);
  messageTimers.set(pending.clientId, setTimeout(() => {
    messageTimers.delete(pending.clientId);
    setThreadState(draft => { for (const item of draft["pending"].filter(p => p.clientId === pending.clientId)) { item["failed"] = true; } });
  }, MESSAGE_ACK_TIMEOUT_MS));
  sendFrame(pendingFrame(pending));
}
export function sendThreadMessage(threadId: number, body: string, mode: SendMode, attachments: Blob[], mentions: number[]): void {
  const pending: PendingMessage = { clientId: crypto.randomUUID(), threadId, body, attachments, mentions, mode, failed: false };
  setThreadState(draft => { draft["pending"] = (rows => [...rows, pending])(draft["pending"]); });
  transmitMessage(pending);
}
export function retryThreadMessage(clientId: string): void {
  const pending = threadState.pending.find(p => p.clientId === clientId);
  if (!pending) return;
  setThreadState(draft => { for (const item of draft["pending"].filter(p => p.clientId === clientId)) { item["failed"] = false; } });
  transmitMessage(pending);
}
function receiveMessage(message: ChatMessage): void {
  // A delayed echo still acknowledges its outgoing request, but cancellation
  // is authoritative even when that echo or a snapshot arrives afterwards.
  if (message.author === "owner" && message.client_id) acknowledgeMessage(message.client_id);
  if (threadState.removedMessageIds[message.id]) return;
  const threadId = message.thread_id ?? 0;
  const prior = threadState.histories[threadId] ?? emptyHistory();
  setThreadState(draft => { draft["histories"][threadId] = { ...prior, messages: mergeById(prior.messages, [message]) }; });
  if (message.author === "agent" && threadState.streamTurnIds[threadId] === undefined && (threadState.streams[threadId]?.length ?? 0) > 0) {
    setThreadState(draft => { draft["turnDetails"][message.id] = [...threadState.streams[threadId]]; });
    setThreadState(draft => { draft["streams"][threadId] = []; });
  }
}
function removeMessage(id: number): void {
  setThreadState(draft => { draft["removedMessageIds"][id] = true; });
  for (const [threadId, history] of Object.entries(threadState.histories)) {
    const removed = history.messages.find(message => message.id === id);
    if (!removed) continue;
    if (removed.client_id) acknowledgeMessage(removed.client_id);
    setThreadState(draft => { draft["histories"][Number(threadId)]["messages"] = (rows => rows.filter(message => message.id !== id))(draft["histories"][Number(threadId)]["messages"]); });
  }
  setThreadState(draft => { reconcile(Object.fromEntries(Object.entries(threadState.turnDetails).filter(([messageId]) => Number(messageId) !== id)))(draft["turnDetails"]); });
}
export function handleThreadMessage(message: ServerMessage): void {
  // Several protocol records can arrive before Solid commits its microtask.
  // A single draft scope reads its own writes, including nested message helpers.
  setThreadState(() => {
  switch (message.type) {
    case "hello_ok":
      setThreadState(draft => { reconcile(message.threads ?? [], "id")(draft["threads"]); });
      for (const row of message.messages) receiveMessage(row);
      for (const pending of threadState.pending) if (!pending.failed) transmitMessage(pending);
      void openThread(threadState.focusedId).catch(() => {});
      break;
    case "thread_upsert":
    case "thread_created":
      setThreadState(draft => { reconcile(upsertThread(threadState.threads, message.thread), "id")(draft["threads"]); });
      if (message.type === "thread_created") {
        const pending = requests.get(message.client_id);
        if (pending) { clearTimeout(pending.timer); requests.delete(message.client_id); pending.resolve(message.thread); }
      }
      break;
    case "thread_opened": {
      const pending = requests.get(message.client_id);
      if (!pending || pending.threadId !== message.detail.thread.id) break;
      clearTimeout(pending.timer); requests.delete(message.client_id);
      const id = message.detail.thread.id;
      setThreadState(draft => { reconcile(upsertThread(threadState.threads, message.detail.thread), "id")(draft["threads"]); });
      const detail = { ...message.detail, messages: message.detail.messages.filter(row => !threadState.removedMessageIds[row.id]) };
      setThreadState(draft => { draft["histories"][id] = mergeDetail(threadState.histories[id] ?? emptyHistory(), detail, pending.beforeId !== null); });
      for (const row of message.detail.messages) if (row.client_id) acknowledgeMessage(row.client_id);
      pending.resolve(detail);
      break;
    }
    case "msg": receiveMessage(message.message); break;
    case "msg_removed": removeMessage(message.id); break;
    case "thread_turn": {
      const id = message.turn.thread_id;
      const prior = threadState.histories[id] ?? emptyHistory();
      const previous = prior.turns.find(turn => turn.id === message.turn.id);
      const currentTurnId = threadState.streamTurnIds[id];
      if (message.turn.state === "running" && previous?.state !== "running" && (currentTurnId === undefined || message.turn.id > currentTurnId)) {
        setThreadState(draft => { draft["streams"][id] = []; });
        setThreadState(draft => { draft["streamTurnIds"][id] = message.turn.id; });
      }
      setThreadState(draft => { draft["histories"][id] = { ...prior, turns: mergeById(prior.turns, [message.turn]) }; });
      if (!["queued", "running"].includes(message.turn.state) && (currentTurnId === undefined || message.turn.id >= currentTurnId)) {
        setThreadState(draft => { draft["streamTurnIds"][id] = message.turn.id; });
        if (message.turn.agent_message_id !== null && (threadState.streams[id]?.length ?? 0) > 0) setThreadState(draft => { draft["turnDetails"][message.turn.agent_message_id!] = [...threadState.streams[id]]; });
        setThreadState(draft => { draft["streams"][id] = []; });
      }
      break;
    }
    case "thread_activity": {
      const id = message.activity.thread_id;
      const prior = threadState.histories[id] ?? emptyHistory();
      setThreadState(draft => { draft["histories"][id] = { ...prior, activities: mergeById(prior.activities, [message.activity]) }; });
      break;
    }
    case "turn_event":
      if (message.thread_id !== null && message.thread_id !== undefined) {
        if (message.turn_id !== undefined && message.turn_id !== null) {
          const currentTurnId = Math.max(threadState.streamTurnIds[message.thread_id] ?? 0, ...(threadState.histories[message.thread_id]?.turns.map(t => t.id) ?? []));
          const turn = threadState.histories[message.thread_id]?.turns.find(t => t.id === message.turn_id);
          if ((currentTurnId !== undefined && message.turn_id < currentTurnId) || (turn && !["queued", "running"].includes(turn.state))) break;
          if (threadState.streamTurnIds[message.thread_id] !== message.turn_id) {
            setThreadState(draft => { draft["streams"][message.thread_id!] = []; });
            setThreadState(draft => { draft["streamTurnIds"][message.thread_id!] = message.turn_id!; });
          }
        }
        const prior = threadState.streams[message.thread_id] ?? [];
        if (!prior.some(row => row.seq === message.seq)) setThreadState(draft => { draft["streams"][message.thread_id!] = [...prior, { seq: message.seq, event: message.event }].sort((a, b) => a.seq - b.seq); });
      }
      break;
    case "error": {
      if (message.client_id) {
        if (requests.has(message.client_id)) { failRequest(message.client_id, message.detail); break; }
        const pending = threadState.pending.find(item => item.clientId === message.client_id);
        if (!pending) break;
        clearMessageTimer(message.client_id);
        setThreadState(draft => { for (const item of draft.pending.filter(p => p.clientId === message.client_id)) item.failed = true;
          draft.error = { operation: "send", detail: message.detail, threadId: pending.threadId }; });
      } else setThreadState(draft => { draft.error = { operation: "request", detail: message.detail }; });
      break;
    }
  }
  });
}

export function focusedThreadRunning(): boolean {
  return threadState.histories[threadState.focusedId]?.turns.some(turn => turn.state === "running") ?? false;
}
