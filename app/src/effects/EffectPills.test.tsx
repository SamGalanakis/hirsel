import { cleanup, render, screen } from "@solidjs/testing-library";
import { flush } from "solid-js";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { closeThreadReach } from "../grants/reach";
import { makeThread } from "../threads/fixtures";
import { setThreadState } from "../threads/store";
import type { ThreadActivity } from "../threads/types";
import type { TimelineEvent } from "../store/types";
import { EffectPills } from "./EffectPills";

const payload = (value: unknown, truncated = false) => ({ text: JSON.stringify(value), truncated });
const call = (id: string, name: string, input: unknown, result: unknown, truncated = false): TimelineEvent[] => [
  { seq: 1, event: { kind: "tool_start", id, name, summary: null, input: payload(input) } },
  { seq: 2, event: { kind: "tool_done", id, name, ok: true, summary: null, result: payload(result, truncated) } },
];
const refusal = (target: Record<string, unknown>, reason = "outside_grant"): ThreadActivity => ({ id: 9, thread_id: 1, turn_id: 10, kind: "refusal", data: { reason, target }, artifact_ids: [], ts: "2026-09-20T10:00:00Z" });

beforeEach(() => flush(() => setThreadState(draft => { draft.threads = [makeThread(1, { title: "Operations", kind: "space" }), makeThread(2, { title: "Billing", kind: "task" })]; })));
afterEach(() => { cleanup(); closeThreadReach(); });

describe("durable-data effect projection", () => {
  it("derives the supported verbs only from complete paired tool payloads", () => {
    const events = [
      ...call("create", "threads_create", { parent: 1 }, { thread_id: 2 }),
      ...call("artifact", "artifacts_create", { title: "Plan" }, { id: 44 }),
      ...call("send", "threads_send", { thread: 2 }, { thread_id: 2, turn_id: 20 }),
      ...call("read", "threads_read", { thread: 2 }, { thread_id: 2 }),
    ].map((event, index) => ({ ...event, seq: index + 1 }));
    render(() => <EffectPills turnId={10} threadId={1} activities={[]} events={events} />);
    expect(screen.getByText("Created · Billing")).toBeInTheDocument();
    expect(screen.getByText("Created · Artifact 44")).toBeInTheDocument();
    expect(screen.getByText("Sent to · Billing")).toBeInTheDocument();
    expect(screen.getByText("Read · Billing")).toBeInTheDocument();
    expect(screen.getAllByRole("button", { name: "Open" })).toHaveLength(4);
  });

  it("shows no pill when either payload is truncated, malformed or unsuccessful", () => {
    const malformed: TimelineEvent[] = [
      { seq: 1, event: { kind: "tool_start", id: "bad", name: "threads_read", summary: null, input: { text: "{", truncated: false } } },
      { seq: 2, event: { kind: "tool_done", id: "bad", name: "threads_read", ok: true, summary: null, result: payload({ thread_id: 2 }) } },
      ...call("truncated", "threads_read", { thread: 2 }, { thread_id: 2 }, true).map((event, index) => ({ ...event, seq: index + 3 })),
      { seq: 5, event: { kind: "tool_start", id: "failed", name: "threads_read", summary: null, input: payload({ thread: 2 }) } },
      { seq: 6, event: { kind: "tool_done", id: "failed", name: "threads_read", ok: false, summary: null, result: payload({ thread_id: 2 }) } },
    ];
    const view = render(() => <EffectPills turnId={10} threadId={1} activities={[]} events={malformed} />);
    expect(view.container.querySelector('[data-slot="effect-pills"]')).toBeNull();
  });

  it("explains a grantable refusal without retrying it", () => {
    render(() => <EffectPills turnId={10} threadId={1} activities={[refusal({ kind: "thread", thread_id: 2 })]} events={[]} />);
    expect(screen.getByText("Refused · Billing")).toBeInTheDocument();
    expect(screen.getByText(/this Thread and its subtree/i)).toBeInTheDocument();
    expect(screen.getByText(/never retries/i)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Review reach" })).toBeInTheDocument();
  });

  it("offers no reach action for owner fences or artifact refusals", () => {
    const view = render(() => <EffectPills turnId={10} threadId={1} activities={[refusal({ kind: "thread", thread_id: 2 }, "owner_fence"), { ...refusal({ kind: "artifact", artifact_id: 44 }), id: 10 }]} events={[]} />);
    expect(view.getByText(/ancestor fence/i)).toBeInTheDocument();
    expect(view.getByText(/no owning Space to guess/i)).toBeInTheDocument();
    expect(view.queryByRole("button", { name: "Review reach" })).toBeNull();
  });
});
