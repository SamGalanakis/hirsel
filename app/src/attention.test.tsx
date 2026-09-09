import { flush } from "solid-js";
import { render, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";
import App from "./App";
import { makeThread } from "./threads/fixtures";
import { setThreadState } from "./threads/store";
vi.mock("./ws/client", () => ({ getStoredToken: () => "test", setStoredToken: vi.fn(), startClient: () => ({ close: vi.fn() }), getClient: () => null, makeClientId: () => "test" }));
afterEach(() => flush(() => setThreadState(draft => { Object.assign(draft, { threads: [], focusedId: 0 }); })));
describe("thread attention", () => {
  it("counts pending attention independently of unread and drops only after attention clears or settlement", async () => {
    flush(() => setThreadState(draft => { draft["threads"] = [makeThread(1, { attention: "needs_owner", read: true }), makeThread(2)]; }));
    render(() => <App />);
    await waitFor(() => expect(document.title).toBe("(1) hirsel"));
    flush(() => setThreadState(draft => { draft["threads"] = [makeThread(1, { attention: "quiet", read: false, revision: 2 })]; }));
    await waitFor(() => expect(document.title).toBe("hirsel"));
    flush(() => setThreadState(draft => { draft["threads"] = [makeThread(1, { attention: "needs_owner", settled_at: "2026-09-09T10:00:00Z", revision: 3 })]; }));
    expect(document.title).toBe("hirsel");
  });
});
