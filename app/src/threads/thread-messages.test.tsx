import { render } from "@solidjs/testing-library";
import { flush } from "solid-js";
import { describe, expect, it } from "vitest";
import { ThreadMessage } from "./ThreadMessages";
import { ActivityEntry } from "./ThreadWork";
import { emptyHistory } from "./model";
import { setThreadState } from "./store";
import { dispatch } from "../store/store";
import type { ChatMessage } from "../protocol";
import type { ThreadActivity, ThreadTurn } from "./types";
const message = (author: ChatMessage["author"]): ChatMessage => ({ id: 1, thread_id: 1, author, body: "Ready when you are", ref: null, ts: "2026-09-09T10:00:00Z", tool_calls: [] });
const row = (author: ChatMessage["author"]) => render(() => <ThreadMessage entry={{ key: `message-1`, kind: "message", message: message(author) }} history={emptyHistory()} threadId={1} />).container.querySelector<HTMLElement>("article")!;
describe("who is speaking", () => {
  it("seats the Owner right in the filled emphasis pair at conversational width", () => {
    const article = row("owner");
    expect(article).toHaveAttribute("data-author", "owner");
    expect(article.className).toContain("flex-row-reverse");
    const bubble = article.querySelector<HTMLElement>('[data-slot="owner-message"]')!;
    expect(bubble.className).toContain("bg-primary");
    expect(bubble.className).toContain("text-primary-foreground");
    expect(bubble.className).toContain("sm:max-w-[60%]");
  });
  it("seats the Agent left on a neutral surface that hugs its own content", () => {
    const article = row("agent");
    expect(article).toHaveAttribute("data-author", "agent");
    expect(article.className).not.toContain("flex-row-reverse");
    const bubble = article.querySelector<HTMLElement>('[data-slot="agent-message"]')!;
    expect(bubble.className).toContain("bg-surface");
    expect(bubble.className).not.toContain("flex-1");
    expect(bubble.className).toContain("max-w-[96%]");
    expect(bubble.className).toContain("sm:max-w-[80%]");
    expect(bubble.className).not.toContain("max-w-[60%]");
  });
  it("gives neither side an avatar gutter: alignment already says who is speaking", () => {
    for (const author of ["owner", "agent"] as const) {
      const article = row(author);
      expect(article.querySelector('[data-slot="message-avatar"]')).toBeNull();
      expect(article.querySelector("svg")).toBeNull();
      expect(article.children).toHaveLength(1);
    }
  });
  it("shows a started turn with nothing to say as one spinner on the margin, with no card", () => {
    const turn: ThreadTurn = { id: 4, requester_thread_id: null, requester_turn_id: null, thread_id: 1, owner_message_id: null, agent_message_id: null, state: "running", accepted_at: "2026-09-09T10:00:00Z", started_at: "2026-09-09T10:00:00Z", finished_at: null };
    flush(() => { dispatch({ type: "connection_status", status: "connected" }); setThreadState(draft => { draft.turnDetails = {}; }); });
    const view = render(() => <ThreadMessage entry={{ key: "turn-4", kind: "turn", turn }} history={emptyHistory()} threadId={1} />);
    const spinner = view.container.querySelector<HTMLElement>('[data-slot="turn-pending"]')!;
    expect(spinner).toHaveAttribute("role", "status");
    expect(spinner.querySelector(".animate-spin")).toBeInTheDocument();
    expect(spinner.textContent).toContain("Hirsel is working");
    expect(view.container.querySelector('[data-slot="agent-message"]')).toBeNull();
    expect(view.container.querySelector('[data-slot="thread-work"]')).toBeNull();
    // The first word the Agent produces brings the card, left-anchored as usual.
    flush(() => setThreadState(draft => { draft.turnDetails = { 4: [{ seq: 1, event: { kind: "reasoning", text: "Checking" } }] }; }));
    const bubble = view.container.querySelector<HTMLElement>('[data-slot="agent-message"]')!;
    expect(bubble).toBeInTheDocument();
    expect(bubble.className).toContain("max-w-[96%]");
    expect(view.container.querySelector('[data-slot="turn-pending"]')).toBeNull();
    expect(view.container.querySelector('[data-slot="work-live"]')).toBeInTheDocument();
  });
  it("renders a routine note as one centred line owned by neither party", () => {
    const activity: ThreadActivity = { artifact_ids: [], id: 7, thread_id: 1, turn_id: null, kind: "summary", ts: "2026-09-09T10:00:00Z", data: { content_md: "Created routine Morning review" } };
    const view = render(() => <ActivityEntry activity={activity} />);
    const note = view.container.querySelector<HTMLElement>('[data-slot="conversation-note"]')!;
    expect(note).toBeInTheDocument();
    expect(note.textContent).toContain("Created routine Morning review");
    expect(note.querySelector(".text-center")).toBeInTheDocument();
    const long = render(() => <ActivityEntry activity={{ ...activity, artifact_ids: [3] }} />);
    expect(long.container.querySelector('[data-slot="conversation-note"]')).toBeNull();
  });
});

it("renders stopped queued work without an execution duration or running avatar", () => {
  const turn: ThreadTurn = { id: 44, thread_id: 1, requester_thread_id: null, requester_turn_id: null, owner_message_id: null, agent_message_id: null, state: "cancelled", accepted_at: "2026-09-09T09:00:00Z", started_at: null, finished_at: "2026-09-09T10:00:00Z" };
  const view = render(() => <ThreadMessage entry={{ key: "turn-44", kind: "turn", turn }} history={emptyHistory()} threadId={1} />);
  expect(view.getByText("Stopped")).toBeInTheDocument();
  expect(view.container.querySelector(".animate-spin")).toBeNull();
  expect(view.container.textContent).not.toContain("1h");
});
