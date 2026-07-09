import { fireEvent, render, waitFor, within } from "@solidjs/testing-library";
import { createRequire } from "node:module";
import { resolve as pathResolve } from "node:path";
import { afterAll, afterEach, beforeAll, describe, expect, it, vi } from "vitest";

/**
 * Headless scenario (spec: "answer two requires_response items back-to-back
 * entirely inside the Inbox"; adapted v1.6 for the Tray). Unlike the mocked
 * component tests, this drives the REAL App over a REAL WebSocket against an
 * in-process scripted host, so it exercises the genuine send path —
 * client_id, optimistic reconcile, offline queue — and lets us assert the
 * frames that actually reached the wire.
 *
 * Checklist proven here:
 *   1. Inbox is scripted with two open requires_response items.
 *   2. The Owner taps the Tray shelf open (no more Inbox tab).
 *   3. Item 1 is answered by a Quick Reply tap; item 2 by the inline input.
 *   4. Neither answer collapses the Tray — it stays expanded throughout.
 *   5. Both replies reach the wire as send_message frames with the correct
 *      body + ref (= each item's anchor).
 *   6. Both optimistic sends reconcile to the host echo (pending → sent).
 *   7. ADR-0009: each anchored reply resolves its item to `done`, so both
 *      leave the open list (the Tray stays expanded regardless).
 *
 * Environment plumbing: vitest's jsdom resolves the `ws` package to its browser
 * stub and ships a `localStorage` with no methods, so we load the real Node
 * `ws` via an explicit-path require and patch localStorage in place before the
 * app modules load.
 */

const now = () => new Date().toISOString();

// Real Node `ws` (bare "ws" resolves to the no-op browser build under vitest).
const req = createRequire(pathResolve(process.cwd(), "vitest.scenario.cjs"));
// eslint-disable-next-line @typescript-eslint/no-explicit-any
const wsLib: any = req(pathResolve(process.cwd(), "node_modules/ws/index.js"));

interface WireFrame {
  client_id: string;
  body: string;
  ref: number | null;
}

interface ScriptedHost {
  port: number;
  received: WireFrame[];
  close: () => Promise<void>;
}

/** A minimal in-process host: replays two anchor messages + two open
 * requires_response Inbox Items on hello, records every send_message, echoes
 * each as an owner `msg` so the optimistic entry reconciles, and (ADR-0009)
 * resolves the anchored item to `done` — the mechanical reply-resolves rule. */
async function startScriptedHost(): Promise<ScriptedHost> {
  const received: WireFrame[] = [];
  let idSeq = 100;

  const anchors = [
    { id: 1, author: "agent", body: "Ready to deploy to prod?", ref: null, ts: now(), attachments: [] },
    { id: 2, author: "agent", body: "Branch is ready to merge.", ref: null, ts: now(), attachments: [] },
  ];
  const inbox = [
    {
      id: 1,
      content: "Deploy to prod?",
      anchor: 1,
      requires_response: true,
      quick_replies: [
        { value: "approve", label: "Approve" },
        { value: "reject", label: "Reject" },
      ],
      status: "open",
      ts: now(),
    },
    {
      id: 2,
      content: "Merge the branch?",
      anchor: 2,
      requires_response: true,
      quick_replies: [],
      status: "open",
      ts: now(),
    },
  ];

  const wss = new wsLib.WebSocketServer({ port: 0 });
  await new Promise<void>((resolve) => wss.on("listening", () => resolve()));
  const port = wss.address().port as number;

  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  wss.on("connection", (ws: any) => {
    ws.on("message", (raw: unknown) => {
      const frame = JSON.parse(String(raw));
      if (frame.type === "hello") {
        ws.send(JSON.stringify({ type: "hello_ok", latest_msg_id: 2, messages: anchors, inbox }));
        return;
      }
      if (frame.type === "send_message") {
        received.push({ client_id: frame.client_id, body: frame.body, ref: frame.ref ?? null });
        const message = {
          id: idSeq++,
          author: "owner",
          body: frame.body,
          ref: frame.ref ?? null,
          ts: now(),
          attachments: [],
        };
        ws.send(JSON.stringify({ type: "msg", message }));
        // ADR-0009: resolve the anchored item to `done`.
        const target = inbox.find((i) => i.status === "open" && i.anchor === (frame.ref ?? null));
        if (target) {
          target.status = "done";
          ws.send(JSON.stringify({ type: "inbox_upsert", item: { ...target } }));
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

/** vitest's environment exposes a method-less locked localStorage proxy; swap
 * in a real in-memory Storage so the ws client's token/last-seen reads work. */
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

describe("Headless scenario: answer two Inbox items back-to-back, in the Inbox", () => {
  let realWebSocket: typeof globalThis.WebSocket | undefined;

  beforeAll(() => {
    realWebSocket = globalThis.WebSocket;
    (globalThis as { WebSocket: unknown }).WebSocket = wsLib.WebSocket;
    vi.stubGlobal("localStorage", memoryStorage());
  });

  afterAll(() => {
    (globalThis as { WebSocket: unknown }).WebSocket = realWebSocket;
    vi.unstubAllGlobals();
    vi.unstubAllEnvs();
  });

  afterEach(() => {
    localStorage.clear();
  });

  it("answers a Quick Reply then an inline reply without ever leaving the Inbox", async () => {
    const host = await startScriptedHost();
    let closeClient: () => void = () => {};
    try {
      vi.stubEnv("VITE_WS_URL", `ws://127.0.0.1:${host.port}`);
      localStorage.setItem("hirsel.token", "dev-token");

      const store = await import("../../store/store");
      const { getClient } = await import("../../ws/client");
      const { default: App } = await import("../../App");
      closeClient = () => getClient()?.close();

      const screen = render(() => <App />);

      // Connected + inbox replayed: both scripted items are queued in the Tray.
      await waitFor(() => expect(store.state.inbox).toHaveLength(2), { timeout: 10000 });

      // Tap the Tray shelf open (no more Inbox tab; never auto-expanded).
      expect(store.state.trayExpanded).toBe(false);
      fireEvent.click(screen.getByLabelText("Open Pings"));
      expect(store.state.trayExpanded).toBe(true);
      await screen.findByText("Deploy to prod?");

      // --- Item 1: answer with a Quick Reply tap, in place ---
      fireEvent.click(screen.getByText("Approve"));
      await waitFor(
        () => expect(store.state.messages.some((m) => m.body === "approve" && !m.pending)).toBe(true),
        { timeout: 10000 },
      );
      expect(store.state.trayExpanded).toBe(true); // no collapse from the Quick Reply

      // --- Item 2: answer with the inline freeform input, in place ---
      const card2 = (await screen.findByText("Merge the branch?")).closest(
        '[data-slot="card"]',
      ) as HTMLElement;
      const input = within(card2).getByPlaceholderText("Reply…") as HTMLTextAreaElement;
      fireEvent.input(input, { target: { value: "merge it" } });
      fireEvent.click(within(card2).getByLabelText("Send reply"));
      await waitFor(
        () => expect(store.state.messages.some((m) => m.body === "merge it" && !m.pending)).toBe(true),
        { timeout: 10000 },
      );

      // --- Assertions: Tray stays open, both replies on the wire w/ correct refs ---
      expect(store.state.trayExpanded).toBe(true);
      expect(store.state.scrollToMessageId).toBeNull();
      expect(store.state.composerDraft).toBeNull();

      const byBody = (b: string) => host.received.find((f) => f.body === b);
      expect(host.received).toHaveLength(2);
      expect(byBody("approve")).toMatchObject({ ref: 1 });
      expect(byBody("merge it")).toMatchObject({ ref: 2 });

      // ADR-0009: both anchored replies resolved their items to `done`, so they
      // have left the open list (reconciled from the host's inbox_upsert).
      await waitFor(
        () => expect(store.state.inbox.every((i) => i.status === "done")).toBe(true),
        { timeout: 10000 },
      );
      expect(store.state.pendingSends).toHaveLength(0);

      // Grounded checklist for the report.
      // eslint-disable-next-line no-console
      console.log(
        "[inbox-reply-scenario] PASS — 2 items scripted; answered via QuickReply + inline; " +
          `Tray stayed expanded throughout (trayExpanded=${store.state.trayExpanded}); ` +
          `wire frames: ${host.received.map((f) => `${f.body}->ref${f.ref}`).join(", ")}`,
      );
    } finally {
      // Stop the client reconnect loop before tearing the host down, otherwise
      // wss.close() waits forever on the still-open client socket.
      closeClient();
      await host.close();
    }
  }, 30000);
});
