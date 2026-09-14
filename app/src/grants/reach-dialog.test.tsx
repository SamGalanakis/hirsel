import { cleanup } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { flush } from "solid-js";
import { setHistoryId } from "../lib/history";
import { makeThread } from "../threads/fixtures";
import { disconnectThreads, setThreadState } from "../threads/store";
import type { ThreadClientMessage, ThreadGrant } from "../threads/types";
import { closeThreadReach } from "./reach";
import { attachGrantTransport, disconnectGrants, handleGrantMessage, reachSummary, resetGrants } from "./store";

const historyId = "ab123456-1234-5678-9abc-123456789abc";
const frames: ThreadClientMessage[] = [];
const grant = (patch: Partial<ThreadGrant> = {}): ThreadGrant => ({ thread_id: 1, target: { kind: "thread", thread_id: 2, title: "Billing", thread_kind: "task" }, granted_by: { kind: "owner" }, granted_at: "2026-09-13T10:00:00Z", note: null, ...patch });
const changed = (revision: number, grants: ThreadGrant[], clientId: string | null = null) =>
  handleGrantMessage({ type: "thread_grants_changed", client_id: clientId, history_id: historyId, thread_id: 1, revision, grants });

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
  it("keeps a reach snapshot from rolling back behind newer Thread metadata", () => {
    flush(() => changed(5, [grant()]));
    flush(() => changed(3, []));
    expect(reachSummary(1)).toContain('+Task #2');
    flush(() => handleGrantMessage({ type: "thread_grants_changed", client_id: null, history_id: "other-history", thread_id: 1, revision: 9, grants: [] }));
    expect(reachSummary(1)).toContain('+Task #2');
  });
});
