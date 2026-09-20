import { flush } from "solid-js";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { setHistoryId } from "../lib/history";
import type { EffectAction, ThreadClientMessage, ThreadEffect, ThreadEffectTarget } from "../threads/types";
import { attachEffectTransport, disconnectEffects, effectState, effectsForTurn, handleEffectMessage, mergeDetailEffects, resetEffects, runEffectAction } from "./store";

const history = "effect-history";
const effect = (id: number, turnId: number, target: ThreadEffectTarget = { kind: "thread", thread_id: 2 }, actions: EffectAction[] = []): ThreadEffect => ({
  receipt: { id, turn_id: turnId, operation_id: `op-${id}`, effect_index: 0, tool: "threads_read", effect: "read", target, target_turn_id: null, request_client_id: null, refusal: null, created_at: "2026-09-20T10:00:00Z" },
  actions,
});

const sent: ThreadClientMessage[] = [];
beforeEach(() => {
  sent.length = 0;
  vi.stubGlobal("crypto", { randomUUID: () => "cancel-client" });
  flush(() => { setHistoryId(history); resetEffects(); });
  attachEffectTransport(frame => sent.push(frame));
});
afterEach(() => { disconnectEffects(); vi.unstubAllGlobals(); });

describe("durable effect projection", () => {
  it("replaces live actions, clears an explicit empty turn and ignores another history", () => {
    const first = effect(1, 10, undefined, [{ kind: "cancel_queued", thread_id: 2, turn_id: 20 }]);
    flush(() => handleEffectMessage({ type: "thread_effects_changed", history_id: history, thread_id: 1, turn_id: 10, effects: [first] }));
    expect(effectsForTurn(10)).toEqual([first]);
    flush(() => handleEffectMessage({ type: "thread_effects_changed", history_id: history, thread_id: 1, turn_id: 10, effects: [] }));
    expect(effectsForTurn(10)).toEqual([]);
    flush(() => handleEffectMessage({ type: "thread_effects_changed", history_id: "other", thread_id: 1, turn_id: 10, effects: [first] }));
    expect(effectsForTurn(10)).toEqual([]);
  });

  it("merges reload and pagination snapshots without deleting unrelated live turns", () => {
    const live = effect(2, 11);
    const reloaded = effect(1, 10);
    flush(() => handleEffectMessage({ type: "thread_effects_changed", history_id: history, thread_id: 1, turn_id: 11, effects: [live] }));
    flush(() => mergeDetailEffects(1, [10], [reloaded]));
    expect(effectsForTurn(10)).toEqual([reloaded]);
    expect(effectsForTurn(11)).toEqual([live]);
    flush(() => resetEffects());
    expect(effectState.turns).toEqual({});
  });

  it("cancels only the exact projected turn and settles only its correlated acknowledgement", () => {
    const action = { kind: "stop" as const, thread_id: 2, turn_id: 20 };
    flush(() => runEffectAction(action));
    expect(sent).toEqual([{ type: "cancel_thread_turn", client_id: "cancel-client", history_id: history, thread_id: 2, turn_id: 20, expected_state: "running" }]);
    flush(() => handleEffectMessage({ type: "thread_turn_cancellation_applied", client_id: "other", history_id: history, thread_id: 2, turn_id: 20 }));
    flush(() => handleEffectMessage({ type: "thread_turn_cancellation_applied", client_id: "cancel-client", history_id: history, thread_id: 2, turn_id: 20 }));
    expect(effectState.error).toBeNull();
  });
});
