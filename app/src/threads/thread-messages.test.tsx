import { render } from "@solidjs/testing-library";
import { describe, expect, it } from "vitest";
import { ThreadMessage } from "./ThreadMessages";
import { ActivityEntry } from "./ThreadWork";
import { emptyHistory } from "./model";
import type { ChatMessage } from "../protocol";
import type { ThreadActivity } from "./types";
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
  it("seats the Agent left on a neutral surface that keeps the measure", () => {
    const article = row("agent");
    expect(article).toHaveAttribute("data-author", "agent");
    expect(article.className).not.toContain("flex-row-reverse");
    const bubble = article.querySelector<HTMLElement>('[data-slot="agent-message"]')!;
    expect(bubble.className).toContain("bg-surface");
    expect(bubble.className).toContain("flex-1");
    expect(bubble.className).not.toContain("max-w-[60%]");
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
