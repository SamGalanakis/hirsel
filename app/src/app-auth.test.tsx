import { render, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// End-to-end C5: a rejected token must dead-end into the gate with an error,
// not "reconnecting…" forever. Drives the real App + real ws client over a
// scriptable WebSocket.
type Listener = (ev: unknown) => void;

class FakeWebSocket {
  static readonly OPEN = 1;
  static instances: FakeWebSocket[] = [];
  url: string;
  readyState = 0;
  private listeners: Record<string, Listener[]> = {};
  constructor(url: string) {
    this.url = url;
    FakeWebSocket.instances.push(this);
  }
  addEventListener(type: string, cb: Listener) {
    (this.listeners[type] ??= []).push(cb);
  }
  removeEventListener() {}
  send() {}
  close() {}
  serverOpen() {
    this.readyState = FakeWebSocket.OPEN;
    for (const cb of this.listeners.open ?? []) cb({});
  }
  serverClose(code: number) {
    this.readyState = 3;
    for (const cb of this.listeners.close ?? []) cb({ code });
  }
}

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
  memStore.set("hirsel.token", "expired-token");
  vi.stubGlobal("localStorage", memLocalStorage);
});

afterEach(() => {
  vi.unstubAllGlobals();
  (globalThis as { WebSocket: unknown }).WebSocket = originalWebSocket;
});

describe("App auth-reject → gate transition (C5)", () => {
  it("drops to the gate with an inline error when the token is rejected", async () => {
    const { default: App } = await import("./App");
    const screen = render(() => <App />);

    // Boots into the shell (a stored token), opening the socket.
    const ws = await waitFor(() => {
      expect(FakeWebSocket.instances).toHaveLength(1);
      return FakeWebSocket.instances[0];
    });
    expect(screen.queryByLabelText("Access token")).toBeNull();

    // Host rejects the token (policy-violation close).
    ws.serverOpen();
    ws.serverClose(1008);

    // Back at the gate, with the inline error and the token cleared.
    const input = await waitFor(() => screen.getByLabelText("Access token"));
    expect(input).toBeTruthy();
    expect(screen.getByRole("alert").textContent).toMatch(/authenticate/i);
    expect(memLocalStorage.getItem("hirsel.token")).toBeNull();
  });
});
