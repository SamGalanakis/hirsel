import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { flush } from "solid-js";
import { setHistoryId } from "../lib/history";
import { makeThread } from "../threads/fixtures";
import { disconnectThreads, setThreadState, threadState } from "../threads/store";
import type { ThreadClientMessage, ThreadGrant } from "../threads/types";
import { ReachDialog } from "./ReachDialog";
import { closeThreadReach, openThreadReach } from "./reach";
import { attachGrantTransport, disconnectGrants, handleGrantMessage, holdsRoot, reachSummary, resetGrants } from "./store";

const historyId = "ab123456-1234-5678-9abc-123456789abc";
const frames: ThreadClientMessage[] = [];
const grant = (patch: Partial<ThreadGrant> = {}): ThreadGrant => ({ thread_id: 1, target: { kind: "thread", thread_id: 2, title: "Billing", thread_kind: "task" }, granted_by: { kind: "owner" }, granted_at: "2026-09-13T10:00:00Z", note: null, ...patch });
const rootGrant = (): ThreadGrant => grant({ target: { kind: "root" } });
const changed = (revision: number, grants: ThreadGrant[], clientId: string | null = null) =>
  handleGrantMessage({ type: "thread_grants_changed", client_id: clientId, history_id: historyId, thread_id: 1, revision, grants });
const open = () => flush(() => openThreadReach(threadState.threads.find(thread => thread.id === 1)!));

beforeEach(() => {
  frames.length = 0;
  flush(() => {
    setHistoryId(historyId);
    resetGrants();
    closeThreadReach();
    setThreadState(draft => { draft.ready = true; draft.threads = [makeThread(1, { title: "lash", kind: "space" }), makeThread(2, { title: "Billing" }), makeThread(3, { title: "Renewals" })]; draft.focusedId = 1; });
  });
  attachGrantTransport(frame => frames.push(frame));
});
afterEach(() => { cleanup(); closeThreadReach(); disconnectGrants(); disconnectThreads(); });

describe("Thread reach dialog", () => {
  it("names the Thread and lists default reach with every grant", () => {
    render(() => <ReachDialog />);
    open();
    const dialog = screen.getByRole("dialog", { name: "Reach of Space #1 “lash”" });
    expect(dialog.textContent).toContain("Itself and everything below it");
    flush(() => changed(2, [grant()]));
    expect(dialog.textContent).toContain('Task #2 "Billing"');
    expect(reachSummary(1)).toBe('self + subtree · +Task #2 "Billing"');
  });

  it("lets the Owner grant a Thread and revoke it again", async () => {
    render(() => <ReachDialog />);
    open();
    fireEvent.click(screen.getByRole("button", { name: /Billing/ }));
    expect(frames[0]).toMatchObject({ type: "grant_thread_reach", history_id: historyId, thread_id: 1, target: 2, note: null });
    flush(() => changed(2, [grant()], (frames[0] as { client_id: string }).client_id));
    const remove = await waitFor(() => {
      const button = screen.getByRole("button", { name: "Remove reach to Thread 2" });
      if (button.hasAttribute("disabled")) throw new Error("still sending");
      return button;
    });

    fireEvent.click(remove);
    expect(frames[1]).toMatchObject({ type: "revoke_thread_reach", thread_id: 1, target: 2 });
    flush(() => changed(3, [], (frames[1] as { client_id: string }).client_id));
    expect(screen.queryByRole("button", { name: "Remove reach to Thread 2" })).toBeNull();
  });

  it("offers everything first and holds root as one row, with no picker left", async () => {
    render(() => <ReachDialog />);
    open();
    const options = screen.getAllByRole("button").filter(button => button.textContent?.trim());
    expect(options[0].textContent).toContain("Everything (root)");

    fireEvent.click(options[0]);
    expect(frames[0]).toMatchObject({ type: "grant_thread_reach", thread_id: 1, target: "root", note: null });
    flush(() => changed(2, [rootGrant()], (frames[0] as { client_id: string }).client_id));
    expect(holdsRoot(1)).toBe(true);
    expect(reachSummary(1)).toBe("everything (root)");
    expect(screen.queryByLabelText("Add reach to another Thread")).toBeNull();

    const remove = await waitFor(() => {
      const button = screen.getByRole("button", { name: "Remove reach to everything" });
      if (button.hasAttribute("disabled")) throw new Error("still sending");
      return button;
    });
    fireEvent.click(remove);
    expect(frames[1]).toMatchObject({ type: "revoke_thread_reach", thread_id: 1, target: "root" });
  });

  it("submits the first offer on Enter and never offers a held or self Thread", () => {
    flush(() => changed(2, [grant()]));
    render(() => <ReachDialog />);
    open();
    expect(screen.queryByRole("button", { name: /#2/ })).toBeNull();
    expect(screen.queryByRole("button", { name: /#1/ })).toBeNull();
    expect(screen.getByRole("button", { name: /Renewals/ })).toBeTruthy();

    const search = screen.getByLabelText("Add reach to another Thread");
    fireEvent.input(search, { target: { value: "Renewals" } });
    fireEvent.keyDown(search, { key: "Enter" });
    expect(frames[0]).toMatchObject({ type: "grant_thread_reach", thread_id: 1, target: 3 });
  });

  it("keeps an offer on one line so a long detail never squeezes the name", () => {
    render(() => <ReachDialog />);
    open();
    const root = screen.getAllByRole("button").find(button => button.textContent?.includes("Everything (root)"))!;
    const label = root.querySelector<HTMLElement>('[title="Everything (root)"]')!;
    const detail = root.querySelector<HTMLElement>('[title^="every Thread"]')!;
    // The name truncates rather than wrapping letter-by-letter into a column.
    expect(label.className).toContain("truncate");
    expect(label.className).not.toContain("wrap-break-word");
    // The detail is capped, so it can never take the whole row from the name.
    expect(detail.className).toContain("truncate");
    expect(detail.className).toContain("max-w-[50%]");
  });

  it("keeps a reach snapshot from rolling back behind newer Thread metadata", () => {
    flush(() => changed(5, [grant()]));
    flush(() => changed(3, []));
    expect(reachSummary(1)).toContain('+Task #2');
    flush(() => handleGrantMessage({ type: "thread_grants_changed", client_id: null, history_id: "other-history", thread_id: 1, revision: 9, grants: [] }));
    expect(reachSummary(1)).toContain('+Task #2');
  });
});
