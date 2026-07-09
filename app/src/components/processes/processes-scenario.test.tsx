import { fireEvent, render, waitFor, within } from "@solidjs/testing-library";
import { createRequire } from "node:module";
import { resolve as pathResolve } from "node:path";
import { afterAll, beforeAll, describe, expect, it, vi } from "vitest";

/**
 * Headless scenario (spec v1.4 gate): the Processes tab + tool-call visibility
 * driven through the REAL App over a REAL WebSocket against an in-process
 * scripted host that PUSHES server frames on command.
 *
 * Proven here:
 *   1. hello_ok seeds a RUNNING sub-agent → Processes badge 1, agent+model chips.
 *   2. Expand → "Ask to stop" → switches to Chat with the composer pre-filled
 *      "stop process <id> (<label>)" (interrupt routes through the Agent).
 *   3. A committed agent message carrying a tool_calls summary (no live timeline
 *      streamed) shows the fallback "⚙ 2 tools" chip that expands to the per-tool
 *      ok list. (The live v1.5 timeline path lives in timeline-scenario.test.tsx.)
 *   4. process_upsert done → the sub-agent moves Running → Finished, badge 0.
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

const SUBAGENT = {
  id: "proc-1",
  kind: "subagent" as const,
  label: "Review the auth refactor",
  agent: "code-reviewer",
  model: "gpt-5.5",
  state: "running" as const,
  started_ts: now(),
  last_event_ts: now(),
  summary: "reading changed files…",
};

/** In-process host: hello_ok seeds one anchor message + the running sub-agent;
 * exposes push() so the test can stream process/tool/turn frames on its own
 * timeline. Echoes owner send_message frames. */
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
          inbox: [],
          processes: [{ ...SUBAGENT }],
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

describe("Headless scenario: Processes tab + tool-call visibility", () => {
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

  it("runs the full process lifecycle + live/committed tool calls", async () => {
    const host = await startScriptedHost();
    let closeClient: () => void = () => {};
    const checklist: string[] = [];
    try {
      vi.stubEnv("VITE_WS_URL", `ws://127.0.0.1:${host.port}`);
      localStorage.setItem("hirsel.token", "dev-token");

      const store = await import("../../store/store");
      const { getClient } = await import("../../ws/client");
      const { runningProcessCount } = await import("../../store/selectors");
      const { default: App } = await import("../../App");
      closeClient = () => getClient()?.close();

      const screen = render(() => <App />);
      await waitFor(() => expect(store.state.processes).toHaveLength(1), { timeout: 10000 });

      // --- 1. Running sub-agent: badge 1, agent+model chips ---
      fireEvent.click(screen.getByText("Processes"));
      await screen.findByText("Running (1)");
      const row = (await screen.findByText("Review the auth refactor")).closest(
        '[data-slot="process-row"]',
      ) as HTMLElement;
      expect(within(row).getByText("code-reviewer")).toBeTruthy();
      expect(within(row).getByText("gpt-5.5")).toBeTruthy();
      expect(runningProcessCount(store.state.processes)).toBe(1);
      checklist.push("running sub-agent shown with agent+model chips, badge 1");

      // --- 2. Expand → Ask to stop → Chat with pre-filled composer ---
      fireEvent.click(within(row).getByText("Review the auth refactor").closest("button")!);
      fireEvent.click(await within(row).findByText("Ask to stop"));
      await waitFor(() => expect(store.state.activeTab).toBe("chat"));
      const composer = (await screen.findByPlaceholderText(
        "Message the Agent…",
      )) as HTMLTextAreaElement;
      await waitFor(() => expect(composer.value).toContain("stop process proc-1"));
      expect(composer.value).toContain("Review the auth refactor");
      checklist.push('"Ask to stop" switched to Chat and pre-filled the composer');

      // --- 3. Committed message with a tool_calls summary but no streamed
      // timeline → the fallback "⚙ N tools" chip (the full v1.5 live-timeline
      // path is covered by timeline-scenario.test.tsx). ---
      host.push({ type: "agent_activity", state: "thinking", text: "Working through it…" });
      host.push({
        type: "msg",
        message: {
          id: 2,
          author: "agent",
          body: "Checked the reducer and grepped — both fine.",
          ref: null,
          ts: now(),
          attachments: [],
          tool_calls: [
            { name: "read_file", ok: true },
            { name: "grep", ok: true },
          ],
        },
      });
      host.push({ type: "agent_activity", state: "idle", text: null });

      await screen.findByText("Checked the reducer and grepped — both fine.");
      const chip = await screen.findByRole("button", { name: /2 tool/ });
      expect(chip.textContent).toContain("2 tools");
      checklist.push('committed "⚙ 2 tools" fallback chip present (no live timeline captured)');

      // Expand the committed chip → per-tool ok list.
      fireEvent.click(chip);
      expect(await screen.findByText("read_file")).toBeTruthy();
      expect(screen.getByText("grep")).toBeTruthy();
      checklist.push("committed chip expands to the per-tool ok list");

      // --- 4. Completion: Running → Finished, badge 0 ---
      host.push({
        type: "process_upsert",
        process: { ...SUBAGENT, state: "done", summary: "done — PR opened", last_event_ts: now() },
      });
      await waitFor(() => expect(runningProcessCount(store.state.processes)).toBe(0), {
        timeout: 10000,
      });
      fireEvent.click(screen.getByText("Processes"));
      await screen.findByText("Finished (1)");
      expect(screen.queryByText("Running (1)")).toBeNull();
      checklist.push("process completed: moved Running → Finished, badge 0");

      // eslint-disable-next-line no-console
      console.log(`[processes-scenario] PASS —\n  - ${checklist.join("\n  - ")}`);
    } finally {
      closeClient();
      await host.close();
    }
  }, 30000);
});
