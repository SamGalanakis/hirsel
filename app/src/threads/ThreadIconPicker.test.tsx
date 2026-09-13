import { flush } from "solid-js";
import { fireEvent, render, waitFor, within } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { dispatch } from "../store/store";
import { setHistoryId } from "../lib/history";
import { ThreadShell } from "./ThreadShell";
import { ThreadAvatar } from "./ThreadAvatar";
import { makeThread } from "./fixtures";
import { closeThreadNavigation } from "./navigation";
import { openThreadIconPicker, setThreadIconTarget } from "./icon-picker";
import { attachThreadTransport, disconnectThreads, handleThreadMessage, setThreadState, threadState } from "./store";
import type { ThreadClientMessage } from "./types";

vi.mock("../ws/client", () => ({
  getClient: () => ({
    cancelTurn: vi.fn(),
    getBlobUrl: vi.fn(async (id: string) => `https://example.test/blob/${id}`),
    uploadBlob: vi.fn(async () => ({ id: "image-blob", name: "thread-icon.webp", mime: "image/webp", size: 12 })),
  }),
  makeClientId: () => crypto.randomUUID(),
}));
vi.mock("./thread-icon-image", async importOriginal => ({
  ...await importOriginal<typeof import("./thread-icon-image")>(),
  normalizeThreadIconImage: vi.fn(async (file: File) => new File([file], "thread-icon.webp", { type: "image/webp" })),
}));
const sent: ThreadClientMessage[] = [];
beforeEach(() => {
  sent.length = 0;
  flush(() => {
    setHistoryId("icon-history"); closeThreadNavigation(); setThreadIconTarget(null);
    dispatch({ type: "connection_status", status: "connected" });
    setThreadState(draft => Object.assign(draft, { ready: true, linkError: null, threads: [makeThread(0, { title: "General" }), makeThread(1, { title: "Garden", read: true }), makeThread(2, { title: "Tools", icon: { kind: "symbol", name: "hammer", tint: "amber" } })], histories: {}, turnDetails: {}, pending: [], focusedId: 1, error: null }));
  });
  attachThreadTransport(frame => sent.push(frame));
});
afterEach(() => { disconnectThreads(); });

describe("Thread icons", () => {
  it("shows monogram defaults and symbol tiles for ordinary zero, rows and the active header", () => {
    const view = render(() => <ThreadShell />);
    expect(view.container.querySelector('header [data-thread-avatar="1"]')).toHaveTextContent("G");
    fireEvent.click(view.getByRole("button", { name: "Spaces and Tasks" }));
    for (const id of [0, 1]) {
      expect(view.container.querySelector(`[data-thread-row="${id}"] [data-thread-avatar]`)).toHaveTextContent("G");
    }
    const symbol = view.container.querySelector('[data-thread-row="2"] [data-thread-avatar]')!;
    expect(symbol).toHaveAttribute("data-thread-symbol", "hammer");
    expect(symbol).toHaveAttribute("data-thread-tint", "amber");
    expect(symbol.querySelector("svg path")).not.toBeNull();
  });
  it("opens from row actions, saves a symbol and tint with its revision and applies the server update everywhere", async () => {
    const view = render(() => <ThreadShell />);
    fireEvent.click(view.getByRole("button", { name: "Spaces and Tasks" }));
    fireEvent.click(view.getByRole("button", { name: "Actions for Garden" }));
    fireEvent.click(await view.findByRole("menuitem", { name: "Change Space icon" }));
    const picker = view.getByRole("dialog", { name: "Change thread icon" });
    fireEvent.click(within(picker).getByRole("button", { name: "leaf" }));
    fireEvent.click(within(picker).getByRole("button", { name: "green" }));
    fireEvent.click(within(picker).getByRole("button", { name: "Save icon" }));
    expect(sent).toContainEqual(expect.objectContaining({ type: "thread_action", history_id: "icon-history", thread_id: 1, action: "set_icon", data: { icon: { kind: "symbol", name: "leaf", tint: "green" } }, expected_revision: 1 }));
    expect(threadState.threads[1].icon).toBeNull();
    flush(() => handleThreadMessage({ type: "thread_upsert", thread: makeThread(1, { title: "Garden", icon: { kind: "symbol", name: "leaf", tint: "green" }, revision: 2 }) }));
    for (const node of [view.container.querySelector('header [data-thread-avatar="1"]'), view.container.querySelector('[data-thread-row="1"] [data-thread-avatar]')]) {
      expect(node).toHaveAttribute("data-thread-symbol", "leaf");
      expect(node).toHaveAttribute("data-thread-tint", "green");
    }
    expect(threadState.focusedId).toBe(1);
  });
  it("filters the grid by search and restores the monogram default", () => {
    const view = render(() => <ThreadShell />);
    flush(() => openThreadIconPicker(threadState.threads.find(thread => thread.id === 2)!));
    const picker = view.getByRole("dialog", { name: "Change thread icon" });
    fireEvent.input(within(picker).getByRole("searchbox", { name: "Search symbols" }), { target: { value: "git b" } });
    expect(within(picker).getByRole("button", { name: "git-branch" })).toBeInTheDocument();
    expect(within(picker).queryByRole("button", { name: "hammer" })).toBeNull();
    fireEvent.input(within(picker).getByRole("searchbox", { name: "Search symbols" }), { target: { value: "zzz" } });
    expect(within(picker).getByText(/No symbol matches/)).toBeInTheDocument();
    fireEvent.click(within(picker).getByRole("button", { name: "Use default" }));
    expect(picker.querySelector('[data-thread-avatar]')).toHaveTextContent("T");
    fireEvent.click(within(picker).getByRole("button", { name: "Save icon" }));
    expect(sent.at(-1)).toMatchObject({ thread_id: 2, data: { icon: null }, expected_revision: 1 });
  });
  it("previews every selection live and enables save only once the icon changes", async () => {
    const view = render(() => <ThreadShell />);
    flush(() => openThreadIconPicker(threadState.threads.find(thread => thread.id === 2)!));
    const picker = view.getByRole("dialog", { name: "Change thread icon" });
    const preview = () => picker.querySelector('[data-slot="thread-icon-preview"] [data-thread-avatar]')!;
    expect(preview()).toHaveAttribute("data-thread-symbol", "hammer");
    expect(within(picker).getByText("hammer")).toBeInTheDocument();
    expect(view.getByRole("button", { name: "Save icon" })).toBeDisabled();
    fireEvent.click(within(picker).getByRole("button", { name: "rocket" }));
    expect(preview()).toHaveAttribute("data-thread-symbol", "rocket");
    expect(within(picker).getByRole("button", { name: "rocket" })).toHaveAttribute("aria-pressed", "true");
    expect(view.getByRole("button", { name: "Save icon" })).toBeEnabled();
    const file = new File(["image bytes"], "bee.png", { type: "image/png" });
    fireEvent.change(within(picker).getByLabelText("Choose icon image"), { target: { files: [file] } });
    await waitFor(() => expect(preview().querySelector("img")).toHaveAttribute("src", "https://example.test/blob/image-blob"));
    expect(within(picker).getByText("Uploaded image")).toBeInTheDocument();
    fireEvent.click(within(picker).getByRole("button", { name: "Remove image" }));
    expect(preview().querySelector("img")).toBeNull();
    expect(preview()).toHaveTextContent("T");
    fireEvent.click(within(picker).getByRole("button", { name: "rocket" }));
    fireEvent.click(within(picker).getByRole("button", { name: "Save icon" }));
    expect(sent.at(-1)).toMatchObject({ thread_id: 2, data: { icon: { kind: "symbol", name: "rocket", tint: "amber" } }, expected_revision: 1 });
  });
  it("uploads a mock image, previews the signed blob, and sends the typed image icon", async () => {
    const view = render(() => <ThreadShell />);
    flush(() => openThreadIconPicker(threadState.threads[1]));
    const picker = view.getByRole("dialog", { name: "Change thread icon" });
    const file = new File(["image bytes"], "garden.png", { type: "image/png" });
    fireEvent.change(within(picker).getByLabelText("Choose icon image"), { target: { files: [file] } });
    await within(picker).findByRole("button", { name: "Remove image" });
    await waitFor(() => expect(picker.querySelector("img")).toHaveAttribute("src", "https://example.test/blob/image-blob"));
    fireEvent.click(within(picker).getByRole("button", { name: "Save icon" }));
    expect(sent.at(-1)).toMatchObject({ data: { icon: { kind: "image", blob_id: "image-blob" } }, expected_revision: 1 });
  });
  it("renders an image at avatar size and falls back to the monogram after a load error", async () => {
    const view = render(() => <ThreadAvatar thread={makeThread(8, { title: "Garden", icon: { kind: "image", blob_id: "garden-image" } })} dense />);
    const image = await waitFor(() => {
      const node = view.container.querySelector("img");
      expect(node).not.toBeNull();
      return node!;
    });
    expect(image).toHaveAttribute("alt", "");
    expect(view.container.firstElementChild).toHaveClass("size-4", "rounded-md");
    fireEvent.error(image);
    expect(view.container.firstElementChild).toHaveTextContent("G");
    expect(view.container.querySelector("img")).toBeNull();
  });
  it("keeps the edit revision fixed and exposes rejected requests through the existing error surface", () => {
    const view = render(() => <ThreadShell />);
    flush(() => openThreadIconPicker(threadState.threads[1]));
    flush(() => handleThreadMessage({ type: "thread_upsert", thread: makeThread(1, { title: "Garden", revision: 2 }) }));
    fireEvent.click(view.getByRole("button", { name: "rocket" }));
    fireEvent.click(view.getByRole("button", { name: "Save icon" }));
    expect(sent.at(-1)).toMatchObject({ expected_revision: 1 });
    flush(() => handleThreadMessage({ type: "error", detail: "Thread changed; retry with its current revision" }));
    expect(view.getAllByText("Hirsel couldn’t complete that request. Your conversation is kept.").length).toBeGreaterThan(0);
  });
  it("cancels without sending and closes across history changes", () => {
    const view = render(() => <ThreadShell />);
    flush(() => openThreadIconPicker(threadState.threads[1]));
    fireEvent.keyDown(view.getByRole("dialog", { name: "Change thread icon" }), { key: "Escape" });
    expect(sent).toEqual([]);
    flush(() => openThreadIconPicker(threadState.threads[1]));
    flush(() => setHistoryId("another-history"));
    expect(view.queryByRole("button", { name: "Save icon" })).toBeNull();
    expect(sent).toEqual([]);
  });
  it("disables save while disconnected", () => {
    const view = render(() => <ThreadShell />);
    flush(() => openThreadIconPicker(threadState.threads[1]));
    fireEvent.click(view.getByRole("button", { name: "star" }));
    expect(view.getByRole("button", { name: "Save icon" })).toBeEnabled();
    flush(() => dispatch({ type: "connection_status", status: "reconnecting" }));
    expect(view.getByRole("button", { name: "Save icon" })).toBeDisabled();
  });
  it("keeps the neutral default tile stable across title and lifecycle updates", () => {
    const view = render(() => <ThreadAvatar thread={threadState.threads[1]} />);
    const avatar = view.container.firstElementChild!;
    flush(() => handleThreadMessage({ type: "thread_upsert", thread: makeThread(1, { title: "Orchard Beds", settled_at: "2026-09-10T10:00:00Z", revision: 2 }) }));
    expect(avatar).toHaveAttribute("data-thread-tint", "neutral");
    expect(avatar).toHaveTextContent("OB");
    expect(avatar).toHaveAttribute("aria-hidden", "true");
  });
});
