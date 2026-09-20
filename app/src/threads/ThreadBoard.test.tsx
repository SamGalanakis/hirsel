import { cleanup, fireEvent, render } from "@solidjs/testing-library";
import { createSignal, flush } from "solid-js";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { setHistoryId } from "../lib/history";
import { makeThread } from "./fixtures";
import { attachThreadTransport, disconnectThreads, setThreadState } from "./store";
import type { ThreadClientMessage } from "./types";
import { ThreadBoard } from "./ThreadBoard";

const frames: ThreadClientMessage[] = [];
beforeEach(() => {
  frames.length = 0;
  setHistoryId("board-history");
  flush(() => setThreadState(draft => {
    draft.ready = true;
    draft.threads = [
      makeThread(1, { kind: "space", title: "Launch", read: true }),
      makeThread(2, { kind: "task", parent_thread_id: 1, title: "Decision", attention: "needs_owner", status: { kind: "needs_you", reason: "Waiting for your input" }, instrument: [{ type: "eyebrow", text: "Decision" }, { type: "heading", text: "Ship on Friday?" }] }),
      makeThread(3, { kind: "task", parent_thread_id: 1, title: "Build", previous_headline: "Compiling", headline: "Tests pass", headline_revision: 2, last_seen_headline_revision: 1 }),
      makeThread(4, { kind: "space", title: "Other" }),
    ];
  }));
  attachThreadTransport(frame => frames.push(frame));
});
afterEach(() => { cleanup(); disconnectThreads(); });

describe("Space board", () => {
  it("shows the three client-derived bands and the instrument question", () => {
    const view = render(() => <ThreadBoard spaceId={1} visible={() => false} />);
    expect(view.getByRole("heading", { name: "Needs you" })).toBeInTheDocument();
    expect(view.getByText("Ship on Friday?")).toBeInTheDocument();
    expect(view.getByRole("heading", { name: "Changed since you looked" })).toBeInTheDocument();
    expect(view.getByText("Compiling → Tests pass")).toBeInTheDocument();
    expect(view.queryByText("Other")).toBeNull();
  });

  it("marks changed rows seen only after the board is actually displayed", () => {
    const [visible, setVisible] = createSignal(false);
    render(() => <ThreadBoard spaceId={1} visible={visible} />);
    expect(frames.filter(frame => frame.type === "mark_thread_headlines_seen")).toHaveLength(0);
    flush(() => setVisible(true));
    expect(frames.filter(frame => frame.type === "mark_thread_headlines_seen")).toEqual([
      expect.objectContaining({ history_id: "board-history", thread_ids: [3] }),
    ]);
  });

  it("keeps row placement while either pointer or focus remains inside", () => {
    const view = render(() => <ThreadBoard spaceId={1} visible={() => false} />);
    const board = view.getByRole("complementary", { name: "Space board" });
    const ids = () => [...view.container.querySelectorAll<HTMLElement>("[data-board-thread]")].map(row => row.dataset.boardThread);
    expect(ids()).toEqual(["2", "3", "1"]);
    fireEvent.pointerEnter(board);
    fireEvent.focusIn(view.container.querySelector('[data-board-thread="3"]')!);
    flush(() => setThreadState(draft => { const row = draft.threads.find(thread => thread.id === 1)!; row.attention = "needs_owner"; row.status = { kind: "needs_you", reason: "Waiting for your input" }; }));
    expect(ids()).toEqual(["2", "3", "1"]);
    fireEvent.pointerLeave(board);
    expect(ids()).toEqual(["2", "3", "1"]);
    fireEvent.focusOut(view.container.querySelector('[data-board-thread="3"]')!, { relatedTarget: document.body });
    expect(ids()).toEqual(["1", "2", "3"]);
  });
});
