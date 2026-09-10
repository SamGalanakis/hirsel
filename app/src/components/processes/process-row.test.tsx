import { fireEvent, render } from "@solidjs/testing-library";
import { describe, expect, it } from "vitest";
import type { ProcessInfo } from "../../protocol";
import { ProcessRow } from "./ProcessRow";

// An honest resting row: two runs of the same monitor carry the SAME label,
// so the row has to say WHEN it started or the list is unorderable; and the
// disclosure mark has to describe what activating it does — these rows unfold
// in place, they do not navigate anywhere.

const AGO = (mins: number) => new Date(Date.now() - mins * 60_000).toISOString();

function finished(over: Partial<ProcessInfo> = {}): ProcessInfo {
  return { thread_id: 1,
    id: "proc-1",
    kind: "monitor",
    label: "Review the auth refactor",
    agent: null,
    model: null,
    state: "done",
    started_ts: AGO(12),
    last_event_ts: AGO(3),
    summary: "read 14 files",
    ...over,
  };
}

describe("ProcessRow: monitor rows tell the truth", () => {
  it("dates the row so two runs of the same monitor are orderable", () => {
    const { getByText, getAllByText } = render(() => (
      <ProcessRow process={finished()} />
    ));
    // Row mode (not the promoted card) — the start time is right there.
    expect(getByText("12m ago")).toBeTruthy();
    expect(getAllByText("Review the auth refactor")).toHaveLength(1);
  });

  it("reports aria-expanded honestly and unfolds in place on activation", async () => {
    const { getByRole, getByText } = render(() => (
      <ProcessRow process={finished()} />
    ));
    const disclosure = getByRole("button", { name: /Show details for/ });
    expect(disclosure.getAttribute("aria-expanded")).toBe("false");

    fireEvent.click(disclosure);
    // Promoted to the card presentation, open, and saying so.
    const open = getByRole("button", { expanded: true });
    expect(open).toBeTruthy();
    expect(getByText("Probe")).toBeTruthy();

    fireEvent.click(open);
    expect(getByRole("button", { name: /Show details for/ }).getAttribute("aria-expanded"))
      .toBe("false");
  });

  it("draws one rotating disclosure chevron, never a navigational ›", () => {
    const { container, getByRole } = render(() => (
      <ProcessRow process={finished()} />
    ));
    const closed = container.querySelector("svg.-rotate-90");
    expect(closed).not.toBeNull();

    fireEvent.click(getByRole("button", { name: /Show details for/ }));
    // Same mark, rotated open — not swapped for a different icon.
    expect(container.querySelector("svg.-rotate-90")).toBeNull();
  });
});
