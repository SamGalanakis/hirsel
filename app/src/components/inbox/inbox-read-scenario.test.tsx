import { fireEvent, render, waitFor, within } from "@solidjs/testing-library";
import userEvent from "@testing-library/user-event";
import { createRequire } from "node:module";
import { resolve as pathResolve } from "node:path";
import { afterAll, afterEach, beforeAll, describe, expect, it, vi } from "vitest";

/**
 * Headless scenario (spec v1.3 gate 2; adapted v1.6 for the Tray): an
 * email-like read lifecycle driven through the REAL App over a REAL
 * WebSocket against an in-process scripted host — the genuine
 * send/read/delete path, asserting the frames on the wire. Inbox now lives in
 * the Tray (no Inbox tab): the item is triaged by tapping the shelf open.
 *
 * Lifecycle proven here:
 *   1. Item arrives UNREAD → shelf badge = 1; tapping the shelf expands the
 *      Tray overlay, where the card shows its accent (unread dot).
 *   2. It scrolls into view → a read_ping frame hits the wire; the host echoes
 *      ping_upsert read=true → the card goes muted (data-read=true), badge = 0.
 *   3. The Owner replies (quick reply) → ADR-0009: the anchored reply resolves
 *      the item to `done`, so it leaves the open list and appears under the
 *      collapsed **Done** section (carrying the you:-reply quote), still inside
 *      the expanded Tray.
 */

const now = () => new Date().toISOString();

const req = createRequire(pathResolve(process.cwd(), "vitest.scenario.cjs"));
// eslint-disable-next-line @typescript-eslint/no-explicit-any
const wsLib: any = req(pathResolve(process.cwd(), "node_modules/ws/index.js"));

interface WireFrame {
  type: string;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  [k: string]: any;
}

interface ScriptedHost {
  port: number;
  received: WireFrame[];
  close: () => Promise<void>;
}

/** In-process host: replays one open, unread requires-response-free item plus
 * its anchor; on read_ping it upserts read=true; on send_message it echoes the
 * owner msg AND (ADR-0009) resolves the item to `done` when the reply is
 * anchored to it; on resolve_ping it upserts status=done. Records every frame. */
async function startScriptedHost(): Promise<ScriptedHost> {
  const received: WireFrame[] = [];
  let idSeq = 100;
  const anchors = [
    { id: 1, author: "agent", body: "Deploy finished.", ref: null, ts: now(), attachments: [] },
  ];
  const item = {
    id: 1,
    name: "deploy-follow-up",
    description: "Review the completed deployment",
    content: "Deploy finished — anything else?",
    anchor: 1,
    requires_response: false,
    quick_replies: [{ value: "all good", label: "All good" }],
    status: "open",
    read: false,
    ts: now(),
  };

  const wss = new wsLib.WebSocketServer({ port: 0 });
  await new Promise<void>((resolve) => wss.on("listening", () => resolve()));
  const port = wss.address().port as number;

  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  wss.on("connection", (ws: any) => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const send = (frame: any) => ws.send(JSON.stringify(frame));
    ws.on("message", (raw: unknown) => {
      const frame = JSON.parse(String(raw));
      received.push(frame);
      if (frame.type === "hello") {
        send({ type: "hello_ok", latest_msg_id: 1, messages: anchors, pings: [item] });
        return;
      }
      if (frame.type === "read_ping" && frame.ping_id === item.id) {
        item.read = true;
        send({ type: "ping_upsert", ping: { ...item } });
        return;
      }
      if (frame.type === "resolve_ping" && frame.ping_id === item.id) {
        item.status = "done";
        send({ type: "ping_upsert", ping: { ...item } });
        return;
      }
      if (frame.type === "send_message") {
        send({
          type: "msg",
          message: {
            id: idSeq++,
            author: "owner",
            body: frame.body,
            ref: frame.ref ?? null,
            ts: now(),
            attachments: [],
          },
        });
        // ADR-0009: an anchored reply to an open item resolves it to done.
        if (frame.ref === item.anchor && item.status === "open") {
          item.status = "done";
          send({ type: "ping_upsert", ping: { ...item } });
        }
      }
    });
  });

  return {
    port,
    received,
    close: () =>
      new Promise<void>((resolve) => {
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        for (const c of wss.clients as Set<any>) c.terminate();
        wss.close(() => resolve());
      }),
  };
}

function memoryStorage(): Storage {
  const mem = new Map<string, string>();
  return {
    get length() {
      return mem.size;
    },
    clear: () => mem.clear(),
    getItem: (k: string) => (mem.has(k) ? mem.get(k)! : null),
    key: (i: number) => Array.from(mem.keys())[i] ?? null,
    removeItem: (k: string) => void mem.delete(k),
    setItem: (k: string, v: string) => void mem.set(k, String(v)),
  } as Storage;
}

/** jsdom has no IntersectionObserver; install a stub that reports every
 * observed element as immediately (and continuously) intersecting, so the
 * card's auto-read dwell timer arms exactly as it would when scrolled on. */
function installIntersectionObserver() {
  class IOStub {
    private cb: IntersectionObserverCallback;
    constructor(cb: IntersectionObserverCallback) {
      this.cb = cb;
    }
    observe(el: Element) {
      // Fire on the next tick so the observing effect has settled.
      setTimeout(() => {
        this.cb(
          [{ isIntersecting: true, target: el } as unknown as IntersectionObserverEntry],
          this as unknown as IntersectionObserver,
        );
      }, 0);
    }
    unobserve() {}
    disconnect() {}
    takeRecords() {
      return [];
    }
  }
  (globalThis as { IntersectionObserver?: unknown }).IntersectionObserver = IOStub;
}

describe("Headless scenario: email-like read → reply → resolve lifecycle", () => {
  let realWebSocket: typeof globalThis.WebSocket | undefined;
  let realIO: typeof globalThis.IntersectionObserver | undefined;

  beforeAll(() => {
    realWebSocket = globalThis.WebSocket;
    realIO = (globalThis as { IntersectionObserver?: typeof globalThis.IntersectionObserver })
      .IntersectionObserver;
    (globalThis as { WebSocket: unknown }).WebSocket = wsLib.WebSocket;
    installIntersectionObserver();
    vi.stubGlobal("localStorage", memoryStorage());
  });

  afterAll(() => {
    (globalThis as { WebSocket: unknown }).WebSocket = realWebSocket;
    (globalThis as { IntersectionObserver: unknown }).IntersectionObserver = realIO;
    vi.unstubAllGlobals();
    vi.unstubAllEnvs();
  });

  afterEach(() => {
    localStorage.clear();
  });

  it("marks read on view (badge clears), then a reply resolves the item into the Done section", async () => {
    const host = await startScriptedHost();
    let closeClient: () => void = () => {};
    try {
      vi.stubEnv("VITE_WS_URL", `ws://127.0.0.1:${host.port}`);
      localStorage.setItem("hirsel.token", "dev-token");

      const store = await import("../../store/store");
      const { getClient } = await import("../../ws/client");
      const { openUnreadCount } = await import("../../store/selectors");
      const { default: App } = await import("../../App");
      closeClient = () => getClient()?.close();

      const screen = render(() => <App />);

      await waitFor(() => expect(store.state.pings).toHaveLength(1), { timeout: 10000 });
      // Tap the Tray shelf open (no more Inbox tab) — never auto-expanded.
      expect(store.state.trayExpanded).toBe(false);
      fireEvent.click(screen.getByLabelText("Open Pings"));
      expect(store.state.trayExpanded).toBe(true);
      // Scope inbox queries to the Tray overlay: the desktop Pings rail is a
      // second, always-mounted PingsView (CSS-hidden below the rail breakpoint
      // but present in the jsdom tree), so an unscoped card query matches twice.
      const tray = () => within(document.querySelector('[data-slot="tray-panel"]') as HTMLElement);

      // --- 1. Arrives UNREAD: unread dot + badge 1 ---
      const card = (await tray().findByText("Deploy finished — anything else?")).closest(
        '[data-slot="card"]',
      ) as HTMLElement;
      expect(card.getAttribute("data-read")).toBe("false");
      expect(within(card).getByLabelText("Unread")).toBeTruthy();
      expect(openUnreadCount(store.state.pings, store.state.unreadOverrides)).toBe(1);
      expect(document.title).toContain("(1)");

      // --- 2. Scrolled into view → read_ping on the wire → muted + badge 0 ---
      await waitFor(
        () => expect(host.received.some((f) => f.type === "read_ping" && f.ping_id === 1)).toBe(true),
        { timeout: 10000 },
      );
      await waitFor(() => expect(store.state.pings[0].read).toBe(true), { timeout: 10000 });
      expect(card.getAttribute("data-read")).toBe("true");
      expect(within(card).queryByLabelText("Unread")).toBeNull();
      expect(openUnreadCount(store.state.pings, store.state.unreadOverrides)).toBe(0);
      await waitFor(() => expect(document.title).toBe("hirsel"));

      // --- 3. Reply (quick reply) → ADR-0009: resolves into the Done section ---
      fireEvent.click(within(card).getByText("All good"));
      await waitFor(
        () => expect(store.state.messages.some((m) => m.body === "all good" && !m.pending)).toBe(true),
        { timeout: 10000 },
      );
      expect(host.received.some((f) => f.type === "send_message" && f.body === "all good")).toBe(true);

      // The anchored reply resolves the item to `done` — optimistically on send,
      // reconciled by the host's ping_upsert — so it leaves the open list and
      // the Tray falls back to its minimal "Done (1)" handle.
      await waitFor(() => expect(store.state.pings[0].status).toBe("done"), { timeout: 10000 });
      const panel = document.querySelector('[data-slot="tray-panel"]') as HTMLElement;
      await within(panel).findByText(/Done \(1\)/);

      // Expand the Done section: the resolved card carries the you:-reply quote
      // as its done detail (ADR-0009).
      const user = userEvent.setup();
      await user.click(within(panel).getByText(/Done \(1\)/));
      const doneCard = (await within(panel).findByText("Deploy finished — anything else?")).closest(
        '[data-slot="card"]',
      ) as HTMLElement;
      expect(doneCard.getAttribute("data-status")).toBe("done");
      expect(within(doneCard).getByText("you:")).toBeTruthy();
      expect(within(doneCard).getByText("all good")).toBeTruthy();

      // Grounded checklist for the report.
      // eslint-disable-next-line no-console
      console.log(
        "[inbox-read-scenario] PASS — unread→read via view (read_ping on wire), badge 1→0, " +
          `reply resolved item to done (you: all good) into the Done section; frames: ${host.received
            .map((f) => f.type)
            .join(",")}`,
      );
    } finally {
      closeClient();
      await host.close();
    }
  }, 30000);
});
