import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { createRequire } from "node:module";
import { createServer } from "node:net";
import { afterEach, describe, expect, it } from "vitest";
import type WebSocketType from "ws";
import type { RawData } from "ws";
import type { Thread, ThreadDetail } from "./threads/types";
import type { ChatMessage } from "./protocol";

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

describe("dev mock Thread contract", () => {
  it("isolates tokens while replaying owned histories and explicit lifecycle", async () => {
    const port = await freePort();
    child = spawn(process.execPath, ["tools/mock-server.mjs"], {
      cwd: process.cwd(),
      env: { ...process.env, MOCK_PORT: String(port), MOCK_REPLY_MS: "10" },
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
    expect(connection.frame.threads).toEqual(expect.arrayContaining([
      expect.objectContaining({ id: 0 }),
      expect.objectContaining({ id: 1, title: "Buy groceries", attention: "quiet", settled_at: null }),
    ]));
    const request = async (command: Record<string, unknown>, type: string) => {
      const response = waitForFrame(connection.ws, frame => frame.type === type);
      connection.ws.send(JSON.stringify(command));
      return response;
    };
    const created = (await request({ type: "create_thread", client_id: "create", title: "New subject" }, "thread_created")).thread as Thread;
    expect(created.attention).toBe("quiet");
    expect((await request({ type: "create_thread", client_id: "create", title: "New subject" }, "thread_created")).thread).toEqual(created);
    const completed = waitForFrame(connection.ws, frame => frame.type === "thread_turn" && (frame.turn as { state?: string }).state === "completed");
    const command = { type: "send_thread_message", thread_id: created.id, client_id: "message", body: "keep investigating", mentions: [1], attachments: [] };
    const owner = (await request(command, "msg")).message as ChatMessage;
    expect(owner).toMatchObject({ thread_id: created.id, mentions: [1], client_id: "message" });
    await completed;
    expect((await request(command, "msg")).message).toEqual(owner);
    await expectActionError(connection.ws, { ...command, body: "conflicting retry" }, "different content");
    const detail = (await request({ type: "open_thread", client_id: "open", thread_id: created.id }, "thread_opened")).detail as ThreadDetail;
    expect(detail.messages).toHaveLength(2);
    expect(detail.messages.every(message => message.thread_id === created.id)).toBe(true);
    expect(detail.turns).toHaveLength(1);
    const earlier = (await request({ type: "open_thread", client_id: "earlier", thread_id: created.id, before_id: detail.messages[1].id }, "thread_opened")).detail as ThreadDetail;
    expect(earlier.messages).toEqual([owner]);
    expect(earlier.has_more).toBe(false);
    await expectActionError(connection.ws, { type: "thread_action", thread_id: created.id, action: "invented", expected_revision: 0 }, "changed");
    await expectActionError(connection.ws, { type: "thread_action", thread_id: created.id, action: "invented", expected_revision: created.revision }, "no generated action");
    expect((await request({ type: "thread_action", thread_id: created.id, action: "read" }, "thread_upsert")).thread).toMatchObject({ id: created.id, read: true, settled_at: null });
    const settled = (await request({ type: "thread_action", thread_id: created.id, action: "settle" }, "thread_upsert")).thread as Thread;
    expect(settled.settled_at).not.toBeNull();
    await close(connection.ws);
    const isolated = await hello(port, "parallel-runbook");
    expect((isolated.frame.threads as Thread[]).some(thread => thread.id === created.id)).toBe(false);
    expect(isolated.frame.messages).toEqual([]);
    await close(isolated.ws);
    connection = await hello(port);
    expect((connection.frame.threads as Thread[]).find(thread => thread.id === created.id)).toEqual(settled);
    expect(connection.frame.messages).toEqual([]);
    const replay = (await request({ type: "open_thread", client_id: "replay", thread_id: created.id }, "thread_opened")).detail as ThreadDetail;
    expect(replay.messages).toEqual(detail.messages);
    expect((await request({ type: "thread_action", thread_id: created.id, action: "reopen" }, "thread_upsert")).thread).toMatchObject({ id: created.id, settled_at: null, read: true });
    await expectActionError(connection.ws, { type: "thread_action", thread_id: 0, action: "settle" }, "orchestrator");
    const publish = async (body: Record<string, unknown>) => {
      const response = await fetch(`http://127.0.0.1:${port}/debug/publish-artifact`, { method:"POST", headers:{ Authorization:"Bearer dev", "Content-Type":"application/json" }, body:JSON.stringify(body) });
      expect(response.status).toBe(200);
      return response.json() as Promise<{id:number;content:string;thread_ids:number[]}>;
    };
    const draft = {title:"Architecture",kind:"html",mime:"text/html",content:"<p>First</p>"};
    const artifact = await publish({operation_id:"artifact-create",thread_id:created.id,draft});
    await publish({operation_id:"artifact-show",thread_id:1,artifact_id:artifact.id});
    await publish({operation_id:"artifact-edit",thread_id:created.id,artifact_id:artifact.id,draft:{...draft,content:"<p>Latest</p>"}});
    const artifactReplay=await publish({operation_id:"artifact-create",thread_id:created.id,draft});
    expect(artifactReplay.content).toBe("<p>Latest</p>");
    expect(artifactReplay.thread_ids).toEqual([1,created.id]);
    const artifacts = await request({type:"list_artifacts",client_id:"list-results",thread_id:1},"artifacts_listed");
    expect(artifacts.artifacts).toEqual([expect.objectContaining({id:artifact.id})]);
    const opened=await request({type:"open_artifact",client_id:"open-result",artifact_id:artifact.id},"artifact_opened");
    expect(opened.artifact).toMatchObject({id:artifact.id,content:"<p>Latest</p>"});
    await close(connection.ws);
    connection=await hello(port);
    const withCards=(await request({type:"open_thread",client_id:"card-replay",thread_id:created.id},"thread_opened")).detail as ThreadDetail;
    expect(withCards.messages.filter(message=>message.artifact_ids?.includes(artifact.id))).toHaveLength(2);
    const other=await hello(port,"isolated-artifacts");
    const otherList=waitForFrame(other.ws,frame=>frame.type==="artifacts_listed");
    other.ws.send(JSON.stringify({type:"list_artifacts",client_id:"empty"}));
    expect((await otherList).artifacts).toEqual([]);
    await close(other.ws);

    await close(connection.ws);
  });
});
