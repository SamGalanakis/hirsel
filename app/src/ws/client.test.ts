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
    for (const cb of this.listeners[type] ?? []) cb(ev);
  }
  serverOpen() {
    this.readyState = FakeWebSocket.OPEN;
    this.emit("open", {});
  }
  serverSend(obj: unknown) {
    this.emit("message", { data: JSON.stringify(obj) });
  }
  serverClose(code: number) {
    this.readyState = FakeWebSocket.CLOSED;
    this.emit("close", { code });
  }

  sentTypes(): string[] {
    return this.sent.map((s) => (JSON.parse(s) as { type: string }).type);
  }
}

const HELLO_OK = { type: "hello_ok", latest_msg_id: 0, messages: [], pings: [] } as const;

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

  it("reconnects with backoff and flushes pending sends on the new socket", async () => {
    vi.useFakeTimers();
    const { store, client } = await load();
    const c = client.startClient("wss://host/ws", "good");
    const ws1 = FakeWebSocket.instances[0];
    ws1.serverOpen();
    ws1.serverSend(HELLO_OK);

    c.sendMessage("hi there", null);
    expect(store.state.pendingSends).toHaveLength(1);
    expect(ws1.sentTypes().filter((t) => t === "send_message")).toHaveLength(1);

    // A network drop (not an auth reject) → reconnecting + scheduled retry.
    ws1.serverClose(1006);
    expect(store.state.connection).toBe("reconnecting");

    vi.advanceTimersByTime(2000); // clears the first backoff (≤ ~1.2s incl. jitter)
    const ws2 = FakeWebSocket.instances[1];
    expect(ws2).toBeTruthy();

    ws2.serverOpen();
    ws2.serverSend(HELLO_OK);
    // flushOutbox resent the still-un-acked send on the fresh socket.
    expect(ws2.sentTypes().filter((t) => t === "send_message")).toHaveLength(1);
    expect(store.state.connection).toBe("connected");
  });

  it("marks a send failed after the fail window with no echo", async () => {
    vi.useFakeTimers();
    const { store, client } = await load();
    const c = client.startClient("wss://host/ws", "good");
    const ws = FakeWebSocket.instances[0];
    ws.serverOpen();
    ws.serverSend(HELLO_OK);

    c.sendMessage("no echo", null);
    const cid = store.state.pendingSends[0].clientId;
    expect(store.state.messages.find((m) => m.clientId === cid)?.failed).toBeFalsy();

    vi.advanceTimersByTime(30_000);
    expect(store.state.messages.find((m) => m.clientId === cid)?.failed).toBe(true);
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

  it("treats repeated pre-hello closes on a never-authed token as auth failure", async () => {
    vi.useFakeTimers();
    const { client } = await load();
    localStorage.setItem("hirsel.token", "bad");
    const onAuthReject = vi.fn();
    client.startClient("wss://host/ws", "bad", { onAuthReject });

    // Strike 1: a generic close before any hello_ok — still just a retry.
    const ws1 = FakeWebSocket.instances[0];
    ws1.serverOpen();
    ws1.serverClose(1006);
    expect(onAuthReject).not.toHaveBeenCalled();

    // Strike 2: reconnects, closes pre-hello again → concluded auth failure.
    vi.advanceTimersByTime(2000);
    const ws2 = FakeWebSocket.instances[1];
    ws2.serverOpen();
    ws2.serverClose(1006);

    expect(onAuthReject).toHaveBeenCalledOnce();
    expect(client.getStoredToken()).toBeNull();
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
    FakeWebSocket.instances[1].serverClose(1006);
    vi.advanceTimersByTime(4000);

    expect(onAuthReject).not.toHaveBeenCalled();
    expect(store.state.connection).toBe("reconnecting");
    expect(FakeWebSocket.instances.length).toBeGreaterThanOrEqual(3);
  });
});

describe("HirselWsClient Ping ops (resolve / reopen)", () => {
  function connected(client: Awaited<ReturnType<typeof load>>["client"]) {
    const c = client.startClient("wss://host/ws", "good");
    const ws = FakeWebSocket.instances[0];
    ws.serverOpen();
    ws.serverSend(HELLO_OK);
    return { c, ws };
  }

  it("reopenPing emits exactly {type:'reopen_ping', ping_id}", async () => {
    const { client } = await load();
    const { c, ws } = connected(client);

    c.reopenPing(42);

    const frame = ws.sent.map((s) => JSON.parse(s)).find((f) => f.type === "reopen_ping");
    expect(frame).toEqual({ type: "reopen_ping", ping_id: 42 });
  });

  it("resolvePing and reopenPing are peers on the wire (resolve_ping / reopen_ping)", async () => {
    const { client } = await load();
    const { c, ws } = connected(client);

    c.resolvePing(7);
    c.reopenPing(7);

    const types = ws.sentTypes();
    expect(types).toContain("resolve_ping");
    expect(types).toContain("reopen_ping");
  });
});
