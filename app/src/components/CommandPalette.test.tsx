import { render, screen, waitFor } from "@solidjs/testing-library";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { CommandPalette, ShortcutHelp } from "./CommandPalette";

// `anyOverlayOpen` seam (parallel worktree) — mock so keymap's import resolves.
vi.mock("../lib/focus", () => ({
  anyOverlayOpen: () => false,
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
      kind: "judgment" as const,
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
    const readInfo = {
      ...judgment,
      id: 4243,
      kind: "info" as const,
      requires_response: false,
      read: true,
      ui: [{ type: "status", label: "done" }],
    };
    store.dispatch({ type: "event_upsert", payload: { type: "event_upsert", event: judgment } });
    store.dispatch({ type: "event_upsert", payload: { type: "event_upsert", event: readInfo } });

    render(() => <CommandPalette open onOpenChange={() => {}} />);
    await waitFor(() => expect(screen.getByText(/Decide A — Alpha/)).toBeInTheDocument());
    expect(screen.getByText(/Decide B — Bravo/)).toBeInTheDocument();
    expect(screen.getByText(/Snooze current · This evening/)).toBeInTheDocument();
    expect(screen.getByText("Archive current")).toBeInTheDocument();
    // The read info card is finished → the sweep offers it (count 1).
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
  });
});
