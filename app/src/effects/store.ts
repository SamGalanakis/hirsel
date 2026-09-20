import { createStore } from "solid-js";
import type { ServerMessage } from "../protocol";
import { openArtifact } from "../artifacts/store";
import { openThreadReach } from "../grants/reach";
import { historyId } from "../lib/history";
import { focusThread, openThread, threadAction, threadState } from "../threads/store";
import type { EffectAction, ThreadClientMessage, ThreadEffect, ThreadTurn } from "../threads/types";

interface TurnEffects { threadId: number; effects: ThreadEffect[] }
interface EffectState { turns: Record<number, TurnEffects>; errors: Record<number, string> }
export const [effectState, setEffectState] = createStore<EffectState>({ turns: {}, errors: {} });
let projectionGeneration = 0;
const turnProjectionGeneration = new Map<number, number>();
let transport: ((frame: ThreadClientMessage) => void) | null = null;
const pending = new Map<string, { history: string; threadId: number; turnId: number; sourceTurnId?: number; timer: ReturnType<typeof setTimeout> }>();

export function attachEffectTransport(send: (frame: ThreadClientMessage) => void): void { transport = send; }
export function disconnectEffects(): void {
  transport = null;
  for (const request of pending.values()) clearTimeout(request.timer);
  pending.clear();
}
export function resetEffects(): void {
  disconnectEffects();
  projectionGeneration++;
  turnProjectionGeneration.clear();
  setEffectState(draft => { draft.turns = {}; draft.errors = {}; });
}
export function effectsForTurn(turnId: number | undefined): ThreadEffect[] { return turnId === undefined ? [] : effectState.turns[turnId]?.effects ?? []; }
export function effectErrorForTurn(turnId: number): string | undefined { return effectState.errors[turnId]; }
export function effectSourceThreadId(turnId: number): number | undefined { return effectState.turns[turnId]?.threadId; }
function sorted(effects: ThreadEffect[]): ThreadEffect[] {
  return [...effects].sort((a, b) => a.receipt.id - b.receipt.id);
}
/** Live frames are bounded deltas for one explicitly named turn. */
export function replaceTurnEffects(threadId: number, turnId: number, effects: ThreadEffect[]): void {
  turnProjectionGeneration.set(turnId, ++projectionGeneration);
  setEffectState(draft => {
    const byId = new Map((draft.turns[turnId]?.effects ?? []).map(effect => [effect.receipt.id, effect]));
    for (const effect of effects) byId.set(effect.receipt.id, effect);
    draft.turns[turnId] = { threadId, effects: sorted([...byId.values()]) };
  });
}
export function beginEffectSnapshot(): number { return projectionGeneration; }
/** A fetched page may race a live frame. Merge receipts for only the page's
 * turns without replacing a live projection or deleting an unrelated turn.
 * Live frames use `replaceTurnEffects`; reconnect pages remain authoritative. */
export function mergeDetailEffects(threadId: number, turnIds: number[], effects: ThreadEffect[] = [], requestGeneration = projectionGeneration): void {
  const groups = new Map<number, ThreadEffect[]>();
  for (const effect of effects) {
    const rows = groups.get(effect.receipt.turn_id) ?? [];
    rows.push(effect); groups.set(effect.receipt.turn_id, rows);
  }
  setEffectState(draft => {
    for (const turnId of new Set(turnIds)) {
      const byId = new Map((draft.turns[turnId]?.effects ?? []).map(effect => [effect.receipt.id, effect]));
      const snapshotIsCurrent = (turnProjectionGeneration.get(turnId) ?? 0) <= requestGeneration;
      for (const effect of groups.get(turnId) ?? []) if (snapshotIsCurrent || !byId.has(effect.receipt.id)) byId.set(effect.receipt.id, effect);
      draft.turns[turnId] = { threadId, effects: sorted([...byId.values()]) };
    }
  });
}
function finish(clientId: string, error?: string): void {
  const request = pending.get(clientId); if (!request) return;
  clearTimeout(request.timer); pending.delete(clientId);
  if (request.history !== historyId()) return;
  if (request.sourceTurnId !== undefined) setEffectState(draft => {
    if (error) draft.errors[request.sourceTurnId!] = error;
    else delete draft.errors[request.sourceTurnId!];
  });
  if (error && request.sourceTurnId !== undefined) {
    const source = effectState.turns[request.sourceTurnId];
    if (source) void openThread(source.threadId).catch(() => {});
  }
}
export function handleEffectMessage(message: ServerMessage): void {
  if (message.type === "thread_effects_changed") {
    if (message.history_id !== historyId()) return;
    replaceTurnEffects(message.thread_id, message.turn_id, message.effects);
  } else if (message.type === "thread_turn_cancellation_applied") {
    const request = pending.get(message.client_id);
    if (request?.history === message.history_id && request.threadId === message.thread_id && request.turnId === message.turn_id) finish(message.client_id);
  } else if (message.type === "error" && message.client_id) {
    finish(message.client_id, message.detail);
  }
}
function cancelExact(history: string, action: Extract<EffectAction, {kind:"cancel_queued"|"stop"}>, expectedState: ThreadTurn["state"], sourceTurnId?: number): void {
  if (!transport || historyId() !== history) {
    if (sourceTurnId !== undefined) setEffectState(draft => { draft.errors[sourceTurnId] = "Reconnect before cancelling this turn."; });
    return;
  }
  const clientId = crypto.randomUUID();
  pending.set(clientId, { history, threadId: action.thread_id, turnId: action.turn_id, sourceTurnId, timer: setTimeout(() => finish(clientId, "Cancellation timed out. Reload and try again."), 20_000) });
  transport({ type: "cancel_thread_turn", client_id: clientId, history_id: history, thread_id: action.thread_id, turn_id: action.turn_id, expected_state: expectedState });
}
export function runEffectAction(action: EffectAction, sourceTurnId?: number): void {
  const history = historyId(); if (!history) return;
  if (action.kind === "open") {
    if (action.target.kind === "thread") focusThread(action.target.thread_id);
    else if (action.target.kind === "artifact") openArtifact(action.target.artifact_id);
  } else if (action.kind === "archive") {
    const thread = threadState.threads.find(candidate => candidate.id === action.thread_id);
    if (thread && !thread.archived_at) threadAction(history, thread.id, "archive", {}, thread.revision);
  } else cancelExact(history, action, action.kind === "stop" ? "running" : "queued", sourceTurnId);
}
export function reviewRefusedReach(sourceTurnId: number, targetThreadId?: number): void {
  const source = effectState.turns[sourceTurnId];
  const thread = source && threadState.threads.find(candidate => candidate.id === source.threadId);
  if (thread) openThreadReach(thread, targetThreadId);
}
