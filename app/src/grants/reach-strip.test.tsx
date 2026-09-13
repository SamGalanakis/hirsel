import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { flush } from "solid-js";
import { setHistoryId } from "../lib/history";
import { makeThread } from "../threads/fixtures";
import { disconnectThreads, setThreadState } from "../threads/store";
import type { ThreadClientMessage, ThreadGrant } from "../threads/types";
import { ReachStrip } from "./ReachStrip";
import { attachGrantTransport, disconnectGrants, handleGrantMessage, reachSummary, resetGrants } from "./store";

const historyId = "ab123456-1234-5678-9abc-123456789abc";
const frames: ThreadClientMessage[] = [];
const grant = (patch: Partial<ThreadGrant> = {}): ThreadGrant => ({ thread_id: 1, target_thread_id: 2, title: "Billing", kind: "space", granted_by: { kind: "owner" }, granted_at: "2026-09-13T10:00:00Z", note: null, ...patch });
const origin = { historyId, threadId: 1 };

beforeEach(() => {
  frames.length = 0;
  flush(() => {
    setHistoryId(historyId);
    resetGrants();
    setThreadState(draft => { draft.ready = true; draft.threads = [makeThread(1), makeThread(2, { title: "Billing" }), makeThread(3, { title: "Renewals" })]; draft.focusedId = 1; });
  });
  attachGrantTransport(frame => frames.push(frame));
});
afterEach(() => { cleanup(); disconnectGrants(); disconnectThreads(); });

describe("Thread reach strip", () => {
  it("shows default reach and every grant on one line", () => {
    render(() => <ReachStrip origin={origin} />);
    const strip = screen.getByRole("region", { name: "Thread reach" });
    expect(strip.textContent).toContain("Reach:self + subtree");
    flush(() => handleGrantMessage({ type: "thread_grants_changed", client_id: null, history_id: historyId, thread_id: 1, revision: 2, grants: [grant()] }));
    expect(strip.textContent).toContain('+Space #2 "Billing"');
    expect(reachSummary(1)).toBe('self + subtree · +Space #2 "Billing"');
  });

  it("lets the Owner grant a Thread and revoke it again", async () => {
    render(() => <ReachStrip origin={origin} />);
    fireEvent.click(screen.getByRole("button", { name: "Add reach to another Thread" }));
    fireEvent.click(screen.getByRole("button", { name: /Billing/ }));
    expect(frames[0]).toMatchObject({ type: "grant_thread_reach", history_id: historyId, thread_id: 1, target_thread_id: 2, note: null });
    flush(() => handleGrantMessage({ type: "thread_grants_changed", client_id: (frames[0] as { client_id: string }).client_id, history_id: historyId, thread_id: 1, revision: 2, grants: [grant()] }));
    const remove = await waitFor(() => {
      const button = screen.getByRole("button", { name: "Remove reach to Thread 2" });
      if (button.hasAttribute("disabled")) throw new Error("still sending");
      return button;
    });

    fireEvent.click(remove);
    expect(frames[1]).toMatchObject({ type: "revoke_thread_reach", thread_id: 1, target_thread_id: 2 });
    flush(() => handleGrantMessage({ type: "thread_grants_changed", client_id: (frames[1] as { client_id: string }).client_id, history_id: historyId, thread_id: 1, revision: 3, grants: [] }));
    expect(screen.queryByRole("button", { name: "Remove reach to Thread 2" })).toBeNull();
  });

  it("never offers a Thread that is already reachable or is the Thread itself", () => {
    flush(() => handleGrantMessage({ type: "thread_grants_changed", client_id: null, history_id: historyId, thread_id: 1, revision: 2, grants: [grant()] }));
    render(() => <ReachStrip origin={origin} />);
    fireEvent.click(screen.getByRole("button", { name: "Add reach to another Thread" }));
    expect(screen.queryByRole("button", { name: /#2/ })).toBeNull();
    expect(screen.queryByRole("button", { name: /#1/ })).toBeNull();
    expect(screen.getByRole("button", { name: /Renewals/ })).toBeTruthy();
  });

  it("keeps a reach snapshot from rolling back behind newer Thread metadata", () => {
    flush(() => handleGrantMessage({ type: "thread_grants_changed", client_id: null, history_id: historyId, thread_id: 1, revision: 5, grants: [grant()] }));
    flush(() => handleGrantMessage({ type: "thread_grants_changed", client_id: null, history_id: historyId, thread_id: 1, revision: 3, grants: [] }));
    expect(reachSummary(1)).toContain("+Space #2");
    flush(() => handleGrantMessage({ type: "thread_grants_changed", client_id: null, history_id: "other-history", thread_id: 1, revision: 9, grants: [] }));
    expect(reachSummary(1)).toContain("+Space #2");
  });
});
