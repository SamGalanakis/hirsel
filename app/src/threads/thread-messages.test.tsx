import { render } from "@solidjs/testing-library";
import { describe, expect, it } from "vitest";
import { ThreadMessage } from "./ThreadMessages";
import { emptyHistory } from "./model";
import type { ChatMessage } from "../protocol";

const msg: ChatMessage = { id: 9, thread_id: 1, author: "owner", body: "Ready", ref: null, ts: "2026-09-09T10:00:00Z", tool_calls: [] };

describe("who is speaking", () => {
  it("seats the Owner right in a filled bubble and the Agent left in a surface bubble at conversational width", () => {
    const seat = (author: ChatMessage["author"]) => {
      const view = render(() => <ThreadMessage entry={{ key: "row", kind: "message", message: { ...msg, author } }} history={emptyHistory()} threadId={1} />);
      const article = view.container.querySelector("article")!;
      const bubble = article.querySelector<HTMLElement>(`[data-slot="${author === "owner" ? "owner" : "agent"}-message"]`)!;
      return { article, bubble };
    };
    const owner = seat("owner");
    expect(owner.article.className).toContain("flex-row-reverse");
    expect(owner.bubble.className).toContain("bg-primary");
    expect(owner.bubble.className).toContain("max-w-reply");
    expect(owner.bubble.className).not.toContain("%");
    const agent = seat("agent");
    expect(agent.article.className).not.toContain("flex-row-reverse");
    expect(agent.bubble.className).toContain("bg-surface");
    // The Agent is a left bubble, not a full-width panel: the same named rem
    // ceiling the Owner's right bubble wears, so the two mirror each other.
    expect(agent.bubble.className).toContain("max-w-reply");
    expect(agent.bubble.className).not.toContain("flex-1");
  });
});
