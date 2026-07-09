import { render } from "@solidjs/testing-library";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { InboxItem } from "../../protocol";

// Tray (spec [P1]): the Inbox tab and TabBar are gone; these are the
// TabBar-badge-equivalent tests plus the new Tray-specific gates (shelf
// preview, 0-items hidden, never-auto-expand). Each test runs against a
// pristine store singleton, same pattern as flows.test.tsx.
beforeEach(() => {
  vi.resetModules();
});

function inboxItem(overrides: Partial<InboxItem> = {}): InboxItem {
  return {
    id: 1,
    content: "Approve the change?",
    anchor: 5,
    requires_response: false,
    quick_replies: [],
    status: "open",
    ts: "2026-07-08T00:00:00Z",
    ...overrides,
  };
}

describe("Tray shelf badge (email-like: open + unread, RR-driven accent)", () => {
  it("counts open unread items, clears as they are read/deleted, and toggles the danger accent on requires_response", async () => {
    const store = await import("../../store/store");
    const { TrayShelf } = await import("./Tray");
    const { queryByLabelText, getByLabelText } = render(() => <TrayShelf />);

    // No items yet -> shelf hidden entirely (0-items rule).
    expect(queryByLabelText("Open inbox")).toBeNull();

    const badge = () =>
      getByLabelText("Open inbox").querySelector(".font-bold") as HTMLElement;

    // Open, unread, non-RR item: badge 1, muted (no danger accent).
    store.dispatch({
      type: "inbox_upsert",
      payload: { type: "inbox_upsert", item: inboxItem({ id: 1, requires_response: false }) },
    });
    expect(badge().textContent).toBe("1");
    expect(badge().classList.contains("bg-muted-foreground")).toBe(true);
    expect(badge().classList.contains("bg-status-danger")).toBe(false);

    // A second open+unread+RR item bumps the badge to 2 AND flips the accent.
    store.dispatch({
      type: "inbox_upsert",
      payload: { type: "inbox_upsert", item: inboxItem({ id: 2, requires_response: true }) },
    });
    expect(badge().textContent).toBe("2");
    expect(badge().classList.contains("bg-status-danger")).toBe(true);

    // Reading the RR item drops the badge back to 1; the danger accent
    // persists because it tracks "any open requires_response", not the
    // unread count.
    store.dispatch({
      type: "inbox_upsert",
      payload: {
        type: "inbox_upsert",
        item: inboxItem({ id: 2, requires_response: true, read: true }),
      },
    });
    expect(badge().textContent).toBe("1");
    expect(badge().classList.contains("bg-status-danger")).toBe(true);

    // Deleting the RR item clears the accent; item 1 (open, unread) keeps the
    // badge at 1.
    store.dispatch({
      type: "inbox_upsert",
      payload: {
        type: "inbox_upsert",
        item: inboxItem({ id: 2, requires_response: true, read: true, status: "archived" }),
      },
    });
    expect(badge().textContent).toBe("1");
    expect(badge().classList.contains("bg-status-danger")).toBe(false);
  });
});

describe("Tray shelf preview", () => {
  it("previews the most-actionable open item: requires_response > unread > newest open", async () => {
    const store = await import("../../store/store");
    const { TrayShelf } = await import("./Tray");
    const { getByLabelText } = render(() => <TrayShelf />);

    // Two read, non-RR items -> falls back to "newest open" (highest id).
    store.dispatch({
      type: "inbox_upsert",
      payload: { type: "inbox_upsert", item: inboxItem({ id: 1, content: "First item", read: true }) },
    });
    store.dispatch({
      type: "inbox_upsert",
      payload: { type: "inbox_upsert", item: inboxItem({ id: 2, content: "Second item", read: true }) },
    });
    expect(getByLabelText("Open inbox").textContent).toContain("Second item");

    // An unread (but non-RR, older) item beats "newest open".
    store.dispatch({
      type: "inbox_upsert",
      payload: {
        type: "inbox_upsert",
        item: inboxItem({ id: 1, content: "First item", read: false }),
      },
    });
    expect(getByLabelText("Open inbox").textContent).toContain("First item");

    // A requires_response item beats an unread one, regardless of recency.
    store.dispatch({
      type: "inbox_upsert",
      payload: {
        type: "inbox_upsert",
        item: inboxItem({ id: 3, content: "Needs your call", requires_response: true, read: true }),
      },
    });
    expect(getByLabelText("Open inbox").textContent).toContain("Needs your call");
  });
});

describe("Tray shelf visibility", () => {
  it("hides entirely with no items, and offers only a minimal Done handle once resolved items exist", async () => {
    const store = await import("../../store/store");
    const { TrayShelf } = await import("./Tray");
    const { queryByLabelText, getByLabelText } = render(() => <TrayShelf />);

    // 0 open, 0 done -> nothing rendered.
    expect(queryByLabelText("Open inbox")).toBeNull();
    expect(queryByLabelText("Open done items")).toBeNull();

    // 0 open, 1 done -> a minimal handle, not the full glyph/badge/preview shelf.
    // (`done` is the ADR-0009 status; the legacy `archived` spelling still
    // renders here too — covered by the synonym in isResolvedStatus.)
    store.dispatch({
      type: "inbox_upsert",
      payload: {
        type: "inbox_upsert",
        item: inboxItem({ id: 1, status: "done", read: true }),
      },
    });
    expect(queryByLabelText("Open inbox")).toBeNull();
    expect(getByLabelText("Open done items").textContent).toBe("Done (1)");

    // An open item arrives -> the full shelf replaces the minimal handle.
    store.dispatch({
      type: "inbox_upsert",
      payload: { type: "inbox_upsert", item: inboxItem({ id: 2, status: "open" }) },
    });
    expect(queryByLabelText("Open done items")).toBeNull();
    expect(getByLabelText("Open inbox")).toBeTruthy();
  });
});

describe("Tray never auto-expands", () => {
  it("stays collapsed — and the overlay panel stays unmounted — when a new inbox item arrives", async () => {
    const store = await import("../../store/store");
    const { TrayOverlay } = await import("./Tray");
    const { container } = render(() => <TrayOverlay />);

    expect(store.state.trayExpanded).toBe(false);
    expect(container.querySelector('[data-slot="tray-panel"]')).toBeNull();

    store.dispatch({
      type: "inbox_upsert",
      payload: {
        type: "inbox_upsert",
        item: inboxItem({ id: 1, requires_response: true }),
      },
    });

    // A requires_response item arriving is the most "urgent" possible event
    // the Tray can receive — it still must not auto-expand.
    expect(store.state.trayExpanded).toBe(false);
    expect(container.querySelector('[data-slot="tray-panel"]')).toBeNull();
  });
});
