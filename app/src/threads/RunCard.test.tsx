import { createSignal, flush } from "solid-js";
import { fireEvent, render } from "@solidjs/testing-library";
import { beforeEach, describe, expect, it } from "vitest";
import type { ChatMessage } from "../protocol";
import type { TimelineEvent } from "../store/types";
import type { ThreadActivity, ThreadTurn } from "./types";
import { RunCard } from "./RunCard";
import { ActivityEntry } from "./ThreadWork";
import { setThreadState, setTurnExpanded } from "./store";

const events: TimelineEvent[] = [
  { seq: 1, at: 1, event: { kind: "tool_done", id: "call-a", name: "read_file", ok: true, summary: "Distinct result: first file contents", result: null } },
  { seq: 2, at: 2, event: { kind: "tool_done", id: "call-b", name: "read_file", ok: false, summary: "Distinct error: second file permission denied", result: null } },
];
const activity: ThreadActivity = { artifact_ids: [], id: 1, thread_id: 1, turn_id: 1, kind: "tool_completed", data: { id: "call-a", name: "read_file", ok: true }, ts: "2026-09-09T10:00:00Z" };
const message: ChatMessage = { id: 1, thread_id: 1, author: "agent", body: "Finished", ref: null, ts: activity.ts, tool_calls: [{ id: "call-a", name: "read_file", ok: true }, { id: "call-b", name: "read_file", ok: false }] };
const turn = (state: ThreadTurn["state"]): ThreadTurn => ({ id: 1, thread_id: 1, requester_thread_id: null, requester_turn_id: null, owner_message_id: 5, agent_message_id: state === "completed" ? 1 : null, state, accepted_at: "2026-09-09T09:59:00Z", started_at: state === "queued" ? null : "2026-09-09T09:59:00Z", finished_at: ["running", "queued"].includes(state) ? null : activity.ts });
/** The Owner's session-lived open/close choices must not leak between runs. */
beforeEach(() => { flush(() => setThreadState(draft => { draft.expandedTurns = {}; })); });

describe("the run card", () => {
  it("names the run, keeps a finished trace closed, and opens it on the header", () => {
    const view = render(() => <RunCard turn={turn("completed")} message={message} events={events} activities={[]} />);
    const header = view.getByRole("button", { name: /Owner message/ });
    expect(header).toHaveAttribute("aria-expanded", "false");
    expect(header).toHaveTextContent("Done");
    expect(view.container.querySelector('[data-slot="timeline"]')).toBeNull();
    expect(view.getByText("Finished")).toBeInTheDocument();
    fireEvent.click(header);
    expect(header).toHaveAttribute("aria-expanded", "true");
    const trace = view.container.querySelector('[data-slot="run-card-trace"]')!;
    expect(header.getAttribute("aria-controls")).toBe(trace.id);
    expect(trace.querySelectorAll('[data-slot="timeline-tool"]')).toHaveLength(2);
    // Enter and Space reach a real button without a keyboard handler of our own.
    expect(header.tagName).toBe("BUTTON");
    fireEvent.click(header);
    expect(view.container.querySelector('[data-slot="run-card-trace"]')).toBeNull();
  });
  it("opens a running run by default and keeps the Owner's close through the turn", () => {
    const [current, setCurrent] = createSignal(turn("running"));
    const view = render(() => <RunCard turn={current()} events={events} activities={[]} live />);
    expect(view.getByRole("button", { name: /Running/ })).toHaveAttribute("aria-expanded", "true");
    expect(view.container.querySelectorAll('[data-slot="timeline-tool"]')).toHaveLength(2);
    fireEvent.click(view.getByRole("button", { name: /Running/ }));
    expect(view.container.querySelector('[data-slot="run-card-trace"]')).toBeNull();
    flush(() => setCurrent(turn("completed")));
    expect(view.getByRole("button", { name: /Owner message/ })).toHaveAttribute("aria-expanded", "false");
  });
  it("names a process wake and a delegation as the run's origin", () => {
    const wake: ThreadActivity = { ...activity, id: 2, kind: "process_completed", data: { name: "morningReview", summary: "" } };
    const process = render(() => <RunCard turn={{ ...turn("completed"), owner_message_id: null }} message={message} events={[]} activities={[wake]} />);
    expect(process.getByRole("button", { name: /Process wake · morningReview/ })).toBeInTheDocument();
    const trigger: ChatMessage = { id: 9, thread_id: 1, author: "agent", body: "wake", ref: null, ts: activity.ts, origin: { kind: "process", process_id: "p1", name: "nightly", trigger: { kind: "cron", expr: "0 9 * * *" }, outcome: "completed", result: null } };
    const triggered = render(() => <RunCard turn={{ ...turn("completed"), owner_message_id: 9 }} trigger={trigger} message={message} events={[]} activities={[]} />);
    expect(triggered.getByRole("button", { name: /Process wake · nightly/ })).toBeInTheDocument();
    const background = render(() => <RunCard turn={{ ...turn("completed"), owner_message_id: null }} message={message} events={[]} activities={[]} />);
    expect(background.getByRole("button", { name: /Background run/ })).toBeInTheDocument();
    const delegated = render(() => <RunCard turn={{ ...turn("completed"), requester_thread_id: 7 }} message={message} events={[]} activities={[]} />);
    expect(delegated.getByRole("button", { name: /Delegation report · #7/ })).toBeInTheDocument();
  });
  it("shows each artifact the run produced once, whatever repeats it", () => {
    const published: ChatMessage = { ...message, artifact_ids: [44, 44, 45] };
    const view = render(() => <RunCard turn={turn("completed")} message={published} events={[]} activities={[{ ...activity, id: 3, kind: "artifact_saved", artifact_ids: [44] }]} />);
    expect(view.container.querySelectorAll('[data-artifact-ref="44"]')).toHaveLength(1);
    expect(view.container.querySelectorAll('[data-artifact-ref="45"]')).toHaveLength(1);
  });
  it("shows failure and recovery beside the reply, not behind the disclosure", () => {
    const view = render(() => <RunCard turn={turn("failed")} events={[]} activities={[{ ...activity, kind: "execution_failed", data: { reason: "Browser checks failed: the page did not load." } }]} />);
    expect(view.getByRole("button", { name: /Failed/ })).toBeInTheDocument();
    const failure = view.container.querySelector('[data-slot="work-failure"]')!;
    expect(failure).toHaveTextContent("Browser checks failed: the page did not load.");
    expect(failure.closest("details")).toBeNull();
    const recovery = view.container.querySelector('[data-slot="work-recovery"]')!;
    expect(recovery).toHaveTextContent("Send a message to continue.");
    expect(failure.nextElementSibling).toBe(recovery);
    expect(view.container.querySelector("details")).toBeNull();
  });
  it("keeps a plain reply's trace closed and its reply plain", () => {
    const view = render(() => <RunCard turn={turn("completed")} message={{ ...message, tool_calls: [] }} events={[]} activities={[]} />);
    expect(view.getByRole("button", { name: /Done/ })).toHaveAttribute("aria-expanded", "false");
    expect(view.container.querySelector('[data-slot="timeline"]')).toBeNull();
    expect(view.getByText("Finished")).toBeInTheDocument();
  });
  it("distinguishes queued, active and stopped runs in the header", () => {
    const [current, setCurrent] = createSignal(turn("queued"));
    const events: TimelineEvent[] = [{ seq: 1, event: { kind: "tool_start", id: "read", name: "read_file", summary: null, input: null } }];
    const view = render(() => <RunCard turn={current()} events={events} activities={[]} />);
    expect(view.getByRole("button", { name: /Queued/ })).toBeInTheDocument();
    flush(() => setCurrent(turn("running")));
    expect(view.getByRole("button", { name: /Running/ })).toBeInTheDocument();
    // What the run is doing right now stays available to assistive technology.
    expect(view.getByText("Gathering context")).toBeInTheDocument();
    flush(() => setCurrent(turn("cancelled")));
    expect(view.getByRole("button", { name: /Cancelled/ })).toBeInTheDocument();
    expect(view.getByText(/Your conversation is kept/)).toBeInTheDocument();
    flush(() => setTurnExpanded(1, true));
    expect(view.getByText("No result recorded")).toBeInTheDocument();
    expect(view.container.querySelector('[data-slot="timeline"] [aria-label="running"]')).toBeNull();
  });
  it("keeps the run's identity and raw events behind Technical details", () => {
    const view = render(() => <RunCard turn={turn("completed")} message={message} events={events} activities={[]} />);
    flush(() => setTurnExpanded(1, true));
    const technical = view.container.querySelector('[data-slot="run-card-technical"]')!;
    expect(technical.querySelector('[role="region"]')).toHaveAttribute("data-slot", "work-diagnostics");
    expect(technical.textContent).toContain("Turn 1 · completed");
    expect(technical.textContent).toContain("call-a");
  });
});

describe("execution result preservation", () => {
  const open = () => flush(() => setTurnExpanded(1, true));
  it("settles a started tool from the recorded final outcome without duplicating the invocation", () => {
    const view = render(() => <RunCard turn={turn("completed")} message={{ ...message, tool_calls: [{ id: "read", name: "read_file", ok: true }] }} events={[{ seq: 1, event: { kind: "tool_start", id: "read", name: "read_file", summary: null, input: null } }]} activities={[]} />);
    open();
    expect(view.container.querySelectorAll('[data-slot="timeline-tool"]')).toHaveLength(1);
    expect(view.container.querySelector('[data-slot="timeline-tool"] [aria-label="ok"]')).toBeTruthy();
    expect(view.getAllByText("read_file")).toHaveLength(1);
    expect(view.queryByText("No result recorded")).toBeNull();
  });

  it("retains ordered rich results and errors once after final summaries and late activity arrive", () => {
    const [final, setFinal] = createSignal<ChatMessage>();
    const [activities, setActivities] = createSignal<ThreadActivity[]>([]);
    const view = render(() => <RunCard message={final()} activities={activities()} events={events} />);
    // One panel is open at a time, so each pill is asked for its own payload.
    const openPill = (index: number) => {
      fireEvent.click(view.getAllByRole("button", { name: /read_file — (?:show|hide) result/ })[index]);
      const panels = [...view.container.querySelectorAll('[data-slot="tool-result"]')];
      expect(panels).toHaveLength(1);
      return [panels[0].getAttribute("data-tool-call-id"), panels[0].textContent];
    };
    expect(openPill(0)).toEqual(["call-a", "Result\nDistinct result: first file contents"]);
    expect(openPill(1)).toEqual(["call-b", "Result\nDistinct error: second file permission denied"]);
    flush(() => setFinal(message));
    flush(() => setActivities([activity]));
    const results = [...view.container.querySelectorAll('[data-slot="tool-result"]')];
    expect(results.map(row => [row.getAttribute("data-tool-call-id"), row.textContent])).toEqual([["call-b", "Result\nDistinct error: second file permission denied"]]);
    expect(openPill(0)).toEqual(["call-a", "Result\nDistinct result: first file contents"]);
    expect(view.getAllByText("read_file")).toHaveLength(2);
    expect(view.container.querySelectorAll('[aria-label="failed"]')).toHaveLength(1);
    expect(view.container.querySelectorAll('[data-slot="timeline-tool"]')).toHaveLength(2);
    expect(view.container.textContent).not.toContain("tool completed");
  });

  it("pairs reverse-order same-name durable completions by canonical ID after reconnect", () => {
    const starts: TimelineEvent[] = [
      { seq: 1, event: { kind: "tool_start", id: "call-a", name: "read_file", summary: "Reading A", input: null } },
      { seq: 2, event: { kind: "tool_start", id: "call-b", name: "read_file", summary: "Reading B", input: null } },
    ];
    const reverseCompletionOrder: ChatMessage = {
      ...message,
      tool_calls: [
        { id: "call-b", name: "read_file", ok: false },
        { id: "call-a", name: "read_file", ok: true },
      ],
    };
    const view = render(() => <RunCard turn={turn("completed")} message={reverseCompletionOrder} events={starts} activities={[]} />);
    open();
    const rows = [...view.container.querySelectorAll('[data-slot="timeline-tool"]')];
    expect(rows).toHaveLength(2);
    expect(rows.map(row => [row.getAttribute("data-tool-call-id"), Boolean(row.querySelector('[aria-label="ok"]')), Boolean(row.querySelector('[aria-label="failed"]'))])).toEqual([
      ["call-a", true, false],
      ["call-b", false, true],
    ]);
  });

  it("joins a partial rich completion with durable outcomes by ID without duplicates", () => {
    const partial: TimelineEvent[] = [
      { seq: 1, event: { kind: "tool_start", id: "call-a", name: "read_file", summary: "Reading A", input: null } },
      { seq: 2, event: { kind: "tool_start", id: "call-b", name: "read_file", summary: "Reading B", input: null } },
      { seq: 3, event: { kind: "tool_done", id: "call-b", name: "read_file", ok: false, summary: "B failed", result: null } },
    ];
    const view = render(() => <RunCard turn={turn("completed")} message={message} events={partial} activities={[]} />);
    open();
    const rows = [...view.container.querySelectorAll('[data-slot="timeline-tool"]')];
    expect(rows).toHaveLength(2);
    expect(rows.map(row => row.getAttribute("data-tool-call-id"))).toEqual(["call-a", "call-b"]);
    expect(view.getByText("Reading B · Failed")).toBeTruthy();
    expect(view.getAllByText("read_file")).toHaveLength(2);
  });

  it("keeps an exact durable tool outcome on a turn with no final message", () => {
    const view = render(() => <RunCard turn={turn("interrupted")} activities={[activity]} events={[{ seq: 1, event: { kind: "tool_start", id: "call-a", name: "read_file", summary: null, input: null } }]} />);
    open();
    const row = view.container.querySelector('[data-slot="timeline-tool"]')!;
    expect(row.getAttribute("data-tool-call-id")).toBe("call-a");
    expect(row.querySelector('[aria-label="ok"]')).toBeTruthy();
    expect(view.getAllByText("read_file")).toHaveLength(1);
  });

  it("keeps both completion payloads when only the first persisted activity has arrived", () => {
    const view = render(() => <RunCard activities={[activity]} events={events} live />);
    const payloads = view.getAllByRole("button", { name: /read_file — show result/ }).map((_, index) => {
      fireEvent.click(view.getAllByRole("button", { name: /read_file — (?:show|hide) result/ })[index]);
      const panels = [...view.container.querySelectorAll('[data-slot="tool-result"]')];
      expect(panels).toHaveLength(1);
      return panels[0].textContent;
    });
    expect(payloads).toEqual(["Result\nDistinct result: first file contents", "Result\nDistinct error: second file permission denied"]);
  });
});


it("preserves overlapping call pairing and reasoning/code positions through reversed completions and persistence", () => {
  const initial: TimelineEvent[] = [
    { seq: 1, event: { kind: "tool_start", id: "call-a", name: "read_file", summary: null, input: null } },
    { seq: 2, event: { kind: "reasoning", text: "Between invocations" } },
    { seq: 3, event: { kind: "code_start", id: "cell", language: "python", code: "print(42)", truncated: false } },
    { seq: 4, event: { kind: "tool_start", id: "call-b", name: "read_file", summary: null, input: null } },
    { seq: 5, event: { kind: "reasoning", text: "After second invocation" } },
  ];
  const [stream, setStream] = createSignal(initial);
  const [final, setFinal] = createSignal<ChatMessage>();
  const [activities, setActivities] = createSignal<ThreadActivity[]>([]);
  {
    const view = render(() => <RunCard message={final()} activities={activities()} events={stream()} />);
    flush(() => setStream([...initial,
      { ...events[1], seq: 6 },
      { seq: 7, event: { kind: "code_done", id: "cell", ok: true, summary: "42" } },
      { ...events[0], seq: 8 },
    ]));
    const slots = () => [...view.container.querySelectorAll('[data-slot="timeline"] > li')].map(row => {
      if (row.getAttribute("data-slot") !== "timeline-tool") return row.getAttribute("data-slot");
      if (row.textContent?.includes("Distinct result:")) return "a/result";
      if (row.textContent?.includes("Distinct error:")) return "b/error";
      return row.getAttribute("data-slot");
    });
    // The cell and the tool that ran inside it are peers: one flat sequence in
    // arrival order, every row keeping its position.
    const expected = ["a/result", "timeline-reasoning", "timeline-code", "b/error", "timeline-reasoning"];
    expect(slots()).toEqual(expected);
    // Only B's activity has been persisted; neither completion may move.
    flush(() => setActivities([{ ...activity, data: { id: "call-b", name: "read_file", ok: false } }]));
    expect(slots()).toEqual(expected);
    flush(() => setFinal(message));
    expect(slots()).toEqual(expected);
    flush(() => setActivities([{ ...activity, data: { id: "call-b", name: "read_file", ok: false } }, { ...activity, id: 2 }]));
    expect(slots()).toEqual(expected);
    const opened = (index: number) => {
      fireEvent.click(view.getAllByRole("button", { name: /read_file — (?:show|hide) result/ })[index]);
      const panels = [...view.container.querySelectorAll('[data-slot="tool-result"]')];
      expect(panels).toHaveLength(1);
      return [panels[0].getAttribute("data-tool-call-id"), panels[0].textContent];
    };
    expect(opened(0)).toEqual(["call-a", "Result\nDistinct result: first file contents"]);
    expect(opened(1)).toEqual(["call-b", "Result\nDistinct error: second file permission denied"]);
    expect(view.getAllByText("read_file")).toHaveLength(2);
    expect(view.container.querySelectorAll('[aria-label="failed"]')).toHaveLength(1);
    expect(view.container.querySelectorAll('[data-slot="timeline-tool"]')).toHaveLength(2);
    // b ran inside the cell but reads beside it, and its panel is a sibling of
    // the pills rather than a branch of the Code entry.
    const cell = view.container.querySelector('[data-slot="timeline-code"]') as HTMLElement;
    expect(cell.querySelector('[data-slot="tool-result"]')).toBeNull();
    expect(view.container.querySelector('[data-slot="timeline"] > [data-slot="timeline-detail"] [data-tool-call-id="call-b"]')).toBeTruthy();
    expect(view.queryByText(/tool completed/)).toBeNull();
  }
});

it("renders a refusal as one quiet centred note, not a third speaker", () => {
  const refusal: ThreadActivity = { artifact_ids: [], id: 4, thread_id: 1, turn_id: 1, kind: "refusal", data: { refused: true, reason: "outside_grant", tool: "threads_read", target: { kind: "thread", thread_id: 51 }, grant_summary: "self + subtree" }, ts: activity.ts };
  const view = render(() => <ActivityEntry activity={refusal} />);
  const note = view.container.querySelector('[data-slot="conversation-note"]')!;
  expect(note).toHaveTextContent("Refused: threads.read Thread 51 — outside grant");
});
