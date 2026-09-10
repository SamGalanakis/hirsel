import { flush } from "solid-js";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// A scriptable WebSocket stand-in (jsdom has no live socket). The client only
// uses addEventListener/removeEventListener/send/close + the static readyState
// enum, so this reproduces that surface and exposes `server*` helpers to drive
// the host side of the conversation.
type Listener = (ev: unknown) => void;

class FakeWebSocket {
  static readonly CONNECTING = 0;
  static readonly OPEN = 1;
  static readonly CLOSING = 2;
  static readonly CLOSED = 3;
  static instances: FakeWebSocket[] = [];

  url: string;
  readyState = FakeWebSocket.CONNECTING;
  sent: string[] = [];
  private listeners: Record<string, Listener[]> = {};

  constructor(url: string) {
    this.url = url;
    FakeWebSocket.instances.push(this);
  }

  addEventListener(type: string, cb: Listener) {
    (this.listeners[type] ??= []).push(cb);
  }
  removeEventListener(type: string, cb: Listener) {
    this.listeners[type] = (this.listeners[type] ?? []).filter((f) => f !== cb);
  }
  send(data: string) {
    this.sent.push(data);
  }
  close(code = 1000) {
    if (this.readyState === FakeWebSocket.CLOSED) return;
    this.readyState = FakeWebSocket.CLOSED;
    this.emit("close", { code });
  }

  // ---- host-side driver ----
  private emit(type: string, ev: unknown) {
    flush(() => { for (const cb of this.listeners[type] ?? []) cb(ev); });
  }
  serverOpen() {
    this.readyState = FakeWebSocket.OPEN;
    this.emit("open", {});
  }
  serverSend(obj: unknown) {
    this.emit("message", { data: JSON.stringify(obj) });
  }
  serverError() {
    this.emit("error", {});
  }
  serverClose(code: number) {
    this.readyState = FakeWebSocket.CLOSED;
    this.emit("close", { code });
  }

  sentTypes(): string[] {
    return this.sent.map((s) => (JSON.parse(s) as { type: string }).type);
  }
}

const HELLO_OK = { type: "hello_ok", history_id: "test-history", threads: [], processes: [], views: [], host_version: "test", model: null, subagent_models: null, prompts: null, providers: null } as const;

// The client reads/writes the unqualified global `localStorage`, which in this
// runner is Node's experimental Web Storage (no valid path ⇒ its methods are
// unusable). Stub a plain in-memory Storage so the token get/set/clear paths run.
const memStore = new Map<string, string>();
const memLocalStorage: Storage = {
  getItem: (k) => (memStore.has(k) ? (memStore.get(k) as string) : null),
  setItem: (k, v) => void memStore.set(k, String(v)),
  removeItem: (k) => void memStore.delete(k),
  clear: () => memStore.clear(),
  key: (i) => [...memStore.keys()][i] ?? null,
  get length() {
    return memStore.size;
  },
};

let originalWebSocket: unknown;

beforeEach(() => {
  vi.resetModules();
  FakeWebSocket.instances = [];
  originalWebSocket = (globalThis as { WebSocket?: unknown }).WebSocket;
  (globalThis as { WebSocket: unknown }).WebSocket = FakeWebSocket;
  memStore.clear();
  vi.stubGlobal("localStorage", memLocalStorage);
});

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
  (globalThis as { WebSocket: unknown }).WebSocket = originalWebSocket;
});

async function load() {
  const store = await import("../store/store");
  const client = await import("./client");
  return { store, client };
}

describe("HirselWsClient lifecycle", () => {
  it("keeps artifact retrieval errors out of the conversation and clears them after retry", async () => {
    const { client } = await load();
    const artifacts = await import("../artifacts/store");
    const threads = await import("../threads/store");
    client.startClient("wss://host/ws", "good");
    const ws = FakeWebSocket.instances[0]; ws.serverOpen(); ws.serverSend(HELLO_OK);
    artifacts.openArtifact(4); flush();
    const request = JSON.parse(ws.sent.at(-1)!);
    ws.serverSend({ type: "error", client_id: request.client_id, detail: "Artifact could not be loaded" });
    expect(artifacts.artifactState.error).toBe("Artifact could not be loaded");
    expect(threads.threadState.error).toBeNull();
    artifacts.openArtifact(4); flush();
    ws.serverSend({ type: "artifact_opened", client_id: JSON.parse(ws.sent.at(-1)!).client_id, artifact: { id: 4, title: "Plan", kind: "html", mime: "text/html", content: "Working", thread_ids: [0], created_at: "a", updated_at: "b" } });
    expect(artifacts.artifactState.error).toBeNull();
    expect(artifacts.artifactState.opened?.content).toBe("Working");
    expect(threads.threadState.error).toBeNull();
    threads.sendThreadMessage(0, "Follow up", "send", [], [], []); flush();
    const send = JSON.parse(ws.sent.at(-1)!);
    ws.serverSend({ type: "error", client_id: send.client_id, detail: "Thread send failed" });
    expect(threads.threadState.error).toMatchObject({ operation: "send", detail: "Thread send failed" });
    expect(threads.threadState.pending[0].failed).toBe(true);
    ws.serverSend({ type: "error", detail: "Connection runtime failed" });
    expect(threads.threadState.error).toMatchObject({ operation: "request", detail: "Connection runtime failed" });
    client.getClient()?.close();
  });
  it("opens, sends hello, and reaches connected on hello_ok", async () => {
    const { store, client } = await load();
    client.startClient("wss://host/ws", "good");
    const ws = FakeWebSocket.instances[0];
    expect(store.state.connection).toBe("connecting");

    ws.serverOpen();
    expect(ws.sentTypes()).toContain("hello");

    ws.serverSend(HELLO_OK);
    expect(store.state.connection).toBe("connected");
  });
});

describe("HirselWsClient signed blob URLs (D9)", () => {
  function connected(client: Awaited<ReturnType<typeof load>>["client"]) {
    const c = client.startClient("wss://host.example/ws", "good");
    const ws = FakeWebSocket.instances[0];
    ws.serverOpen();
    ws.serverSend(HELLO_OK);
    return { c, ws };
  }
  function lastGetBlobUrl(ws: FakeWebSocket): { type: string; client_id: string; blob_id: string } {
    return ws.sent.map((s) => JSON.parse(s)).find((f) => f.type === "get_blob_url");
  }

  it("resolves get_blob_url with an absolute, origin-prefixed signed URL", async () => {
    const { client } = await load();
    const { c, ws } = connected(client);

    const p = c.getBlobUrl("blob-1");
    const frame = lastGetBlobUrl(ws);
    expect(frame.blob_id).toBe("blob-1");

    ws.serverSend({
      type: "blob_url",
      client_id: frame.client_id,
      blob_id: "blob-1",
      url: "/blob/blob-1?exp=123&sig=abc",
      expires_at: 123,
    });
    await expect(p).resolves.toBe("https://host.example/blob/blob-1?exp=123&sig=abc");
  });

  it("rejects the request when the host returns a correlated error", async () => {
    const { client } = await load();
    const { c, ws } = connected(client);

    const p = c.getBlobUrl("gone");
    const frame = lastGetBlobUrl(ws);
    ws.serverSend({ type: "error", detail: "no such blob", client_id: frame.client_id });

    await expect(p).rejects.toThrow(/no such blob/);
  });
});



describe("HirselWsClient auth rejection (C5)", () => {
  it("keeps reconnecting when refused connections never reach open", async () => {
    vi.useFakeTimers();
    const { store, client } = await load();
    localStorage.setItem("hirsel.token", "good");
    const onAuthReject = vi.fn();
    client.startClient("wss://host/ws", "good", { onAuthReject });

    // A refused connection emits error then close without ever reaching open.
    FakeWebSocket.instances[0].serverError();
    vi.advanceTimersByTime(2000);
    flush();
    FakeWebSocket.instances[1].serverError();
    vi.advanceTimersByTime(4000);
    flush();

    expect(onAuthReject).not.toHaveBeenCalled();
    expect(client.getStoredToken()).toBe("good");
    expect(store.state.connection).toBe("reconnecting");
    expect(FakeWebSocket.instances.length).toBeGreaterThanOrEqual(3);
  });

  it("clears the token and routes to the gate on an auth-reject close code", async () => {
    const { client } = await load();
    localStorage.setItem("hirsel.token", "bad");
    const onAuthReject = vi.fn();
    client.startClient("wss://host/ws", "bad", { onAuthReject });
    const ws = FakeWebSocket.instances[0];
    ws.serverOpen();
    ws.serverClose(1008); // policy-violation: token rejected

    expect(onAuthReject).toHaveBeenCalledOnce();
    expect(onAuthReject.mock.calls[0][0]).toMatch(/authenticate/i);
    expect(client.getStoredToken()).toBeNull();
    // No reconnect: only ever the one socket.
    expect(FakeWebSocket.instances).toHaveLength(1);
  });

  it("gates immediately on a pre-auth error frame with no client_id (the real host signal)", async () => {
    const { client } = await load();
    localStorage.setItem("hirsel.token", "bad");
    const onAuthReject = vi.fn();
    client.startClient("wss://host/ws", "bad", { onAuthReject });
    const ws = FakeWebSocket.instances[0];
    ws.serverOpen();
    // Host rejects the hello: an error frame (no client_id), then a plain close.
    ws.serverSend({ type: "error", detail: "invalid hello: bad token" });

    expect(onAuthReject).toHaveBeenCalledOnce();
    // The host's reason is surfaced verbatim.
    expect(onAuthReject.mock.calls[0][0]).toBe("invalid hello: bad token");
    expect(client.getStoredToken()).toBeNull();

    // The subsequent plain close must not schedule a reconnect.
    ws.serverClose(1000);
    expect(FakeWebSocket.instances).toHaveLength(1);
  });

  it("does NOT treat a post-auth error frame as an auth rejection", async () => {
    const { client } = await load();
    localStorage.setItem("hirsel.token", "good");
    const onAuthReject = vi.fn();
    const c = client.startClient("wss://host/ws", "good", { onAuthReject });
    const ws = FakeWebSocket.instances[0];
    ws.serverOpen();
    ws.serverSend(HELLO_OK); // authenticated
    void c;

    // A global error after authentication is a runtime error, never a gate-out.
    ws.serverSend({ type: "error", detail: "something went wrong" });
    expect(onAuthReject).not.toHaveBeenCalled();
    expect(client.getStoredToken()).toBe("good"); // token untouched
  });


  it("gates when a reconnect receives a pre-auth error after an earlier success", async () => {
    vi.useFakeTimers();
    const { client } = await load();
    localStorage.setItem("hirsel.token", "rotated");
    const onAuthReject = vi.fn();
    client.startClient("wss://host/ws", "rotated", { onAuthReject });
    const ws1 = FakeWebSocket.instances[0];
    ws1.serverOpen();
    ws1.serverSend(HELLO_OK);

    ws1.serverClose(1006);
    vi.advanceTimersByTime(2000);
    flush();
    const ws2 = FakeWebSocket.instances[1];
    ws2.serverOpen();
    ws2.serverSend({ type: "error", detail: "invalid hello: rotated token" });

    expect(onAuthReject).toHaveBeenCalledOnce();
    expect(onAuthReject.mock.calls[0][0]).toBe("invalid hello: rotated token");
    expect(client.getStoredToken()).toBeNull();

    ws2.serverClose(1000);
    vi.runAllTimers();
    expect(onAuthReject).toHaveBeenCalledOnce();
    expect(FakeWebSocket.instances).toHaveLength(2);
  });


  it("does NOT gate a mid-session drop once the token has authenticated", async () => {
    vi.useFakeTimers();
    const { store, client } = await load();
    const onAuthReject = vi.fn();
    client.startClient("wss://host/ws", "good", { onAuthReject });
    const ws1 = FakeWebSocket.instances[0];
    ws1.serverOpen();
    ws1.serverSend(HELLO_OK); // authenticated once — heuristic now disabled

    // Two pre-hello closes in a row afterwards must keep reconnecting, never gate.
    ws1.serverClose(1006);
    vi.advanceTimersByTime(2000);
    flush();
    FakeWebSocket.instances[1].serverClose(1006);
    vi.advanceTimersByTime(4000);
    flush();

    expect(onAuthReject).not.toHaveBeenCalled();
    expect(store.state.connection).toBe("reconnecting");
    expect(FakeWebSocket.instances.length).toBeGreaterThanOrEqual(3);
  });
});

describe("current history boundary", () => {
  it("sends only tagged auth and waits for hello before sending requests", async () => {
    const { client } = await load();
    const c = client.startClient("wss://host/ws", "good");
    const ws = FakeWebSocket.instances[0]; ws.serverOpen();
    expect(JSON.parse(ws.sent[0])).toEqual({ type: "hello", auth: { static_token: "good" } });
    c.setAgentPrompt("New prompt");
    expect(ws.sentTypes()).toEqual(["hello"]);
    ws.serverSend(HELLO_OK);
    expect(ws.sentTypes()).toContain("set_agent_prompt"); c.close();
  });
  it("replays an unacknowledged Thread send only when history identity matches", async () => {
    vi.useFakeTimers(); const { client } = await load();
    const threads = await import("../threads/store");
    const c = client.startClient("wss://host/ws", "good"); const first = FakeWebSocket.instances[0]; first.serverOpen(); first.serverSend(HELLO_OK);
    flush(() => threads.sendThreadMessage(4,"Retain me","send",[],[], []));
    const sent = JSON.parse(first.sent.find(row=>JSON.parse(row).type==="send_thread_message")!);
    first.serverClose(1006); vi.advanceTimersByTime(2000); const same = FakeWebSocket.instances[1]; same.serverOpen();
    expect(same.sentTypes()).not.toContain("send_thread_message"); same.serverSend(HELLO_OK);
    expect(same.sent.map(row=>JSON.parse(row))).toContainEqual(sent); c.close();
  });
  it("reconciles a turn that finishes while a same-history connection is offline", async () => {
    vi.useFakeTimers();
    const { client } = await load();
    const threads = await import("../threads/store");
    const { conversationEntries } = await import("../threads/conversation");
    const { makeThread } = await import("../threads/fixtures");
    const owner = { id: 10, thread_id: 1, author: "owner" as const, body: "Finish this", ref: null, ts: "2026-09-10T10:00:00Z" };
    const running = { requester_thread_id: null, requester_turn_id: null, id: 1, thread_id: 1, state: "running" as const, owner_message_id: owner.id, agent_message_id: null, started_at: owner.ts, finished_at: null };
    const thread = makeThread(1, { running_turn: running });
    const c = client.startClient("wss://host/ws", "good");
    const first = FakeWebSocket.instances[0];
    first.serverOpen(); first.serverSend({ ...HELLO_OK, threads: [thread] });
    flush(() => threads.focusThread(1, false));
    const firstOpen = first.sent.map(row => JSON.parse(row)).find(frame => frame.type === "open_thread");
    first.serverSend({ type: "thread_opened", client_id: firstOpen.client_id, detail: { thread, brief: { text: "", artifact_ids: [] }, messages: [owner], turns: [running], activities: [], related_items: [], has_more: false } });
    expect(threads.threadState.histories[1].turns[0].state).toBe("running");

    first.serverClose(1006);
    vi.advanceTimersByTime(2_000);
    const reconnected = FakeWebSocket.instances[1];
    reconnected.serverOpen(); reconnected.serverSend({ ...HELLO_OK, threads: [makeThread(1)] });
    const reconnectOpen = reconnected.sent.map(row => JSON.parse(row)).find(frame => frame.type === "open_thread");
    const final = { id: 11, thread_id: 1, author: "agent" as const, body: "Done", ref: owner.id, ts: "2026-09-10T10:00:05Z" };
    const completed = { ...running, state: "completed" as const, agent_message_id: final.id, finished_at: final.ts };
    reconnected.serverSend({ type: "thread_opened", client_id: reconnectOpen.client_id, detail: { thread: makeThread(1, { last_finished_turn: completed }), brief: { text: "", artifact_ids: [] }, messages: [owner, final], turns: [completed], activities: [], related_items: [], has_more: false } });

    expect(threads.threadState.histories[1].turns).toEqual([completed]);
    expect(conversationEntries(threads.threadState.histories[1])).toContainEqual({ key: "turn-1", kind: "message", message: final, turn: completed });
    c.close();
  });
  it("clears stale cache, uploads and queued operations on reset while retaining unsent text for recovery", async () => {
    vi.useFakeTimers(); const { client } = await load();
    const threads = await import("../threads/store"); const artifacts = await import("../artifacts/store");
    const c = client.startClient("wss://host/ws", "good"); const first = FakeWebSocket.instances[0]; first.serverOpen(); first.serverSend(HELLO_OK);
    flush(() => { threads.sendThreadMessage(4,"Saved unsent text","send",[],[], []); threads.setThreadState(draft=>{ draft.focusedId=4; draft.histories[4] = { brief: { text: "Old", artifact_ids: [] }, messages: [], turns: [], activities: [], hasMore: false, loaded: true }; }); artifacts.setArtifactState({ selectedId: 2 }); });
    const upload = c.uploadBlob("upload-old","old.txt","text/plain","eA==").catch(error=>error.message);
    first.serverClose(1006); c.setAgentPrompt("Stale queued operation"); vi.advanceTimersByTime(2000);
    const next = FakeWebSocket.instances[1]; next.serverOpen(); next.serverSend({ ...HELLO_OK, history_id: "fresh-history" }); await Promise.resolve(); flush();
    expect(next.sentTypes()).not.toContain("send_thread_message"); expect(next.sentTypes()).not.toContain("set_agent_prompt");
    expect(threads.threadState.pending).toEqual([]); expect(threads.threadState.focusedId).toBeNull(); expect(threads.threadState.error).toBeNull();
    expect(threads.threadState.histories).toEqual({});
    expect(artifacts.artifactState.selectedId).toBeNull(); expect(await upload).toContain("History was reset");
    const { recoveredDrafts } = await import("../lib/history"); expect(recoveredDrafts().map(row=>row.text)).toContain("Saved unsent text"); c.close();
  });
});
