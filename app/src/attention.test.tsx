import { render, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { EventKind } from "./protocol";
import type { EventItem } from "./protocol";

// Wave 1: the attention layer (tab-title badge, favicon dot, desktop
// notification) reads the "needs you" count over EVENTS — open, undecided
// judgments. These drive the real App
// over a scriptable WebSocket + a controlled event set.

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

function judgment(id: number, extra: Partial<EventItem> = {}): EventItem {
  return {
    id,
    kind: EventKind.Judgment,
    source: { kind: "subagent", ref: "hirsel-ui" },
    name: `@j${id}`,
    description: "needs you",
    ui: [{ type: "heading", text: `Question ${id}?` }],
    requires_response: true,
    quick_replies: [],
    status: "open",
    read: false,
    anchor: 0,
    ts: "2026-07-13T09:00:00Z",
    blocking: true,
    ...extra,
  };
}

/** A hello_ok that replaces the event set wholesale (overrides any DEV mock
 * seed), so each test starts from a known count. `pings` stays required on the
 * wire type but is ignored by the reducer now. */
function helloOk(events: EventItem[]) {
  return {
    type: "hello_ok" as const,
    payload: {
      type: "hello_ok" as const,
      latest_msg_id: 0,
      messages: [],
      pings: [],
      events,
      processes: [],
      views: [],
    },
  };
}

function faviconHref(): string {
  return document.querySelector<HTMLLinkElement>('link[rel="icon"]')?.href ?? "";
}

let originalWebSocket: unknown;

beforeEach(() => {
  vi.resetModules();
  FakeWebSocket.instances = [];
  originalWebSocket = (globalThis as { WebSocket?: unknown }).WebSocket;
  (globalThis as { WebSocket: unknown }).WebSocket = FakeWebSocket;
  memStore.clear();
  memStore.set("hirsel.token", "tok");
  vi.stubGlobal("localStorage", memLocalStorage);
  document.head.innerHTML = '<link rel="icon" type="image/svg+xml" href="/favicon.svg" />';
  document.title = "hirsel";
});

afterEach(() => {
  vi.unstubAllGlobals();
  (globalThis as { WebSocket: unknown }).WebSocket = originalWebSocket;
});

describe("attention layer reads the needs-you count over events", () => {
  it("puts the open-judgment count in the tab title and reverts on decide", async () => {
    const { default: App } = await import("./App");
    const { dispatch } = await import("./store/store");
    render(() => <App />);

    dispatch(helloOk([]));
    await waitFor(() => expect(document.title).toBe("hirsel"));

    dispatch({ type: "event_upsert", payload: { type: "event_upsert", event: judgment(9001) } });
    await waitFor(() => expect(document.title).toBe("(1) hirsel"));

    dispatch({ type: "event_upsert", payload: { type: "event_upsert", event: judgment(9002) } });
    await waitFor(() => expect(document.title).toBe("(2) hirsel"));

    // Deciding one drops the count; awareness (a summary) never contributes.
    dispatch({ type: "event_decide_local", eventId: 9001 });
    dispatch({
      type: "event_upsert",
      payload: { type: "event_upsert", event: judgment(9003, { kind: EventKind.Summary, blocking: false }) },
    });
    await waitFor(() => expect(document.title).toBe("(1) hirsel"));
  });

  it("swaps the favicon to the dotted variant while anything needs you", async () => {
    const { default: App } = await import("./App");
    const { dispatch } = await import("./store/store");
    render(() => <App />);

    dispatch(helloOk([]));
    await waitFor(() => expect(faviconHref()).toMatch(/\/favicon\.svg$/));

    dispatch({ type: "event_upsert", payload: { type: "event_upsert", event: judgment(9001) } });
    await waitFor(() => expect(faviconHref()).toMatch(/\/favicon-dot\.svg$/));

    dispatch({ type: "event_decide_local", eventId: 9001 });
    await waitFor(() => expect(faviconHref()).toMatch(/\/favicon\.svg$/));
  });

  it("respects the title-badge preference toggle", async () => {
    memStore.set("hirsel.titleBadge", "0");
    const { default: App } = await import("./App");
    const { dispatch } = await import("./store/store");
    render(() => <App />);

    dispatch(helloOk([judgment(9001)]));
    // Badge disabled → the title stays clean even with a needs-you judgment.
    await waitFor(() => expect(document.title).toBe("hirsel"));
  });
});

describe("desktop notification for a new blocking judgment", () => {
  it("fires once, silent, only while hidden with permission granted", async () => {
    const notifSpy = vi.fn();
    class FakeNotification {
      static permission: NotificationPermission = "granted";
      static requestPermission = vi.fn(async () => "granted" as NotificationPermission);
      constructor(title: string, opts?: NotificationOptions) {
        notifSpy(title, opts);
      }
    }
    vi.stubGlobal("Notification", FakeNotification);
    Object.defineProperty(document, "visibilityState", {
      configurable: true,
      get: () => "hidden",
    });

    const { default: App } = await import("./App");
    const { dispatch } = await import("./store/store");
    render(() => <App />);

    // Prime with an empty set so the first effect run records "no blocking work".
    dispatch(helloOk([]));
    await waitFor(() => expect(document.title).toBe("hirsel"));
    notifSpy.mockClear();

    dispatch({ type: "event_upsert", payload: { type: "event_upsert", event: judgment(9001) } });
    await waitFor(() => expect(notifSpy).toHaveBeenCalledTimes(1));
    const [, opts] = notifSpy.mock.calls[0];
    expect(opts).toMatchObject({ silent: true, tag: "hirsel-judgment-9001" });

    // A non-blocking awareness event does not notify.
    dispatch({
      type: "event_upsert",
      payload: { type: "event_upsert", event: judgment(9002, { kind: EventKind.Info, blocking: false }) },
    });
    await waitFor(() => expect(document.title).toBe("(1) hirsel"));
    expect(notifSpy).toHaveBeenCalledTimes(1);
  });
});
