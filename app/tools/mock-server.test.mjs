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
  const server = spawn(process.execPath, [new URL("mock-server.mjs", import.meta.url).pathname], { env: { ...process.env, MOCK_PORT: "0", MOCK_SEED: "none", MOCK_REPLY_MS: "10" }, stdio: ["ignore", "pipe", "inherit"] });
  const connections = [];
  try {
    const [output] = await once(server.stdout, "data");
    const port = output.toString().match(/127\.0\.0\.1:(\d+)/)?.[1];
    assert.ok(port, output.toString());
    const a = client(`ws://127.0.0.1:${port}/ws`); connections.push(a.ws);
    await once(a.ws, "open"); a.send({ type: "hello", auth: { static_token: "a" } });
    assert.equal((await a.next("hello_ok")).threads.length, 0);
    a.send({ type: "upload_blob", client_id: "upload", name: "list.txt", mime: "text/plain", data_b64: Buffer.from("milk").toString("base64") });
    const blob = (await a.next("blob_ok")).blob;
    a.send({ type: "get_blob_url", client_id: "url", blob_id: blob.id });
    const grant = await a.next("blob_url");
    assert.ok(grant.expires_at > Date.now() / 1000);
    assert.equal(await (await fetch(`http://127.0.0.1:${port}${grant.url}`)).text(), "milk");
    assert.equal((await fetch(`http://127.0.0.1:${port}/blob/${blob.id}?token=a`)).status, 403);
    a.send({ type: "create_thread", parent_thread_id: null, client_id: "create", title: "Buy groceries" });
    const thread = (await a.next("thread_created")).thread;
    assert.equal(thread.attention, "quiet");
    a.send({ type: "create_thread", parent_thread_id: null, client_id: "create", title: "Buy groceries" });
    assert.equal((await a.next("thread_created")).thread.id, thread.id);
    a.send({ type: "send_thread_message", artifact_ids: [], client_id: "message", thread_id: thread.id, body: "Milk", attachments: [], mentions: [] });
    const owner = (await a.next("msg", frame => frame.message.author === "owner")).message;
    await a.next("thread_turn", frame => frame.turn.state === "completed");
    a.send({ type: "send_thread_message", artifact_ids: [], client_id: "message", thread_id: thread.id, body: "Milk" });
    assert.equal((await a.next("msg", frame => frame.message.author === "owner")).message.id, owner.id);
    a.send({ type: "thread_action", thread_id: thread.id, action: "read" });
    assert.equal((await a.next("thread_upsert", frame => frame.thread.read)).thread.settled_at, null);
    a.send({ type: "thread_action", thread_id: thread.id, action: "settle" });
    await a.next("thread_upsert", frame => frame.thread.settled_at !== null);
    a.ws.close();
    const b = client(`ws://127.0.0.1:${port}/ws`); connections.push(b.ws);
    await once(b.ws, "open"); b.send({ type: "hello", auth: { static_token: "a" } });
    const hello = await b.next("hello_ok");
    assert.ok(hello.threads.find(row => row.id === thread.id).settled_at);
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
