import { threadNavigationOpen, setThreadNavigationOpen } from "../threads/navigation";
import { flush } from "solid-js";
import { render, screen, waitFor } from "@solidjs/testing-library";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { CommandPalette, ShortcutHelp } from "./CommandPalette";
import { makeThread } from "../threads/fixtures";
import { setThreadState, threadState } from "../threads/store";

// This suite is about the palette's own list/filter behaviour, so the focus
// module is stubbed out. The real overlay-presence registry is exercised in
// `src/lib/overlay-presence.test.tsx` instead.
vi.mock("../lib/focus", () => ({
  anyOverlayOpen: () => false,
  createOverlayPresence: () => {},
  createFocusTrap: () => {},
  focusMainComposer: () => {},
}));

describe("CommandPalette", () => {
  it("lists the core commands when open", async () => {
    render(() => <CommandPalette open onOpenChange={() => {}} />);
    await waitFor(() => expect(screen.getByRole("combobox")).toBeInTheDocument());
    expect(screen.getAllByText("Focus Hirsel").length).toBeGreaterThan(0);
    expect(screen.getByText("Open threads")).toBeInTheDocument();
    expect(screen.getByText("Open Processes")).toBeInTheDocument();
  });

  it("opens the shared Thread drawer through its navigation command", async () => {
    flush(() => setThreadNavigationOpen(false));
    const onOpenChange = vi.fn();
    render(() => <CommandPalette open onOpenChange={onOpenChange} />);
    await userEvent.setup().click(await screen.findByText("Open threads"));
    await waitFor(() => expect(threadNavigationOpen()).toBe(true));
    expect(onOpenChange).toHaveBeenCalledWith(false);
    flush(() => setThreadNavigationOpen(false));
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
    const { unmount } = render(() => <CommandPalette open onOpenChange={() => {}} />);
    await waitFor(() => expect(screen.getByRole("combobox")).toBeInTheDocument());
    expect(screen.queryByText("Open Hirsel")).toBeNull();
    unmount();

    flush(() => setThreadState(draft => { draft["focusedId"] = 11; }));
    render(() => <CommandPalette open onOpenChange={() => {}} />);
    await waitFor(() => expect(screen.getByText("Open Hirsel")).toBeInTheDocument());
    await userEvent.setup().click(screen.getByText("Open Hirsel"));
    await waitFor(() => expect(threadState.focusedId).toBe(0));
  });

  it("does not invent a hidden search destination for an unmatched query", async () => {
    const user = userEvent.setup();
    render(() => <CommandPalette open onOpenChange={() => {}} />);
    const input = await screen.findByRole("combobox");
    await user.type(input, "zzzznope");
    await waitFor(() => expect(screen.getByText("No matching commands")).toBeInTheDocument());
  });
});

describe("Thread lifecycle commands", () => {
  it("offers explicit settlement without deriving completion from read", async () => {
    flush(() => setThreadState(draft => { Object.assign(draft, { threads: [makeThread(11, { read: true })], focusedId: 11 }); }));
    render(() => <CommandPalette open onOpenChange={() => {}} />);
    await waitFor(() => expect(screen.getByText("Settle thread")).toBeInTheDocument());
    expect(screen.queryByText(/Clear finished/)).toBeNull();
    flush(() => setThreadState(draft => { Object.assign(draft, { threads: [], focusedId: 0 }); }));
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
    // The Threads group is no longer an empty heading the sheet silently drops.
    expect(screen.getByText("Threads")).toBeInTheDocument();
    expect(screen.getByText("Open Hirsel")).toBeInTheDocument();
  });
});
