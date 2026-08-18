import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { createRequire } from "node:module";
import { createServer } from "node:net";
import { afterEach, describe, expect, it } from "vitest";
import type WebSocketType from "ws";
import type { RawData } from "ws";

const require = createRequire(import.meta.url);
// Bypass Vite's browser-condition alias for `ws`: this test exercises a real
// child-process WebSocket server even though the rest of the suite uses jsdom.
const NodeWebSocket = require("../node_modules/ws/index.js") as typeof WebSocketType;

let child: ChildProcessWithoutNullStreams | undefined;

afterEach(async () => {
  if (!child || child.exitCode !== null) return;
  const exited = new Promise<void>((resolve) => child?.once("exit", () => resolve()));
  child.kill("SIGTERM");
  await exited;
  child = undefined;
});

async function freePort(): Promise<number> {
  const server = createServer();
  await new Promise<void>((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("failed to allocate mock port");
  await new Promise<void>((resolve, reject) => server.close((error) => error ? reject(error) : resolve()));
  return address.port;
}

function waitForFrame(
  ws: WebSocketType,
  predicate: (frame: Record<string, unknown>) => boolean,
): Promise<Record<string, unknown>> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      cleanup();
      reject(new Error("timed out waiting for mock frame"));
    }, 5_000);
    const onMessage = (raw: RawData) => {
      const frame = JSON.parse(raw.toString()) as Record<string, unknown>;
      if (!predicate(frame)) return;
      cleanup();
      resolve(frame);
    };
    const onError = (error: Error) => {
      cleanup();
      reject(error);
    };
    const cleanup = () => {
      clearTimeout(timer);
      ws.off("message", onMessage);
      ws.off("error", onError);
    };
    ws.on("message", onMessage);
    ws.on("error", onError);
  });
}

async function hello(
  port: number,
  token = "dev",
): Promise<{ ws: WebSocketType; frame: Record<string, unknown> }> {
  const ws = new NodeWebSocket(`ws://127.0.0.1:${port}/ws`);
  await new Promise<void>((resolve, reject) => {
    ws.once("open", resolve);
    ws.once("error", reject);
  });
  const response = waitForFrame(ws, (frame) => frame.type === "hello_ok");
  ws.send(JSON.stringify({ type: "hello", token, last_seen_msg_id: null }));
  return { ws, frame: await response };
}

function close(ws: WebSocketType): Promise<void> {
  if (ws.readyState === NodeWebSocket.CLOSED) return Promise.resolve();
  return new Promise((resolve) => {
    ws.once("close", () => resolve());
    ws.close();
  });
}

async function expectActionError(
  ws: WebSocketType,
  action: Record<string, unknown>,
  detail: string,
): Promise<void> {
  const rejected = waitForFrame(
    ws,
    (frame) => frame.type === "error" && String(frame.detail).includes(detail),
  );
  ws.send(JSON.stringify(action));
  expect(await rejected).toMatchObject({ type: "error" });
}

describe("dev mock task contract", () => {
  it("isolates tokens while replaying each token's actions and messages", async () => {
    const port = await freePort();
    child = spawn(process.execPath, ["tools/mock-server.mjs"], {
      cwd: process.cwd(),
      env: { ...process.env, MOCK_PORT: String(port), MOCK_REPLY_MS: "5000" },
      stdio: ["pipe", "pipe", "pipe"],
    });
    await new Promise<void>((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error("mock server did not start")), 5_000);
      child?.stdout.on("data", (chunk) => {
        if (!chunk.toString().includes("listening on")) return;
        clearTimeout(timer);
        resolve();
      });
      child?.once("exit", (code) => reject(new Error(`mock server exited early (${code})`)));
    });

    let connection = await hello(port);
    const first = connection.frame;
    expect(first.pings).toEqual([]);
    expect(first.events).toHaveLength(3);
    expect(first.model).toMatchObject({ current: { id: "gpt-5.6-sol", variant: "medium" } });
    expect(first.processes).toEqual(
      expect.arrayContaining([
        expect.objectContaining({ kind: "subagent", state: "running" }),
      ]),
    );

    const seededEvents = first.events as Array<Record<string, unknown>>;
    const deploy = seededEvents.find((event) => event.name === "deploy-4821");
    const auth = seededEvents.find((event) => event.name === "auth-pr");
    expect(deploy).toMatchObject({ kind: "judgment", status: "open", read: false });
    expect(auth).toMatchObject({ kind: "judgment", status: "open" });

    const echoed = waitForFrame(
      connection.ws,
      (frame) => frame.type === "msg" && (frame.message as { body?: string })?.body === "keep investigating",
    );
    connection.ws.send(JSON.stringify({
      type: "send_message",
      client_id: "contract-message",
      body: "keep investigating",
      ref: deploy?.anchor,
      mentions: [deploy?.id],
      attachments: [],
      mode: "send",
    }));
    expect((await echoed).message).toMatchObject({
      body: "keep investigating",
      ref: deploy?.anchor,
      mentions: [deploy?.id],
    });
    await close(connection.ws);

    connection = await hello(port);
    expect((connection.frame.events as Array<Record<string, unknown>>).find((event) => event.id === deploy?.id))
      .toMatchObject({ status: "open" });
    expect(connection.frame.messages).toEqual(expect.arrayContaining([
      expect.objectContaining({ body: "keep investigating", ref: deploy?.anchor, mentions: [deploy?.id] }),
    ]));

    const advanced = waitForFrame(
      connection.ws,
      (frame) => frame.type === "event_upsert" && (frame.event as { id?: number })?.id === deploy?.id,
    );
    connection.ws.send(JSON.stringify({
      type: "event_action",
      event_id: deploy?.id,
      action: "advance",
      data: { choice: "A" },
    }));
    const canary = (await advanced).event as Record<string, unknown>;
    expect(canary).toMatchObject({
      id: deploy?.id,
      anchor: deploy?.anchor,
      name: "deploy-4821",
      status: "open",
    });
    expect(canary.ui).toEqual(expect.arrayContaining([
      expect.objectContaining({ type: "heading", text: "Canary is healthy. Promote production?" }),
      expect.objectContaining({ type: "status", state: "success" }),
    ]));

    const chosen = waitForFrame(
      connection.ws,
      (frame) => frame.type === "event_upsert" && (frame.event as { id?: number })?.id === deploy?.id,
    );
    await expectActionError(connection.ws, {
      type: "event_action",
      event_id: deploy?.id,
      action: "choose",
      data: { choice: "A", label: "Wrong release" },
    }, "does not match choice");
    connection.ws.send(JSON.stringify({
      type: "event_action",
      event_id: deploy?.id,
      action: "choose",
      data: { choice: "A" },
    }));
    expect((await chosen).event).toMatchObject({ status: "done", id: deploy?.id, anchor: deploy?.anchor });
    await close(connection.ws);

    const isolated = await hello(port, "parallel-runbook");
    const isolatedEvents = isolated.frame.events as Array<Record<string, unknown>>;
    expect(isolatedEvents.find((event) => event.name === "deploy-4821"))
      .toMatchObject({ status: "open" });
    expect(isolated.frame.messages).not.toEqual(expect.arrayContaining([
      expect.objectContaining({ body: "keep investigating" }),
    ]));
    await close(isolated.ws);

    connection = await hello(port);
    expect((connection.frame.events as Array<Record<string, unknown>>).find((event) => event.id === deploy?.id))
      .toMatchObject({ status: "done" });
    const reopened = waitForFrame(
      connection.ws,
      (frame) => frame.type === "event_upsert" && (frame.event as { id?: number })?.id === deploy?.id,
    );
    connection.ws.send(JSON.stringify({ type: "event_action", event_id: deploy?.id, action: "reopen", data: {} }));
    const reopenedEvent = (await reopened).event as Record<string, unknown>;
    expect(reopenedEvent).toMatchObject({ status: "open", id: deploy?.id, anchor: deploy?.anchor });
    expect(reopenedEvent.ui).toEqual(expect.arrayContaining([
      expect.objectContaining({ type: "heading", text: "Canary is healthy. Promote production?" }),
    ]));

    for (const [data, detail] of [
      [{}, "requires data.reviewer"],
      [{ reviewer: 42 }, "must be a string"],
      [{ reviewer: "sam", injected: true }, "unknown Task action data field"],
      [{ reviewer: "x".repeat(9_000) }, "exceeds 8192 bytes"],
    ] as const) {
      await expectActionError(connection.ws, {
        type: "event_action",
        event_id: auth?.id,
        action: "submit",
        data,
      }, detail);
    }
    const submitted = waitForFrame(
      connection.ws,
      (frame) => frame.type === "event_upsert" && (frame.event as { id?: number })?.id === auth?.id,
    );
    connection.ws.send(JSON.stringify({
      type: "event_action",
      event_id: auth?.id,
      action: "submit",
      data: { reviewer: "sam" },
    }));
    expect((await submitted).event).toMatchObject({ status: "done" });
    await close(connection.ws);

    connection = await hello(port);
    const finalEvents = connection.frame.events as Array<Record<string, unknown>>;
    expect(finalEvents.find((event) => event.id === deploy?.id)).toMatchObject({ status: "open" });
    expect(finalEvents.find((event) => event.id === auth?.id)).toMatchObject({ status: "done" });
    await close(connection.ws);
  });
});
