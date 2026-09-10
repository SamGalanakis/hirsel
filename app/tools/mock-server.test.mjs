import { spawn } from "node:child_process";
import { once } from "node:events";
import assert from "node:assert/strict";
import { test } from "node:test";
import { WebSocket } from "ws";

function client(url) {
  const ws = new WebSocket(url);
  const frames = [];
  ws.on("message", data => frames.push(JSON.parse(data.toString())));
  return { ws, frames, send: frame => ws.send(JSON.stringify(frame)), async next(type, predicate = () => true) {
    const deadline = Date.now() + 5000;
    while (Date.now() < deadline) {
      const index = frames.findIndex(frame => frame.type === type && predicate(frame));
      if (index !== -1) return frames.splice(index, 1)[0];
      await new Promise(resolve => setTimeout(resolve, 10));
    }
    throw new Error(`Missing ${type}: ${JSON.stringify(frames)}`);
  }};
}
test("Thread mock preserves identity, owned history and lifecycle across reconnect", async () => {
  const server = spawn(process.execPath, [new URL("mock-server.mjs", import.meta.url).pathname], { env: { ...process.env, MOCK_PORT: "0", MOCK_SEED: "none", MOCK_REPLY_MS: "100" }, stdio: ["ignore", "pipe", "inherit"] });
  const connections = [];
  try {
    const [output] = await once(server.stdout, "data");
    const port = output.toString().match(/127\.0\.0\.1:(\d+)/)?.[1];
    assert.ok(port, output.toString());
    const a = client(`ws://127.0.0.1:${port}/ws`); connections.push(a.ws);
    await once(a.ws, "open"); a.send({ type: "hello", auth: { static_token: "a" } });
    const initialHello = await a.next("hello_ok");
    assert.equal(initialHello.threads.length, 0);
    const addressed = frame => ({ history_id: initialHello.history_id, ...frame });
    a.send({ type: "upload_blob", client_id: "upload", name: "list.txt", mime: "text/plain", data_b64: Buffer.from("milk").toString("base64") });
    const blob = (await a.next("blob_ok")).blob;
    a.send({ type: "get_blob_url", client_id: "url", blob_id: blob.id });
    const grant = await a.next("blob_url");
    assert.ok(grant.expires_at > Date.now() / 1000);
    assert.equal(await (await fetch(`http://127.0.0.1:${port}${grant.url}`)).text(), "milk");
    assert.equal((await fetch(`http://127.0.0.1:${port}/blob/${blob.id}?token=a`)).status, 403);
    const create = addressed({ type: "create_thread", parent_thread_id: null, client_id: "create", title: "Buy groceries" });
    a.send(create);
    const thread = (await a.next("thread_created")).thread;
    assert.equal(thread.attention, "quiet");
    a.send(create);
    assert.deepEqual((await a.next("thread_created")).thread, thread, "an original create retry replays the complete initial summary");
    a.send(addressed({ ...create, title: "Different groceries" }));
    assert.match((await a.next("error", frame => frame.detail === "client_id already used")).detail, /client_id already used/);
    a.send(addressed({ type: "send_thread_message", artifact_ids: [], client_id: "message", thread_id: thread.id, body: "Milk", attachments: [], mentions: [] }));
    const owner = (await a.next("msg", frame => frame.message.author === "owner")).message;
    const running = await a.next("thread_turn", frame => frame.turn.state === "running");
    a.send(create);
    const runningReplay = (await a.next("thread_created")).thread;
    assert.deepEqual(runningReplay.running_turn, running.turn, "a duplicate create while running reflects current turn state");
    assert.equal(runningReplay.last_finished_turn, null);
    const completed = await a.next("thread_turn", frame => frame.turn.state === "completed");
    a.send(create);
    const completedReplay = (await a.next("thread_created")).thread;
    assert.equal(completedReplay.running_turn, null);
    assert.deepEqual(completedReplay.last_finished_turn, completed.turn, "a duplicate create after completion reflects the terminal turn");
    a.send(addressed({ type: "send_thread_message", artifact_ids: [], client_id: "message", thread_id: thread.id, body: "Milk" }));
    assert.equal((await a.next("msg", frame => frame.message.author === "owner")).message.id, owner.id);
    a.send(addressed({ type: "thread_action", thread_id: thread.id, action: "read" }));
    assert.equal((await a.next("thread_upsert", frame => frame.thread.read)).thread.settled_at, null);
    a.send(addressed({ type: "thread_action", thread_id: thread.id, action: "settle" }));
    const settled = await a.next("thread_upsert", frame => frame.thread.settled_at !== null);
    a.send(create);
    const settledReplay = (await a.next("thread_created")).thread;
    assert.deepEqual(settledReplay, settled.thread, "create replay and action upsert share one current summary");
    a.ws.close();
    const b = client(`ws://127.0.0.1:${port}/ws`); connections.push(b.ws);
    await once(b.ws, "open"); b.send({ type: "hello", auth: { static_token: "a" } });
    const hello = await b.next("hello_ok");
    assert.deepEqual(hello.threads.find(row => row.id === thread.id), settledReplay, "reconnect inventory agrees with the latest create replay");
    assert.equal("messages" in hello, false, "hello carries inventory only");
    b.send({ type: "open_thread", client_id: "open", thread_id: thread.id });
    const detail = (await b.next("thread_opened")).detail;
    assert.equal(detail.messages.length, 2);
    assert.ok(detail.messages.every(row => row.thread_id === thread.id));
    assert.equal(detail.turns.length, 1, "retry never starts a duplicate turn");
  } finally {
    for (const ws of connections) ws.terminate();
    server.kill("SIGTERM");
    await once(server, "exit");
  }
});
