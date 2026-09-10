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
  ws.send(JSON.stringify({ type: "hello", auth: { static_token: token } }));
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
  expect(await rejected).toMatchObject({ type: "error", client_id: action.client_id });
}

describe("dev mock Thread contract", () => {
  it("isolates tokens while replaying owned histories and explicit lifecycle", async () => {
    const port = await freePort();
    child = spawn(process.execPath, ["tools/mock-server.mjs"], {
      cwd: process.cwd(),
      env: { ...process.env, MOCK_PORT: String(port), MOCK_REPLY_MS: "100" },
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
      expect.objectContaining({ id: 1, title: "Buy groceries", attention: "quiet", settled_at: null }),
    ]));
    const historyId = connection.frame.history_id as string;
    const addressed = (command: Record<string, unknown>) => ({ history_id: historyId, ...command });
    let nextActionId = 0;
    const request = async (command: Record<string, unknown>, type: string) => {
      const response = waitForFrame(connection.ws, frame => frame.type === type);
      const correlated = command.type === "thread_action" && command.client_id == null
        ? { client_id: `action-${++nextActionId}`, ...command }
        : command;
      const frame = ["create_thread", "send_thread_message", "thread_action", "cancel_turn"].includes(String(command.type)) ? addressed(correlated) : correlated;
      connection.ws.send(JSON.stringify(frame));
      return response;
    };
    const createCommand = { parent_thread_id: null, type: "create_thread", client_id: "create", title: "New subject" };
    const created = (await request(createCommand, "thread_created")).thread as Thread;
    expect(created.attention).toBe("quiet");
    expect((await request(createCommand, "thread_created")).thread).toEqual(created);
    await expectActionError(connection.ws, addressed({ ...createCommand, title: "Different subject" }), "client_id already used");
    const running = waitForFrame(connection.ws, frame => frame.type === "thread_turn" && (frame.turn as { state?: string }).state === "running");
    const completed = waitForFrame(connection.ws, frame => frame.type === "thread_turn" && (frame.turn as { state?: string }).state === "completed");
    const command = { type: "send_thread_message", artifact_ids: [], thread_id: created.id, client_id: "message", body: "keep investigating", mentions: [1], attachments: [] };
    const owner = (await request(command, "msg")).message as ChatMessage;
    expect(owner).toMatchObject({ thread_id: created.id, mentions: [1], client_id: "message" });
    const runningFrame = await running;
    const runningReplay = (await request(createCommand, "thread_created")).thread as Thread;
    expect(runningReplay.running_turn).toEqual(runningFrame.turn);
    expect(runningReplay.queued_turn_count).toBe(0);
    expect(runningReplay.last_finished_turn).toBeNull();
    const completedFrame = await completed;
    const completedReplay = (await request(createCommand, "thread_created")).thread as Thread;
    expect(completedReplay.running_turn).toBeNull();
    expect(completedReplay.last_finished_turn).toEqual(completedFrame.turn);
    expect((await request(command, "msg")).message).toEqual(owner);
    await expectActionError(connection.ws, addressed({ ...command, body: "conflicting retry" }), "different content");
    const detail = (await request({ type: "open_thread", client_id: "open", thread_id: created.id }, "thread_opened")).detail as ThreadDetail;
    expect(detail.messages).toHaveLength(2);
    expect(detail.messages.every(message => message.thread_id === created.id)).toBe(true);
    expect(detail.turns).toHaveLength(1);
    const earlier = (await request({ type: "open_thread", client_id: "earlier", thread_id: created.id, before_id: detail.messages[1].id }, "thread_opened")).detail as ThreadDetail;
    expect(earlier.messages).toEqual([owner]);
    expect(earlier.has_more).toBe(false);
    await expectActionError(connection.ws, addressed({ type: "thread_action", client_id: "action-stale", thread_id: created.id, action: "invented", expected_revision: 0 }), "changed");
    await expectActionError(connection.ws, addressed({ type: "thread_action", client_id: "action-unsupported", thread_id: created.id, action: "invented", expected_revision: created.revision }), "no generated action");
    const readAck = waitForFrame(connection.ws, frame => frame.type === "thread_action_applied" && frame.client_id === "action-read");
    expect((await request({ type: "thread_action", client_id: "action-read", thread_id: created.id, action: "read" }, "thread_upsert")).thread).toMatchObject({ id: created.id, read: true, settled_at: null });
    expect(await readAck).toMatchObject({ type: "thread_action_applied", client_id: "action-read", history_id: historyId, thread_id: created.id });
    const settled = (await request({ type: "thread_action", thread_id: created.id, action: "settle" }, "thread_upsert")).thread as Thread;
    expect(settled.settled_at).not.toBeNull();
    expect(settled.icon).toBeNull();
    const withIcon = (await request({ type: "thread_action", thread_id: created.id, action: "set_icon", data: { icon: "👩🏽‍💻" }, expected_revision: settled.revision }, "thread_upsert")).thread as Thread;
    expect(withIcon.icon).toBe("👩🏽‍💻");
    await expectActionError(connection.ws, addressed({ type: "thread_action", client_id: "action-icon-stale", thread_id: created.id, action: "set_icon", data: { icon: "🌱" }, expected_revision: settled.revision }), "changed");
    await expectActionError(connection.ws, addressed({ type: "thread_action", client_id: "action-icon-invalid", thread_id: created.id, action: "set_icon", data: { icon: "x\n" }, expected_revision: withIcon.revision }), "Invalid thread icon");
    const reset = (await request({ type: "thread_action", thread_id: created.id, action: "set_icon", data: { icon: null }, expected_revision: withIcon.revision }, "thread_upsert")).thread as Thread;
    expect(reset.icon).toBeNull();
    await close(connection.ws);
    const isolated = await hello(port, "parallel-runbook");
    expect((isolated.frame.threads as Thread[]).some(thread => thread.id === created.id)).toBe(false);
    expect(isolated.frame).not.toHaveProperty("messages");
    await close(isolated.ws);
    connection = await hello(port);
    const reconnectedSummary = (connection.frame.threads as Thread[]).find(thread => thread.id === created.id);
    expect(reconnectedSummary).toEqual(reset);
    expect((await request(createCommand, "thread_created")).thread).toEqual(reconnectedSummary);
    expect(connection.frame).not.toHaveProperty("messages");
    const replay = (await request({ type: "open_thread", client_id: "replay", thread_id: created.id }, "thread_opened")).detail as ThreadDetail;
    expect(replay.messages).toEqual(detail.messages);
    const reopened = (await request({ type: "thread_action", thread_id: created.id, action: "reopen" }, "thread_upsert")).thread as Thread;
    expect(reopened).toMatchObject({ id: created.id, settled_at: null, read: true });
    await expectActionError(connection.ws, addressed({ type: "thread_action", client_id: "action-missing-thread", thread_id: 0, action: "settle" }), "does not exist");
    await expectActionError(connection.ws, addressed({ type: "create_thread", client_id: "missing-parent", title: "Missing parent" }), "parent_thread_id");
    const childThread = (await request({ type: "create_thread", client_id: "child", title: "Focused review", parent_thread_id: created.id }, "thread_created")).thread as Thread;
    expect(childThread.parent_thread_id).toBe(created.id);
    await expectActionError(connection.ws, addressed({ type: "thread_action", client_id: "action-child-pin", thread_id: childThread.id, action: "pin", expected_revision: childThread.revision }), "Only top-level threads");
    const pinned = (await request({ type: "thread_action", thread_id: created.id, action: "pin", expected_revision: reopened.revision }, "thread_upsert")).thread as Thread;
    expect(pinned.pinned_at).not.toBeNull();
    expect(pinned.last_activity_at).toBe(reopened.last_activity_at);
    expect(pinned.settled_at).toBeNull();
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

    const recipient=(await request({type:"create_thread",client_id:"artifact-recipient",title:"Unrelated artifact discussion",parent_thread_id:null},"thread_created")).thread as Thread;
    const beforeShare=(await request({type:"open_artifact",client_id:"preview-only",artifact_id:artifact.id},"artifact_opened")).artifact as {thread_ids:number[]};
    expect(beforeShare.thread_ids).not.toContain(recipient.id);
    const plainDone=waitForFrame(connection.ws,frame=>frame.type==="thread_turn"&&(frame.turn as {thread_id:number;state:string}).thread_id===recipient.id&&(frame.turn as {state:string}).state==="completed");
    const plain=(await request({type:"send_thread_message",client_id:"plain-artifact-number",thread_id:recipient.id,body:`Edit artifact ${artifact.id}`,attachments:[],mentions:[],artifact_ids:[]},"msg")).message as ChatMessage;
    expect(plain.artifact_ids).toEqual([]); await plainDone;
    const contextMessage={type:"send_thread_message",client_id:"explicit-artifact-reference",thread_id:recipient.id,body:"Make this simpler",attachments:[],mentions:[],artifact_ids:[artifact.id,artifact.id]};
    const shared=(await request(contextMessage,"msg")).message as ChatMessage;
    expect(shared.artifact_ids).toEqual([artifact.id]);
    expect((await request({...contextMessage,artifact_ids:[artifact.id]},"msg")).message).toEqual(shared);
    await expectActionError(connection.ws,addressed({...contextMessage,artifact_ids:[]}),"different content");
    await expectActionError(connection.ws,addressed({...contextMessage,client_id:"invalid-artifact",artifact_ids:[999999]}),"does not exist");
    const afterShare=(await request({type:"open_thread",client_id:"shared-history",thread_id:recipient.id},"thread_opened")).detail as ThreadDetail;
    expect(afterShare.messages.filter(message=>message.author==="owner")).toHaveLength(2);
    expect(afterShare.messages.filter(message=>message.artifact_ids?.includes(artifact.id))).toHaveLength(1);
    const linked=(await request({type:"open_artifact",client_id:"after-reference",artifact_id:artifact.id},"artifact_opened")).artifact as {thread_ids:number[]};
    expect(linked.thread_ids).toContain(recipient.id);
    await close(connection.ws);
  });
});
