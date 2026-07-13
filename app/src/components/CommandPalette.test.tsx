import { render, screen, waitFor } from "@solidjs/testing-library";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { CommandPalette, ShortcutHelp } from "./CommandPalette";

// `anyOverlayOpen` seam (parallel worktree) — mock so keymap's import resolves.
vi.mock("../lib/focus", () => ({
  anyOverlayOpen: () => false,
  focusMainComposer: () => {},
}));

describe("CommandPalette", () => {
  it("lists the core commands when open", async () => {
    render(() => <CommandPalette open onOpenChange={() => {}} />);
    await waitFor(() => expect(screen.getByRole("combobox")).toBeInTheDocument());
    expect(screen.getByText("Focus composer")).toBeInTheDocument();
    expect(screen.getByText("Open Pings")).toBeInTheDocument();
    expect(screen.getByText("Open Processes")).toBeInTheDocument();
  });

  it("filters commands by query", async () => {
    const user = userEvent.setup();
    render(() => <CommandPalette open onOpenChange={() => {}} />);
    const input = await screen.findByRole("combobox");
    await user.type(input, "process");
    await waitFor(() => {
      expect(screen.getByText("Open Processes")).toBeInTheDocument();
      expect(screen.queryByText("Focus composer")).not.toBeInTheDocument();
    });
  });

  it("runs the highlighted command on Enter and closes", async () => {
    const user = userEvent.setup();
    const onOpenChange = vi.fn();
    render(() => <CommandPalette open onOpenChange={onOpenChange} />);
    const input = await screen.findByRole("combobox");
    await user.type(input, "inbox");
    await user.keyboard("{Enter}");
    expect(onOpenChange).toHaveBeenCalledWith(false);
  });

  it("shows a fallback when nothing matches", async () => {
    const user = userEvent.setup();
    render(() => <CommandPalette open onOpenChange={() => {}} />);
    const input = await screen.findByRole("combobox");
    await user.type(input, "zzzznope");
    await waitFor(() => expect(screen.getByText("No matching commands")).toBeInTheDocument());
  });
});

describe("ShortcutHelp", () => {
  it("renders grouped shortcuts when open", async () => {
    render(() => <ShortcutHelp open onOpenChange={() => {}} />);
    await waitFor(() =>
      expect(screen.getByRole("heading", { name: "Keyboard shortcuts" })).toBeInTheDocument(),
    );
    expect(screen.getByText("Command palette")).toBeInTheDocument();
    expect(screen.getByText("Focus composer")).toBeInTheDocument();
    expect(screen.getByText("Jump to latest")).toBeInTheDocument();
  });
});
