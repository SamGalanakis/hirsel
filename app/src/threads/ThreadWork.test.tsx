import { createSignal, flush } from "solid-js";
import { fireEvent, render } from "@solidjs/testing-library";
import { describe, expect, it } from "vitest";
import type { ChatMessage } from "../protocol";
import type { TimelineEvent } from "../store/types";
import type { ThreadActivity, ThreadTurn } from "./types";
import { ThreadWork } from "./ThreadWork";
import { setShowAgentCode } from "../lib/prefs";

const events: TimelineEvent[] = [
  { seq: 1, at: 1, event: { kind: "tool_done", id: "call-a", name: "read_file", ok: true, summary: "Distinct result: first file contents" } },
  { seq: 2, at: 2, event: { kind: "tool_done", id: "call-b", name: "read_file", ok: false, summary: "Distinct error: second file permission denied" } },
];
const activity: ThreadActivity = { artifact_ids: [], id: 1, thread_id: 1, turn_id: 1, kind: "tool_completed", data: { id: "call-a", name: "read_file", ok: true }, ts: "2026-09-09T10:00:00Z" };
const message: ChatMessage = { id: 1, thread_id: 1, author: "agent", body: "Finished", ref: null, ts: activity.ts, tool_calls: [{ id: "call-a", name: "read_file", ok: true }, { id: "call-b", name: "read_file", ok: false }] };
const turn = (state: ThreadTurn["state"]): ThreadTurn => ({ id: 1, thread_id: 1, requester_thread_id: null, requester_turn_id: null, owner_message_id: null, agent_message_id: state === "completed" ? 1 : null, state, started_at: "2026-09-09T09:59:00Z", finished_at: ["running", "queued"].includes(state) ? null : activity.ts });

describe("readable work outcomes", () => {
  it("shows failure and recovery in the inline stream", () => {
    const view = render(() => <ThreadWork turn={turn("failed")} events={[]} activities={[{ ...activity, kind: "execution_failed", data: { reason: "Browser checks failed: the page did not load." } }]} />);
    expect(view.getByText("Couldn’t finish")).toBeTruthy();
    const failure = view.container.querySelector('[data-slot="work-failure"]')!;
    expect(failure).toHaveTextContent("Browser checks failed: the page did not load.");
    expect(failure.closest("details")).toBeNull();
    const recovery = view.container.querySelector('[data-slot="work-recovery"]')!;
    expect(recovery).toHaveTextContent("Send a message to continue.");
    expect(failure.nextElementSibling).toBe(recovery);
    expect(view.container.querySelector("details")).toBeNull();
  });
  it("keeps plain replies free of an empty work disclosure", () => {
    const view = render(() => <ThreadWork turn={turn("completed")} message={{ ...message, tool_calls: [] }} events={[]} activities={[]} />);
    expect(view.container.querySelector('[data-slot="thread-work"]')).toBeNull();
  });
  it("distinguishes queued, active and stopped work without opening diagnostics", () => {
    const [current, setCurrent] = createSignal(turn("queued"));
    const view = render(() => <ThreadWork turn={current()} events={[{ seq: 1, event: { kind: "tool_start", id: "read", name: "read_file", summary: null } }]} activities={[]} />);
    expect(view.getByText("Queued")).toBeTruthy();
    flush(() => setCurrent(turn("running")));
    expect(view.getByText("Gathering context")).toBeTruthy();
    flush(() => setCurrent(turn("cancelled")));
    expect(view.getByText("Stopped")).toBeTruthy();
    expect(view.getByText(/Your conversation is kept/).closest("details")).toBeNull();
    expect(view.getByText("No result recorded")).toBeTruthy();
    expect(view.container.querySelector('[aria-label="running"]')).toBeNull();
  });
});

describe("execution result preservation", () => {
  it("settles a started tool from the recorded final outcome without duplicating the invocation", () => {
    const view = render(() => <ThreadWork turn={turn("completed")} message={{ ...message, tool_calls: [{ id: "read", name: "read_file", ok: true }] }} events={[{ seq: 1, event: { kind: "tool_start", id: "read", name: "read_file", summary: null } }]} activities={[]} />);
    expect(view.getByText("Activity")).toBeTruthy();
    expect(view.container.querySelector('[data-slot="work-details"]')).toBeNull();
    expect(view.container.querySelectorAll('[data-slot="timeline-tool"]')).toHaveLength(1);
    expect(view.container.querySelector('[data-slot="timeline-tool"] [aria-label="ok"]')).toBeTruthy();
    expect(view.getAllByText("read_file")).toHaveLength(1);
    expect(view.queryByText("No result recorded")).toBeNull();
  });

  it("retains ordered rich results and errors once after final summaries and late activity arrive", () => {
    const [final, setFinal] = createSignal<ChatMessage>();
    const [activities, setActivities] = createSignal<ThreadActivity[]>([]);
    const view = render(() => <ThreadWork message={final()} activities={activities()} events={events} />);
    for (const button of view.getAllByRole("button", { name: /read_file — show result/ })) fireEvent.click(button);
    expect(view.getByText("Distinct result: first file contents")).toBeTruthy();
    expect(view.getByText("Distinct error: second file permission denied")).toBeTruthy();
    flush(() => setFinal(message));
    flush(() => setActivities([activity]));
    expect(view.getAllByText("Distinct result: first file contents")).toHaveLength(1);
    expect(view.getAllByText("Distinct error: second file permission denied")).toHaveLength(1);
    const results = [...view.container.querySelectorAll('[data-slot="tool-result"]')];
    expect(results.map(row => row.textContent)).toEqual(["Distinct result: first file contents", "Distinct error: second file permission denied"]);
    expect(view.getAllByText("read_file")).toHaveLength(2);
    expect(view.container.querySelectorAll('[aria-label="failed"]')).toHaveLength(1);
    expect(view.container.querySelectorAll('[data-slot="timeline-tool"]')).toHaveLength(2);
    expect(view.container.textContent).not.toContain("tool completed");
    expect(view.queryByRole("button", { name: "4 tool calls" })).toBeNull();
  });

  it("pairs reverse-order same-name durable completions by canonical ID after reconnect", () => {
    const starts: TimelineEvent[] = [
      { seq: 1, event: { kind: "tool_start", id: "call-a", name: "read_file", summary: "Reading A" } },
      { seq: 2, event: { kind: "tool_start", id: "call-b", name: "read_file", summary: "Reading B" } },
    ];
    const reverseCompletionOrder: ChatMessage = {
      ...message,
      tool_calls: [
        { id: "call-b", name: "read_file", ok: false },
        { id: "call-a", name: "read_file", ok: true },
      ],
    };
    const view = render(() => <ThreadWork turn={turn("completed")} message={reverseCompletionOrder} events={starts} activities={[]} />);
    const rows = [...view.container.querySelectorAll('[data-slot="timeline-tool"]')];
    expect(rows).toHaveLength(2);
    expect(rows.map(row => [row.getAttribute("data-tool-call-id"), Boolean(row.querySelector('[aria-label="ok"]')), Boolean(row.querySelector('[aria-label="failed"]'))])).toEqual([
      ["call-a", true, false],
      ["call-b", false, true],
    ]);
  });

  it("joins a partial rich completion with durable outcomes by ID without duplicates", () => {
    const partial: TimelineEvent[] = [
      { seq: 1, event: { kind: "tool_start", id: "call-a", name: "read_file", summary: "Reading A" } },
      { seq: 2, event: { kind: "tool_start", id: "call-b", name: "read_file", summary: "Reading B" } },
      { seq: 3, event: { kind: "tool_done", id: "call-b", name: "read_file", ok: false, summary: "B failed" } },
    ];
    const view = render(() => <ThreadWork turn={turn("completed")} message={message} events={partial} activities={[]} />);
    const rows = [...view.container.querySelectorAll('[data-slot="timeline-tool"]')];
    expect(rows).toHaveLength(2);
    expect(rows.map(row => row.getAttribute("data-tool-call-id"))).toEqual(["call-a", "call-b"]);
    expect(view.getByText("B failed")).toBeTruthy();
    expect(view.getAllByText("read_file")).toHaveLength(2);
  });

  it("keeps an exact durable tool outcome on a turn with no final message", () => {
    const view = render(() => <ThreadWork turn={turn("interrupted")} activities={[activity]} events={[{ seq: 1, event: { kind: "tool_start", id: "call-a", name: "read_file", summary: null } }]} />);
    const row = view.container.querySelector('[data-slot="timeline-tool"]')!;
    expect(row.getAttribute("data-tool-call-id")).toBe("call-a");
    expect(row.querySelector('[aria-label="ok"]')).toBeTruthy();
    expect(view.getAllByText("read_file")).toHaveLength(1);
  });

  it("keeps both completion payloads when only the first persisted activity has arrived", () => {
    const view = render(() => <ThreadWork activities={[activity]} events={events} live />);
    for (const button of view.getAllByRole("button", { name: /read_file — show result/ })) fireEvent.click(button);
    expect(view.getAllByText("Distinct result: first file contents")).toHaveLength(1);
    expect(view.getAllByText("Distinct error: second file permission denied")).toHaveLength(1);
    expect([...view.container.querySelectorAll('[data-slot="tool-result"]')].map(row => row.textContent)).toEqual(["Distinct result: first file contents", "Distinct error: second file permission denied"]);
    expect(view.queryByRole("button", { name: /tool calls/ })).toBeNull();
  });
});


it("preserves overlapping call pairing and reasoning/code positions through reversed completions and persistence", () => {
  const initial: TimelineEvent[] = [
    { seq: 1, event: { kind: "tool_start", id: "call-a", name: "read_file", summary: null } },
    { seq: 2, event: { kind: "reasoning", text: "Between invocations" } },
    { seq: 3, event: { kind: "code_start", id: "cell", language: "python", code: "print(42)", truncated: false } },
    { seq: 4, event: { kind: "tool_start", id: "call-b", name: "read_file", summary: null } },
    { seq: 5, event: { kind: "reasoning", text: "After second invocation" } },
  ];
  const [stream, setStream] = createSignal(initial);
  const [final, setFinal] = createSignal<ChatMessage>();
  const [activities, setActivities] = createSignal<ThreadActivity[]>([]);
  flush(() => setShowAgentCode(true));
  try {
    const view = render(() => <ThreadWork message={final()} activities={activities()} events={stream()} />);
    flush(() => setStream([...initial,
      { ...events[1], seq: 6 },
      { seq: 7, event: { kind: "code_done", id: "cell", ok: true, summary: "42" } },
      { ...events[0], seq: 8 },
    ]));
    const slots = () => [...view.container.querySelectorAll('[data-slot="timeline"] > li')].map(row => {
      if (row.textContent?.includes("Distinct result:")) return "a/result";
      if (row.textContent?.includes("Distinct error:")) return "b/error";
      return row.getAttribute("data-slot");
    });
    const expected = ["a/result", "timeline-reasoning", "timeline-code", "b/error", "timeline-reasoning"];
    expect(slots()).toEqual(expected);
    // Only B's activity has been persisted; neither completion may move.
    flush(() => setActivities([{ ...activity, data: { id: "call-b", name: "read_file", ok: false } }]));
    expect(slots()).toEqual(expected);
    flush(() => setFinal(message));
    expect(slots()).toEqual(expected);
    flush(() => setActivities([{ ...activity, data: { id: "call-b", name: "read_file", ok: false } }, { ...activity, id: 2 }]));
    expect(slots()).toEqual(expected);
    for (const button of view.getAllByRole("button", { name: /read_file — show result/ })) fireEvent.click(button);
    const results = [...view.container.querySelectorAll('[data-slot="tool-result"]')];
    expect(results.map(row => [row.getAttribute("data-tool-call-id"), row.textContent])).toEqual([
      ["call-a", "Distinct result: first file contents"],
      ["call-b", "Distinct error: second file permission denied"],
    ]);
    expect(view.getAllByText("Distinct result: first file contents")).toHaveLength(1);
    expect(view.getAllByText("Distinct error: second file permission denied")).toHaveLength(1);
    expect(view.getAllByText("read_file")).toHaveLength(2);
    expect(view.container.querySelectorAll('[aria-label="failed"]')).toHaveLength(1);
    expect(view.container.querySelectorAll('[data-slot="timeline-tool"]')).toHaveLength(2);
    expect(view.queryByText(/tool completed/)).toBeNull();
  } finally { flush(() => setShowAgentCode(false)); }
});
