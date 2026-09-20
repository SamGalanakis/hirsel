import { cleanup, fireEvent, render, screen } from "@solidjs/testing-library";
import { flush } from "solid-js";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { ReachDialog } from "../grants/ReachDialog";
import { closeThreadReach } from "../grants/reach";
import { resetGrants } from "../grants/store";
import { setHistoryId } from "../lib/history";
import { makeThread } from "../threads/fixtures";
import { setThreadState } from "../threads/store";
import { ThreadMessage } from "../threads/ThreadMessages";
import type { ThreadEffect, ThreadEffectTarget, ThreadTurn } from "../threads/types";
import { EffectPills } from "./EffectPills";
import { handleEffectMessage, resetEffects } from "./store";

const history = "effect-history";
const turn = (state: ThreadTurn["state"] = "completed"): ThreadTurn => ({ id: 10, thread_id: 1, requester_thread_id: null, requester_turn_id: null, owner_message_id: null, agent_message_id: null, state, accepted_at: "2026-09-20T10:00:00Z", started_at: "2026-09-20T10:00:00Z", finished_at: state === "running" ? null : "2026-09-20T10:00:01Z" });
const refused = (target: ThreadEffectTarget, reason = "outside_grant"): ThreadEffect => ({
  receipt: { id: 1, turn_id: 10, operation_id: "refusal", effect_index: 0, tool: "threads_read", effect: "refused", target, target_turn_id: null, request_client_id: null, refusal: { reason, grant_summary: "self + subtree", detail: "outside reach" }, created_at: "2026-09-20T10:00:00Z" },
  actions: [],
});
const receive = (effects: ThreadEffect[]) => flush(() => handleEffectMessage({ type: "thread_effects_changed", history_id: history, thread_id: 1, turn_id: 10, effects }));

beforeEach(() => flush(() => {
  setHistoryId(history); resetEffects(); resetGrants(); closeThreadReach();
  setThreadState(draft => { draft.ready = true; draft.threads = [makeThread(1, { title: "Operations", kind: "space" }), makeThread(2, { title: "Billing", kind: "task", parent_thread_id: null })]; });
}));
afterEach(() => { cleanup(); closeThreadReach(); });

describe("effect pills", () => {
  it("renders a live effect and a completed effect-only reply outside the trace", () => {
    receive([{ ...refused({ kind: "thread", thread_id: 2 }), receipt: { ...refused({ kind: "thread", thread_id: 2 }).receipt, effect: "read", refusal: null } }]);
    const current = turn("running");
    const view = render(() => <ThreadMessage entry={{ key: "turn-10", kind: "turn", turn: current }} threadId={1} history={{ brief: { text: "", artifact_ids: [] }, messages: [], turns: [current], activities: [], hasMore: false, loaded: true }} />);
    expect(view.queryByText("Read · Billing")).toBeInTheDocument();
    expect(view.container.querySelector('[data-slot="turn-pending"]')).toBeNull();
    const pills = view.container.querySelector('[data-slot="effect-pills"]')!;
    expect(pills.closest('[data-slot="run-card-trace"]')).toBeNull();

    cleanup();
    const completed = turn();
    render(() => <ThreadMessage entry={{ key: "turn-10", kind: "turn", turn: completed }} threadId={1} history={{ brief: { text: "", artifact_ids: [] }, messages: [], turns: [completed], activities: [], hasMore: false, loaded: true }} />);
    expect(screen.getByText("Read · Billing")).toBeInTheDocument();
  });

  it("offers the exact grantable Thread subtree through Reach without auto-retry", () => {
    receive([refused({ kind: "thread", thread_id: 2 })]);
    render(() => <><EffectPills turnId={10} /><ReachDialog /></>);
    expect(screen.getByText(/this Thread and everything below it/i)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Review reach" }));
    const dialog = screen.getByRole("dialog", { name: /Reach of Space #1/ });
    expect(dialog.querySelector<HTMLInputElement>('[aria-label="Add reach to another Thread"]')?.value).toBe("#2");
    expect(screen.getByRole("button", { name: /Billing/ })).toBeInTheDocument();
  });

  it("explains owner_fence without offering an ineffective grant", () => {
    receive([refused({ kind: "thread", thread_id: 2 }, "owner_fence")]);
    render(() => <EffectPills turnId={10} />);
    expect(screen.getByText(/ancestor fence/i)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Review reach" })).toBeNull();
  });

  it("does not guess a Space for an artifact refusal", () => {
    receive([refused({ kind: "artifact", artifact_id: 44 })]);
    render(() => <EffectPills turnId={10} />);
    expect(screen.getByText(/no owning Space to guess/i)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Review reach" })).toBeNull();
  });
});
