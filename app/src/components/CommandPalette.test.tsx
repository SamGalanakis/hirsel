import { setHistoryId } from "../lib/history";
import { threadNavigationOpen, closeThreadNavigation } from "../threads/navigation";
import { flush } from "solid-js";
import { render, screen, waitFor } from "@solidjs/testing-library";
import userEvent from "@testing-library/user-event";
import { beforeEach, afterEach, describe, expect, it, vi } from "vitest";
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

beforeEach(()=>{const storage=new Map<string,string>();vi.stubGlobal("localStorage",{getItem:(key:string)=>storage.get(key)??null,setItem:(key:string,value:string)=>storage.set(key,value),removeItem:(key:string)=>storage.delete(key)});});
afterEach(()=>vi.unstubAllGlobals());
describe("CommandPalette", () => {
  it("lists the core commands when open", async () => {
    render(() => <CommandPalette open onOpenChange={() => {}} />);
    await waitFor(() => expect(screen.getByRole("combobox")).toBeInTheDocument());
    expect(screen.getAllByText("Focus conversation").length).toBeGreaterThan(0);
    expect(screen.getByText("Open threads")).toBeInTheDocument();
    expect(screen.getByText("Open Processes")).toBeInTheDocument();
  });

  it("opens the shared Thread drawer through its navigation command", async () => {
    flush(() => closeThreadNavigation());
    const onOpenChange = vi.fn();
    render(() => <CommandPalette open onOpenChange={onOpenChange} />);
    await userEvent.setup().click(await screen.findByText("Open threads"));
    await waitFor(() => expect(threadNavigationOpen()).toBe(true));
    expect(onOpenChange).toHaveBeenCalledWith(false);
    flush(() => closeThreadNavigation());
  });

  it("finds older Threads by title and exact reference without selecting during search", async () => {
    flush(() => setThreadState(draft => { setHistoryId("palette-history"); draft.ready = true; draft.focusedId = 1; draft.threads = [makeThread(0, { title: "Orchestrator" }), makeThread(1), makeThread(88, { title: "Archived launch notes", archived_at: "2026-09-09T10:00:00Z" })]; }));
    const user = userEvent.setup(); const onOpenChange = vi.fn();
    render(() => <CommandPalette open onOpenChange={onOpenChange} />);
    const input = await screen.findByRole("combobox");
    await user.type(input, "launch notes");
    expect(await screen.findByRole("option", { name: /Archived launch notes.*#88.*archived/ })).toBeTruthy();
    expect(threadState.focusedId).toBe(1);
    await user.clear(input); await user.type(input, "#88");
    expect(screen.getAllByRole("option")).toHaveLength(1);
    expect(threadState.focusedId).toBe(1);
    await user.keyboard("{Enter}");
    await waitFor(() => expect(threadState.focusedId).toBe(88));
    expect(onOpenChange).toHaveBeenCalledWith(false);
    flush(() => setThreadState(draft => { setHistoryId("palette-history"); draft.ready = true; draft.focusedId = 0; draft.threads = []; }));
  });

  it("filters commands by query", async () => {
    const user = userEvent.setup();
    render(() => <CommandPalette open onOpenChange={() => {}} />);
    const input = await screen.findByRole("combobox");
    await user.type(input, "process");
    await waitFor(() => {
      expect(screen.getByText("Open Processes")).toBeInTheDocument();
      expect(screen.queryByText("Focus conversation")).not.toBeInTheDocument();
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

  it("offers retained zero as an ordinary named destination", async () => {
    flush(() => setThreadState(draft => { setHistoryId("palette-history"); draft.ready = true; draft.focusedId = null; draft.threads = [makeThread(0, { title: "General" })]; }));
    render(() => <CommandPalette open intent="threads" onOpenChange={() => {}} />);
    await userEvent.setup().click(await screen.findByText("General"));
    expect(threadState.focusedId).toBe(0);
    expect(location.pathname).toBe("/t/0");
  });
  it("does not invent a hidden search destination for an unmatched query", async () => {
    const user = userEvent.setup();
    render(() => <CommandPalette open onOpenChange={() => {}} />);
    const input = await screen.findByRole("combobox");
    await user.type(input, "zzzznope");
    await waitFor(() => expect(screen.getByText("No matching commands or threads")).toBeInTheDocument());
  });
});

describe("Thread lifecycle commands", () => {
  it("offers explicit settlement without deriving completion from read", async () => {
    flush(() => setThreadState(draft => { setHistoryId("palette-history"); draft.ready = true; Object.assign(draft, { threads: [makeThread(11, { read: true })], focusedId: 11 }); }));
    render(() => <CommandPalette open onOpenChange={() => {}} />);
    await waitFor(() => expect(screen.getByText("Settle thread")).toBeInTheDocument());
    expect(screen.queryByText(/Clear finished/)).toBeNull();
    flush(() => setThreadState(draft => { setHistoryId("palette-history"); draft.ready = true; Object.assign(draft, { threads: [], focusedId: 0 }); }));
  });
});

describe("ShortcutHelp", () => {
  it("renders grouped shortcuts when open", async () => {
    render(() => <ShortcutHelp open onOpenChange={() => {}} />);
    await waitFor(() =>
      expect(screen.getByRole("heading", { name: "Keyboard shortcuts" })).toBeInTheDocument(),
    );
    expect(screen.getByText("Search commands and threads")).toBeInTheDocument();
    expect(screen.getAllByText("Focus conversation").length).toBeGreaterThan(0);
    expect(screen.getByText("Jump to latest")).toBeInTheDocument();
  });
});

it("opens Thread search with only Thread destinations while the command palette retains commands", async () => {
  const store=await import("../threads/store");
  const { makeThread }=await import("../threads/fixtures");
  flush(()=>store.setThreadState(draft=>{ draft.threads=[makeThread(7,{title:"Find groceries"})]; }));
  render(()=> <CommandPalette open intent="threads" onOpenChange={()=>{}} />);
  expect(await screen.findByRole("combobox",{name:"Search threads"})).toBeTruthy();
  expect(screen.getAllByRole("option").map(option=>option.textContent)).toHaveLength(1);
  expect(screen.getByRole("option")).toHaveTextContent("Find groceries");
  expect(screen.queryByRole("option",{name:/Open Settings/})).toBeNull();
});
