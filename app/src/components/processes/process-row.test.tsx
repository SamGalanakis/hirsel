import { fireEvent, render } from "@solidjs/testing-library";
import { describe, expect, it, vi } from "vitest";
import type { ProcessInfo } from "../../protocol";
import { ProcessRow } from "./ProcessRow";

// Two runs of the same process may carry the same name,
// so the row has to say WHEN it started or the list is unorderable; and the
// disclosure mark has to describe what activating it does — these rows unfold
// in place, they do not navigate anywhere.

const AGO = (mins: number) => new Date(Date.now() - mins * 60_000).toISOString();

function finished(over: Partial<ProcessInfo> = {}): ProcessInfo {
  return { thread_id: 1,
    id: "proc-1",
    name: "Review the auth refactor",
    trigger: "cron */5 * * * *",
    trigger_subscription_key: "review-auth",
    trigger_revision: 3,
    trigger_enabled: true,
    cancellable: true,
    state: "done",
    started_ts: AGO(12),
    last_event_ts: AGO(3),
    last_fired_ts: AGO(3),
    last_outcome: "read 14 files",
    ...over,
  };
}

describe("ProcessRow: registry-backed process rows", () => {
  it("dates the row so two runs of the same process are orderable", () => {
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
    expect(getByText("Trigger")).toBeTruthy();

    fireEvent.click(open);
    expect(getByRole("button", { name: /Show details for/ }).getAttribute("aria-expanded"))
      .toBe("false");
  });

  it("wires cancel and trigger-disable actions", () => {
    const cancel = vi.fn();
    const disable = vi.fn();
    const process = finished({ state: "waiting" });
    const { getByRole } = render(() => (
      <ProcessRow process={process} onCancel={cancel} onDisableTrigger={disable} />
    ));
    fireEvent.click(getByRole("button", { name: "Show details for Review the auth refactor" }));
    fireEvent.click(getByRole("button", { name: "Cancel process" }));
    fireEvent.click(getByRole("button", { name: "Disable trigger" }));
    expect(cancel).toHaveBeenCalledWith(process);
    expect(disable).toHaveBeenCalledWith(process);
  });

  it("does not offer cancellation before a registered trigger has fired", () => {
    const process = finished({ state: "waiting", cancellable: false });
    const { getByRole, queryByRole } = render(() => (
      <ProcessRow process={process} onCancel={vi.fn()} onDisableTrigger={vi.fn()} />
    ));
    fireEvent.click(getByRole("button", { name: "Show details for Review the auth refactor" }));
    expect(queryByRole("button", { name: "Cancel process" })).toBeNull();
    expect(getByRole("button", { name: "Disable trigger" })).toBeTruthy();
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
