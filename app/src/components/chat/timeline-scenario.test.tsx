import { fireEvent, render, waitFor, within } from "@solidjs/testing-library";
import { createRequire } from "node:module";
import { resolve as pathResolve } from "node:path";
import { afterAll, beforeAll, describe, expect, it, vi } from "vitest";

/**
 * Headless scenario (spec v1.5 gate): the running turn renders as a lash-CLI
 * timeline, driven through the REAL App over a REAL WebSocket against an
 * in-process host that PUSHES turn_event frames on command.
 *
 * Proven here (spec checklist):
 *   1. Send `timeline` → a prose block is visible BEFORE the tool row.
 *   2. The tool row shows a spinner, then a check with a clean summary (no `{"`).
 *   3. A second prose block appears AFTER the tool.
 *   4. The reasoning row is collapsed until clicked, then reveals its text.
 *   5. Commit collapses the timeline into the committed message.
 *   6. "turn details" expands to show the same sequence.
 */

const now = () => new Date().toISOString();

const req = createRequire(pathResolve(process.cwd(), "vitest.scenario.cjs"));
// eslint-disable-next-line @typescript-eslint/no-explicit-any
const wsLib: any = req(pathResolve(process.cwd(), "node_modules/ws/index.js"));

interface ScriptedHost {
  port: number;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  push: (frame: any) => void;
  close: () => Promise<void>;
}

async function startScriptedHost(): Promise<ScriptedHost> {
  const wss = new wsLib.WebSocketServer({ port: 0 });
  await new Promise<void>((resolve) => wss.on("listening", () => resolve()));
  const port = wss.address().port as number;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const conns = new Set<any>();
  let idSeq = 100;

  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  wss.on("connection", (ws: any) => {
    conns.add(ws);
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const send = (frame: any) => ws.send(JSON.stringify(frame));
    ws.on("close", () => conns.delete(ws));
    ws.on("message", (raw: unknown) => {
      const frame = JSON.parse(String(raw));
      if (frame.type === "hello") {
        send({
          type: "hello_ok",
          latest_msg_id: 1,
          messages: [
            { id: 1, author: "agent", body: "Ready.", ref: null, ts: now(), attachments: [] },
          ],
          pings: [],
          processes: [],
        });
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
      }
    });
  });

  return {
    port,
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    push: (frame: any) => {
      const json = JSON.stringify(frame);
      for (const ws of conns) if (ws.readyState === ws.OPEN) ws.send(json);
    },
    close: () =>
      new Promise<void>((resolve) => {
        for (const c of conns) c.terminate();
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

describe("Headless scenario: running-turn timeline (v1.5)", () => {
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

  it("streams prose ↔ tool ↔ prose ↔ reasoning, then collapses into turn details", async () => {
    const host = await startScriptedHost();
    let closeClient: () => void = () => {};
    const checklist: string[] = [];
    try {
      vi.stubEnv("VITE_WS_URL", `ws://127.0.0.1:${host.port}`);
      localStorage.setItem("hirsel.token", "dev-token");

      const store = await import("../../store/store");
      const { getClient } = await import("../../ws/client");
      const { default: App } = await import("../../App");
      closeClient = () => getClient()?.close();

      const screen = render(() => <App />);
      await screen.findByText("Ready.");

      // Send the trigger word.
      const composer = (await screen.findByLabelText("Message Hirsel")) as HTMLTextAreaElement;
      fireEvent.input(composer, { target: { value: "timeline" } });
      // Enter is the send on a fine pointer — the capsule has no Send button.
      fireEvent.keyDown(composer, { key: "Enter" });
      await screen.findByText("timeline");

      // --- Stream the turn timeline ---
      host.push({ type: "agent_activity", state: "thinking", text: "Working through it…" });
      host.push({ type: "turn_event", seq: 1, event: { kind: "prose", text: "First I'll read the reducer." } });

      // 1. The opening prose is the reply being WRITTEN: the trailing prose run
      // streams into the margin as a reply in committed typography, not as a
      // row in the work log. No tool row exists anywhere yet.
      await screen.findByText("First I'll read the reducer.");
      const timeline = () => document.querySelector('[data-slot="timeline"]') as HTMLElement | null;
      const reply = () =>
        document.querySelector('[data-slot="streaming-reply"]') as HTMLElement | null;
      await waitFor(() => expect(reply()).toBeTruthy());
      expect(reply()!.textContent).toContain("First I'll read the reducer.");
      expect(document.querySelector('[data-slot="timeline-tool"]')).toBeNull();
      checklist.push("opening prose streamed as the in-flight reply, before any tool row");

      // 2. Tool starts → spinner (running), no result yet.
      host.push({
        type: "turn_event",
        seq: 2,
        event: { kind: "tool_start", id: "t1", name: "read_file", summary: "src/store/reducer.ts" },
      });
      await waitFor(() =>
        expect(timeline()!.querySelector('[aria-label="running"]')).toBeTruthy(),
      );
      // Calling a tool CLOSES the reply the Agent was writing: that prose is now
      // finished work, so it drops into the log ahead of the tool row (seq
      // order), and no reply is in flight.
      const kinds1 = Array.from(timeline()!.children).map((c) => c.getAttribute("data-slot"));
      expect(kinds1).toEqual(["timeline-prose", "timeline-tool"]);
      expect(reply()).toBeNull();
      checklist.push("a tool call closed the in-flight reply into the work log, spinner running");

      // tool_done → spinner becomes a check, clean summary (no raw JSON).
      host.push({
        type: "turn_event",
        seq: 3,
        event: { kind: "tool_done", id: "t1", name: "read_file", ok: true, summary: "read 142 lines" },
      });
      await waitFor(() => expect(timeline()!.querySelector('[aria-label="ok"]')).toBeTruthy());
      const toolRow = timeline()!.querySelector('[data-slot="timeline-tool"]') as HTMLElement;
      expect(toolRow.textContent).toContain("read 142 lines");
      expect(toolRow.textContent).not.toContain('{"');
      checklist.push("tool row resolved to a check with a clean summary (no JSON)");

      // 3. Second prose block after the tool.
      host.push({
        type: "turn_event",
        seq: 4,
        event: { kind: "prose", text: "The handler is wired correctly." },
      });
      await screen.findByText("The handler is wired correctly.");
      // Prose after the tool opens a NEW reply — again in the margin, while the
      // work log keeps the earlier prose and the resolved tool.
      await waitFor(() => expect(reply()).toBeTruthy());
      expect(reply()!.textContent).toContain("The handler is wired correctly.");
      const kinds2 = Array.from(timeline()!.children).map((c) => c.getAttribute("data-slot"));
      expect(kinds2).toEqual(["timeline-prose", "timeline-tool"]);
      checklist.push("prose after the tool opened a new in-flight reply, work log unchanged");

      // 4. Reasoning row: collapsed until clicked.
      host.push({
        type: "turn_event",
        seq: 5,
        event: { kind: "reasoning", text: "seq keeps the tool between the two prose blocks." },
      });
      // Reasoning likewise closes the reply, so by now the log holds the whole
      // prose ↔ tool ↔ prose ↔ reasoning sequence and nothing is in flight.
      const reasoning = () =>
        timeline()!.querySelector('[data-slot="timeline-reasoning"]') as HTMLElement | null;
      await waitFor(() => expect(reasoning()).toBeTruthy());
      expect(reply()).toBeNull();
      expect(Array.from(timeline()!.children).map((c) => c.getAttribute("data-slot"))).toEqual([
        "timeline-prose",
        "timeline-tool",
        "timeline-prose",
        "timeline-reasoning",
      ]);
      const rToggle = within(reasoning()!).getByRole("button");
      expect(rToggle.getAttribute("aria-expanded")).toBe("false");
      expect(screen.queryByText("seq keeps the tool between the two prose blocks.")).toBeNull();
      fireEvent.click(rToggle);
      expect(rToggle.getAttribute("aria-expanded")).toBe("true");
      await screen.findByText("seq keeps the tool between the two prose blocks.");
      checklist.push("reasoning row collapsed by default, expands to its text on click");

      // 5. Commit: idle + an agent message collapses the live timeline. The
      // real Host publishes the idle boundary from the observation bridge the
      // moment the session commits and only then broadcasts the message from
      // the turn pump, so idle-before-msg is the order to prove.
      host.push({ type: "agent_activity", state: "idle", text: null });
      host.push({
        type: "msg",
        message: {
          id: 2,
          author: "agent",
          body: "Checked the reducer — no changes needed.",
          ref: null,
          ts: now(),
          attachments: [],
          tool_calls: [{ name: "read_file", ok: true }],
        },
      });

      await screen.findByText("Checked the reducer — no changes needed.");
      await waitFor(() => expect(timeline()).toBeNull());
      // The live prose is gone from the DOM (folded away).
      expect(screen.queryByText("First I'll read the reducer.")).toBeNull();
      expect(store.state.turnEvents).toHaveLength(0);
      checklist.push("commit collapsed the live timeline into the committed message");

      // 6. "turn details" expands to the same sequence.
      const details = await screen.findByRole("button", { name: "Turn details" });
      fireEvent.click(details);
      await screen.findByText("First I'll read the reducer.");
      expect(screen.getByText("The handler is wired correctly.")).toBeTruthy();
      const detailTimeline = document.querySelector('[data-slot="timeline"]') as HTMLElement;
      const detailKinds = Array.from(detailTimeline.children).map((c) =>
        c.getAttribute("data-slot"),
      );
      expect(detailKinds).toEqual([
        "timeline-prose",
        "timeline-tool",
        "timeline-prose",
        "timeline-reasoning",
      ]);
      checklist.push('"turn details" expanded to the same prose ↔ tool ↔ prose ↔ reasoning sequence');

      // eslint-disable-next-line no-console
      console.log(`[timeline-scenario] PASS —\n  - ${checklist.join("\n  - ")}`);
    } finally {
      closeClient();
      await host.close();
    }
  }, 30000);
});
