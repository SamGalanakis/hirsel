import { flush } from "solid-js";
import { fireEvent, render, within } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { dispatch } from "../store/store";
import { setHistoryId } from "../lib/history";
import { ThreadShell } from "./ThreadShell";
import { ThreadAvatar } from "./ThreadAvatar";
import { makeThread } from "./fixtures";
import { closeThreadNavigation } from "./navigation";
import { openThreadIconPicker, setThreadIconTarget, threadIconError } from "./icon-picker";
import { attachThreadTransport, disconnectThreads, handleThreadMessage, setThreadState, threadState } from "./store";
import type { ThreadClientMessage } from "./types";

vi.mock("../ws/client", () => ({ getClient: () => ({ cancelTurn: vi.fn() }), makeClientId: () => crypto.randomUUID() }));
const sent: ThreadClientMessage[] = [];
beforeEach(() => {
  sent.length = 0;
  flush(() => {
    setHistoryId("icon-history"); closeThreadNavigation(); setThreadIconTarget(null);
    dispatch({ type: "connection_status", status: "connected" });
    setThreadState(draft => Object.assign(draft, { ready: true, linkError: null, threads: [makeThread(0, { title: "General" }), makeThread(1, { title: "Garden", read: true }), makeThread(2, { title: "Tools", icon: "🛠️" })], histories: {}, streams: {}, streamTurnIds: {}, turnDetails: {}, pending: [], focusedId: 1, error: null }));
  });
  attachThreadTransport(frame => sent.push(frame));
});
afterEach(() => { disconnectThreads(); });

describe("Thread icons", () => {
  it("shows defaults and custom icons for ordinary zero, rows and the active header", () => {
    const view = render(() => <ThreadShell />);
    expect(view.container.querySelector('header [data-thread-avatar="1"]')).toHaveTextContent("G");
    fireEvent.click(view.getByRole("button", { name: "Threads" }));
    for (const [id, icon] of [[0, "G"], [1, "G"], [2, "🛠️"]]) {
      expect(view.container.querySelector(`[data-thread-row="${id}"] [data-thread-avatar]`)).toHaveTextContent(String(icon));
    }
  });
  it("opens from row actions, saves a preset with its revision and applies the server update everywhere", async () => {
    const view = render(() => <ThreadShell />);
    fireEvent.click(view.getByRole("button", { name: "Threads" }));
    fireEvent.click(view.getByRole("button", { name: "Actions for Garden" }));
    fireEvent.click(await view.findByRole("menuitem", { name: "Change thread icon" }));
    const picker = view.getByRole("dialog", { name: "Change thread icon" });
    fireEvent.click(within(picker).getByRole("button", { name: "Seedling" }));
    fireEvent.click(within(picker).getByRole("button", { name: "Save icon" }));
    expect(sent).toContainEqual({ type: "thread_action", history_id: "icon-history", thread_id: 1, action: "set_icon", data: { icon: "🌱" }, expected_revision: 1 });
    expect(threadState.threads[1].icon).toBeNull();
    flush(() => handleThreadMessage({ type: "thread_upsert", thread: makeThread(1, { title: "Garden", icon: "🌱", revision: 2 }) }));
    expect(view.container.querySelector('header [data-thread-avatar="1"]')).toHaveTextContent("🌱");
    expect(view.container.querySelector('[data-thread-row="1"] [data-thread-avatar]')).toHaveTextContent("🌱");
    expect(threadState.focusedId).toBe(1);
  });
  it("sends custom joined emoji exactly and resets explicitly to null", () => {
    const view = render(() => <ThreadShell />);
    flush(() => openThreadIconPicker(threadState.threads.find(thread => thread.id === 2)!));
    fireEvent.input(view.getByRole("textbox", { name: "Custom emoji or symbol" }), { target: { value: "👩🏽‍💻" } });
    fireEvent.click(view.getByRole("button", { name: "Save icon" }));
    expect(sent.at(-1)).toMatchObject({ thread_id: 2, data: { icon: "👩🏽‍💻" }, expected_revision: 1 });
    flush(() => handleThreadMessage({ type: "thread_upsert", thread: makeThread(2, { title: "Tools", icon: "👩🏽‍💻", revision: 2 }) }));
    flush(() => openThreadIconPicker(threadState.threads.find(thread => thread.id === 2)!));
    fireEvent.click(view.getByRole("button", { name: "Use default" }));
    expect(view.getByRole("dialog", { name: "Change thread icon" }).querySelector('[data-thread-avatar]')).toHaveTextContent("T");
    fireEvent.click(view.getByRole("button", { name: "Save icon" }));
    expect(sent.at(-1)).toMatchObject({ thread_id: 2, data: { icon: null }, expected_revision: 2 });
  });
  it("keeps the edit revision fixed and exposes rejected requests through the existing error surface", () => {
    const view = render(() => <ThreadShell />);
    flush(() => openThreadIconPicker(threadState.threads[1]));
    flush(() => handleThreadMessage({ type: "thread_upsert", thread: makeThread(1, { title: "Garden", revision: 2 }) }));
    fireEvent.click(view.getByRole("button", { name: "Rocket" }));
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
  it("rejects invalid input and disables save while disconnected", () => {
    const view = render(() => <ThreadShell />);
    flush(() => openThreadIconPicker(threadState.threads[1]));
    fireEvent.input(view.getByRole("textbox", { name: "Custom emoji or symbol" }), { target: { value: "x".repeat(17) } });
    expect(view.getByRole("button", { name: "Save icon" })).toBeDisabled();
    expect(view.getByRole("alert")).toHaveTextContent("16 characters");
    fireEvent.click(view.getByRole("button", { name: "Use default" }));
    flush(() => dispatch({ type: "connection_status", status: "reconnecting" }));
    expect(view.getByRole("button", { name: "Save icon" })).toBeDisabled();
  });
  it("keeps an ID's fallback color stable across title and lifecycle updates", () => {
    const view = render(() => <ThreadAvatar thread={threadState.threads[1]} />);
    const avatar = view.container.firstElementChild!;
    const before = avatar.className;
    flush(() => handleThreadMessage({ type: "thread_upsert", thread: makeThread(1, { title: "Orchard", settled_at: "2026-09-10T10:00:00Z", revision: 2 }) }));
    expect(avatar.className).toBe(before);
    expect(avatar).toHaveTextContent("O");
    expect(avatar).toHaveAttribute("aria-hidden", "true");
  });
  it("accepts Unicode emoji composition but excludes controls and line separators", () => {
    for (const icon of [null, "🌱", "👩🏽‍💻", "⭐".repeat(16)]) expect(threadIconError(icon)).toBeNull();
    for (const icon of ["", "   ", "x\n", "x\u0085", "x\u2028", "x\u2029", "x".repeat(17)]) expect(threadIconError(icon)).not.toBeNull();
  });
});
