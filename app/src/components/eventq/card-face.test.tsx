import { render } from "@solidjs/testing-library";
import { describe, expect, it } from "vitest";
import type { EventItem } from "../../protocol";
import { EventCardRenderer } from "../../views/EventCardRenderer";
import { EventCardHeader } from "./EventCard";

// Card-face diet (Wave 1): the mono `@name` slug and the kind micro-label have
// left the card FACE; the source is shown only when it isn't the resident agent;
// the eyebrow speaks the human unblocks fact.

function ev(overrides: Partial<EventItem> = {}): EventItem {
  return {
    id: 1,
    kind: "judgment",
    source: { kind: "subagent", ref: "hirsel-ui" },
    name: "@reopen-op-shape",
    description: "How should reopening be wired?",
    ui: [{ type: "heading", text: "How should reopening be wired?" }],
    requires_response: true,
    quick_replies: [],
    status: "open",
    read: false,
    anchor: 0,
    ts: "2026-07-13T09:19:00Z",
    blocking: true,
    ...overrides,
  };
}

describe("EventCardHeader — card-face diet", () => {
  it("does not render the mono @name slug on the face", () => {
    const { queryByText, container } = render(() => <EventCardHeader ev={ev()} />);
    expect(queryByText("@reopen-op-shape")).toBeNull();
    // No mono span at all in the header (the slug was the only one).
    expect(container.querySelector(".font-mono")).toBeNull();
  });

  it("does not render the kind micro-label on the face", () => {
    expect(
      render(() => <EventCardHeader ev={ev({ kind: "judgment" })} />).queryByText("Judgment"),
    ).toBeNull();
    expect(
      render(() => <EventCardHeader ev={ev({ kind: "summary" })} />).queryByText("Summary"),
    ).toBeNull();
    expect(render(() => <EventCardHeader ev={ev({ kind: "info" })} />).queryByText("Info")).toBeNull();
  });

  it("shows the source only when it is not the resident agent", () => {
    expect(
      render(() => <EventCardHeader ev={ev({ source: { kind: "subagent", ref: "hirsel-ui" } })} />)
        .queryByText("hirsel-ui"),
    ).not.toBeNull();
    expect(
      render(() => <EventCardHeader ev={ev({ source: { kind: "agent", ref: "hirsel-host" } })} />)
        .queryByText("hirsel-host"),
    ).toBeNull();
  });

  it("keeps the accent stripe on an open judgment", () => {
    const { container } = render(() => <EventCardHeader ev={ev()} />);
    expect(container.querySelector(".bg-primary")).not.toBeNull();
  });

  it("keeps the small event age at a contrast-safe foreground step", () => {
    const { container } = render(() => <EventCardHeader ev={ev()} />);
    expect(container.querySelector('[data-slot="event-age"]')?.className).toContain(
      "text-foreground/80",
    );
  });
});

describe("EventCardRenderer — eyebrow speaks the unblocks fact", () => {
  it("renders the unblocks text and keeps the boundary stripe", () => {
    const { getByText, container } = render(() => (
      <EventCardRenderer
        ui={[
          { type: "eyebrow", tone: "accent", boundary: true, text: "Deciding unblocks 2 agents" },
          { type: "heading", text: "Which model?" },
        ]}
      />
    ));
    expect(getByText("Deciding unblocks 2 agents")).toBeTruthy();
    // The boundary flag draws the indigo stripe.
    expect(container.querySelector('[aria-hidden="true"].bg-primary\\/40')).not.toBeNull();
    // The old fixed copy is gone.
    expect(container.textContent).not.toMatch(/fleet stopped/i);
  });
});
