import { fireEvent, render } from "@solidjs/testing-library";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Ping, QuickReply } from "../../protocol";

// Finish-tier Pings polish (spec items 5 + 7): the resting rail hierarchy (the
// open bucket is ALWAYS labelled whenever another section is visible, and Done
// is a demoted collapsed accordion) and the quick-reply cap. Fresh store
// singleton per test (resetModules + dynamic import), the suite's pattern.
beforeEach(() => {
  vi.resetModules();
});

function ping(overrides: Partial<Ping> = {}): Ping {
  return {
    id: 1,
    name: "change-approval",
    description: "Approve the change",
    content: "Approve?",
    anchor: 5,
    requires_response: false,
    quick_replies: [],
    status: "open",
    ts: "2026-07-08T00:00:00Z",
    ...overrides,
  };
}

function upsert(store: typeof import("../../store/store"), p: Ping) {
  store.dispatch({ type: "ping_upsert", payload: { type: "ping_upsert", ping: p } });
}

describe("PingsView: the open bucket is always labelled when another section shows", () => {
  it('labels a lone open bucket "Open" when Done is present, so open work leads', async () => {
    const store = await import("../../store/store");
    const { PingsView } = await import("./PingsView");
    const { container } = render(() => <PingsView />);

    upsert(store, ping({ id: 2, description: "Still open" }));
    upsert(store, ping({ id: 1, description: "Handled", status: "done", read: true }));

    // The single open, non-requires-response Ping no longer floats UNLABELED
    // above a prominent Done accordion — it leads under an "Open" header.
    expect(container.textContent).toContain("Open (1)");
    expect(container.textContent).toContain("Done (1)");
    expect(container.textContent).not.toContain("Needs reply");
  });

  it('labels it "Other" when a "Needs reply" section leads the column', async () => {
    const store = await import("../../store/store");
    const { PingsView } = await import("./PingsView");
    const { container } = render(() => <PingsView />);

    upsert(store, ping({ id: 3, description: "Answer me", requires_response: true }));
    upsert(store, ping({ id: 2, description: "Just so you know" }));

    expect(container.textContent).toContain("Needs reply (1)");
    expect(container.textContent).toContain("Other (1)");
    expect(container.textContent).not.toContain("Open (1)");
  });

  it("shows no bucket header when the open bucket is the only section", async () => {
    const store = await import("../../store/store");
    const { PingsView } = await import("./PingsView");
    const { container } = render(() => <PingsView />);

    upsert(store, ping({ id: 2, description: "Only thing here" }));

    expect(container.textContent).not.toContain("Open (");
    expect(container.textContent).not.toContain("Other (");
  });

  it("exposes the demoted Done accordion state via aria-expanded", async () => {
    const store = await import("../../store/store");
    const { PingsView } = await import("./PingsView");
    const { getByRole } = render(() => <PingsView />);

    upsert(store, ping({ id: 2, description: "Open one" }));
    upsert(store, ping({ id: 1, description: "Done one", status: "done", read: true }));

    const done = getByRole("button", { name: /Done/ });
    expect(done.getAttribute("aria-expanded")).toBe("false");
    fireEvent.click(done);
    expect(done.getAttribute("aria-expanded")).toBe("true");
  });
});

describe("PingsView: Done is dense history with an honest truncation note", () => {
  it("renders Done items as dense rows (not full cards) and caps at 20 with a note", async () => {
    const store = await import("../../store/store");
    const { PingsView } = await import("./PingsView");
    const { container, getAllByLabelText, getByRole } = render(() => <PingsView />);

    // 21 resolved Pings — one past the DONE_LIMIT cap of 20.
    for (let i = 1; i <= 21; i++) {
      upsert(store, ping({ id: 100 + i, name: `done-${i}`, description: `Done ${i}`, status: "done", read: true }));
    }

    // Expand the demoted Done accordion.
    fireEvent.click(getByRole("button", { name: /Done \(21\)/ }));

    // Dense rows expose a per-row "Open Ping <name>" button; a full expanded
    // card does not. Exactly the capped 20 show, and the cap is surfaced, not
    // silent (spec item 4).
    expect(getAllByLabelText(/^Open Ping done-/)).toHaveLength(20);
    expect(container.textContent).toContain("Showing latest 20");
  });

  it("shows no truncation note when Done is at or under the cap", async () => {
    const store = await import("../../store/store");
    const { PingsView } = await import("./PingsView");
    const { container, getByRole } = render(() => <PingsView />);

    for (let i = 1; i <= 20; i++) {
      upsert(store, ping({ id: 200 + i, name: `d-${i}`, description: `D ${i}`, status: "done", read: true }));
    }
    fireEvent.click(getByRole("button", { name: /Done \(20\)/ }));

    expect(container.textContent).not.toContain("Showing latest");
  });
});

describe("QuickReplyButtons: capped with a keyboard-reachable overflow", () => {
  function replies(n: number): QuickReply[] {
    return Array.from({ length: n }, (_, i) => ({ value: `v${i}`, label: `Reply ${i}` }));
  }

  it("caps the visible set and folds the rest behind a real button", async () => {
    const { QuickReplyButtons } = await import("./QuickReplyButtons");
    const { getAllByRole, getByRole } = render(() => (
      <QuickReplyButtons quickReplies={replies(6)} onTap={() => {}} />
    ));

    // 4 replies + a "+2 more" reveal — every option a focusable button.
    expect(getAllByRole("button")).toHaveLength(5);
    const more = getByRole("button", { name: /Show 2 more/ });
    fireEvent.click(more);
    // All 6 now visible, the overflow control gone.
    expect(getAllByRole("button")).toHaveLength(6);
  });

  it("renders every reply inline when at or under the cap", async () => {
    const { QuickReplyButtons } = await import("./QuickReplyButtons");
    const { getAllByRole } = render(() => (
      <QuickReplyButtons quickReplies={replies(3)} onTap={() => {}} />
    ));
    expect(getAllByRole("button")).toHaveLength(3);
  });
});
