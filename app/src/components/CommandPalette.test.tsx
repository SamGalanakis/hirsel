import { render, screen, waitFor } from "@solidjs/testing-library";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { CommandPalette, ShortcutHelp } from "./CommandPalette";
import { EventKind } from "../protocol";

// This suite is about the palette's own list/filter behaviour, so the focus
// module is stubbed out. The real overlay-presence registry is exercised in
// `src/lib/overlay-presence.test.tsx` instead.
vi.mock("../lib/focus", () => ({
  anyOverlayOpen: () => false,
  createOverlayPresence: () => {},
  focusMainComposer: () => {},
  focusTaskIndex: () => {},
}));

describe("CommandPalette", () => {
  it("lists the core commands when open", async () => {
    render(() => <CommandPalette open onOpenChange={() => {}} />);
    await waitFor(() => expect(screen.getByRole("combobox")).toBeInTheDocument());
    expect(screen.getAllByText("Focus Hirsel").length).toBeGreaterThan(0);
    expect(screen.getByText("Focus tasks")).toBeInTheDocument();
    expect(screen.getByText("Open Processes")).toBeInTheDocument();
  });

  it("filters commands by query", async () => {
    const user = userEvent.setup();
    render(() => <CommandPalette open onOpenChange={() => {}} />);
    const input = await screen.findByRole("combobox");
    await user.type(input, "process");
    await waitFor(() => {
      expect(screen.getByText("Open Processes")).toBeInTheDocument();
      expect(screen.queryByText("Focus Hirsel")).not.toBeInTheDocument();
    });
  });

  it("runs the highlighted command on Enter and closes", async () => {
    const user = userEvent.setup();
    const onOpenChange = vi.fn();
    render(() => <CommandPalette open onOpenChange={onOpenChange} />);
    const input = await screen.findByRole("combobox");
    await user.type(input, "process");
    await user.keyboard("{Enter}");
    expect(onOpenChange).toHaveBeenCalledWith(false);
  });

  it("offers the focus exit only while a task is focused", async () => {
    const store = await import("../store/store");
    const { unmount } = render(() => <CommandPalette open onOpenChange={() => {}} />);
    await waitFor(() => expect(screen.getByRole("combobox")).toBeInTheDocument());
    expect(screen.queryByText("Clear task focus")).toBeNull();
    unmount();

    store.toggleTaskFocus(11);
    render(() => <CommandPalette open onOpenChange={() => {}} />);
    await waitFor(() => expect(screen.getByText("Clear task focus")).toBeInTheDocument());
    await userEvent.setup().click(screen.getByText("Clear task focus"));
    await waitFor(() => expect(store.state.focusedTaskId).toBeNull());
  });

  it("does not invent a hidden search destination for an unmatched query", async () => {
    const user = userEvent.setup();
    render(() => <CommandPalette open onOpenChange={() => {}} />);
    const input = await screen.findByRole("combobox");
    await user.type(input, "zzzznope");
    await waitFor(() => expect(screen.getByText("No matching commands")).toBeInTheDocument());
  });
});

describe("CommandPalette — contextual queue actions (Wave-3 ⌘K depth)", () => {
  it("surfaces Decide / Snooze / Clear-finished for the current queue", async () => {
    const store = await import("../store/store");
    const judgment = {
      id: 4242,
      kind: EventKind.Judgment,
      source: { kind: "agent" as const, ref: "host" },
      name: "@ctx",
      description: "context judgment",
      requires_response: true,
      quick_replies: [],
      status: "open" as const,
      read: false,
      anchor: 0,
      ts: "2026-07-14T09:00:00Z",
      ui: [
        { type: "heading", text: "Ctx" },
        {
          type: "optionList",
          action: "choose",
          options: [{ key: "A", label: "Alpha" }, { key: "B", label: "Bravo" }],
        },
      ],
    };
    const readSummary = {
      ...judgment,
      id: 4243,
      kind: EventKind.Summary,
      requires_response: false,
      read: true,
      ui: [{ type: "status", label: "done" }],
    };
    // Housekeeping info is not a Task, so it never joins the sweep's count.
    const readInfo = { ...readSummary, id: 4244, kind: EventKind.Info };
    store.dispatch({ type: "event_upsert", payload: { type: "event_upsert", event: judgment } });
    store.dispatch({ type: "event_upsert", payload: { type: "event_upsert", event: readSummary } });
    store.dispatch({ type: "event_upsert", payload: { type: "event_upsert", event: readInfo } });

    render(() => <CommandPalette open onOpenChange={() => {}} />);
    await waitFor(() => expect(screen.getByText(/Decide A — Alpha/)).toBeInTheDocument());
    expect(screen.getByText(/Decide B — Bravo/)).toBeInTheDocument();
    expect(screen.getByText(/Snooze current · This evening/)).toBeInTheDocument();
    expect(screen.getByText("Archive current")).toBeInTheDocument();
    // The read summary card is finished → the sweep offers it (count 1); the
    // read info card sits outside the Task set entirely and is not counted.
    expect(screen.getByText(/Clear finished \(1\)/)).toBeInTheDocument();

    // Clean the store so the earlier assertions in this file's other suites are
    // unaffected by ordering (the singleton persists across tests).
    store.dispatch({
      type: "hello_ok",
      payload: { type: "hello_ok", latest_msg_id: 0, messages: [], pings: [], events: [] },
    });
  });
});

describe("ShortcutHelp", () => {
  it("renders grouped shortcuts when open", async () => {
    render(() => <ShortcutHelp open onOpenChange={() => {}} />);
    await waitFor(() =>
      expect(screen.getByRole("heading", { name: "Keyboard shortcuts" })).toBeInTheDocument(),
    );
    expect(screen.getByText("Command palette")).toBeInTheDocument();
    expect(screen.getAllByText("Focus Hirsel").length).toBeGreaterThan(0);
    expect(screen.getByText("Jump to latest")).toBeInTheDocument();
    // The Tasks group is no longer an empty heading the sheet silently drops.
    expect(screen.getByText("Tasks")).toBeInTheDocument();
    expect(screen.getByText("Clear task focus")).toBeInTheDocument();
  });
});
