#!/usr/bin/env node
// Ephemeral development host for the authoritative Thread protocol.
// Same-token reconnects retain state until restart. No provider calls are made.
import { createServer } from "node:http";
import { createHash, randomUUID } from "node:crypto";
import { WebSocketServer } from "ws";

const port = Number(process.env.MOCK_PORT ?? 8787);
const replyDelay = Number(process.env.MOCK_REPLY_MS ?? 300);
const tenants = new Map();
const blobGrants = new Map();
const now = () => new Date().toISOString();
const accepts = token => typeof token === "string" && token.length > 0 && (!process.env.MOCK_TOKEN || token === process.env.MOCK_TOKEN);
const send = (ws, frame) => { if (ws.readyState === ws.OPEN) ws.send(JSON.stringify(frame)); };
const broadcast = (world, frame) => {
  const outgoing = frame.type === "thread_upsert" ? { ...frame, thread: summary(world, frame.thread) } : frame;
  for (const ws of world.clients) send(ws, outgoing);
  const id = frame.type === "msg" ? frame.message.thread_id : frame.type === "thread_turn" ? frame.turn.thread_id : null;
  if (id !== null) broadcast(world, { type: "thread_upsert", thread: threadFor(world, id) });
};
function makeThread(id, title, parent_thread_id = null) {
  return { id, title, icon: null, showcased_artifact_id: null, parent_thread_id, pinned_at: null, description: "", instrument: null, attention: "quiet", settled_at: null, archived_at: null, snoozed_until: null, read: false, created_at: now(), updated_at: now(), revision: 1, running_turn: null, queued_turn_count: 0, last_finished_turn: null, last_activity_at: now() };
}
function worldFor(token) {
  if (!tenants.has(token)) {
    const threads = [];
    if (process.env.MOCK_SEED !== "none") threads.push(makeThread(1, "Buy groceries"));
    tenants.set(token, { token, history_id: randomUUID(), threads, messages: [], relatedItems: [], nextRelatedItem: 1, relatedReceipts: new Map(), artifacts: [], artifactOperations: new Map(), nextArtifact: 1, turns: [], activities: [], clients: new Set(), requests: new Map(), blobs: new Map(), timers: new Map(), queue: [], nextThread: Math.max(0, ...threads.map(thread => thread.id)) + 1, nextMessage: 1, nextTurn: 1 });
  }
  return tenants.get(token);
}
function summary(world, thread) {
  const turns = world.turns.filter(turn => turn.thread_id === thread.id);
  const terminal = turns.filter(turn => turn.finished_at).sort((a, b) => Date.parse(b.finished_at) - Date.parse(a.finished_at) || b.id - a.id);
  const times = [thread.created_at, ...world.messages.filter(row => row.thread_id === thread.id).map(row => row.ts), ...world.activities.filter(row => row.thread_id === thread.id).map(row => row.ts), ...turns.flatMap(turn => [turn.started_at, turn.finished_at]).filter(Boolean)];
  return { ...thread, running_turn: turns.find(turn => turn.state === "running") ?? null, queued_turn_count: turns.filter(turn => turn.state === "queued").length, last_finished_turn: terminal[0] ?? null, last_activity_at: new Date(Math.max(...times.map(Date.parse))).toISOString() };
}
function threadFor(world, id) {
  const thread = world.threads.find(row => row.id === id);
  if (!thread) throw new Error(`Thread #${id} does not exist`);
  return thread;
}
function updateThread(world, thread, patch) {
  Object.assign(thread, patch, { revision: thread.revision + 1, updated_at: now() });
  broadcast(world, { type: "thread_upsert", thread });
}
function addMessage(world, threadId, author, body, extra = {}) {
  const message = { id: world.nextMessage++, thread_id: threadId, author, body, ref: null, attachments: [], tool_calls: [], mentions: [], ts: now(), ...extra };
  world.messages.push(message);
  broadcast(world, { type: "msg", message });
  return message;
}
function startTurn(world, message) {
  const turn = { id: world.nextTurn++, thread_id: message.thread_id, owner_message_id: message.id, requester_thread_id: threadFor(world, message.thread_id).parent_thread_id, requester_turn_id: null, agent_message_id: null, state: "queued", started_at: now(), finished_at: null };
  world.turns.push(turn);
  world.queue.push({ turn, message });
  broadcast(world, { type: "thread_turn", turn });
  runNext(world);
}
function runNext(world) {
  if (world.turns.some(turn => turn.state === "running")) return;
  const next = world.queue.shift();
  if (!next) return;
  const { turn, message } = next;
  turn.state = "running";
  broadcast(world, { type: "thread_turn", turn });
  broadcast(world, { type: "turn_event", thread_id: turn.thread_id, turn_id: turn.id, seq: 1, event: { kind: "prose", text: "Checking this thread…" } });
  const timer = setTimeout(() => {
    world.timers.delete(turn.id);
    const reply = addMessage(world, turn.thread_id, "agent", `Received in thread #${turn.thread_id}: ${message.body}`, { ref: message.id });
    Object.assign(turn, { state: "completed", agent_message_id: reply.id, finished_at: now() });
    broadcast(world, { type: "thread_turn", turn });
    runNext(world);
  }, replyDelay);
  world.timers.set(turn.id, timer);
}
function artifactSummary(world, artifact) {
  const { content: _content, ...summary } = artifact;
  return { ...summary, thread_ids: [...new Set([...[...world.messages, ...world.activities].filter(message => message.artifact_ids?.includes(artifact.id)).map(message => message.thread_id), ...world.threads.filter(thread => thread.showcased_artifact_id === artifact.id).map(thread => thread.id)])].sort((a,b)=>a-b) };
}
function artifactFor(world,id) {
  const artifact=world.artifacts.find(row=>row.id===id);
  if(!artifact) throw new Error(`Artifact #${id} does not exist`);
  return artifact;
}
function publishArtifactFixture(world, request) {
  threadFor(world,request.thread_id);
  if(typeof request.operation_id!=="string" || !request.operation_id.length) throw new Error("operation_id is required");
  const input=createHash("sha256").update(JSON.stringify(request)).digest("hex");
  const prior=world.artifactOperations.get(request.operation_id);
  if(prior) {
    if(prior.input!==input) throw new Error("operation_id already used for different input");
    return {...artifactSummary(world,artifactFor(world,prior.id)),content:artifactFor(world,prior.id).content};
  }
  const draft=request.draft;
  if(draft && (!draft.title?.trim() || !["solid","html","file"].includes(draft.kind) || typeof draft.content!=="string" || Buffer.byteLength(draft.content)>1048576)) throw new Error("Invalid artifact fixture");
  if(!request.artifact_id && !draft) throw new Error("Artifact draft required");
  let artifact;
  if(request.artifact_id) {
    artifact=artifactFor(world,request.artifact_id);
    if(draft) Object.assign(artifact,{title:draft.title,kind:draft.kind,mime:draft.mime,filename:draft.filename??null,content:draft.content,updated_at:now()});
  } else {
    artifact={id:world.nextArtifact++,title:draft.title,kind:draft.kind,mime:draft.mime,filename:draft.filename??null,content:draft.content,created_at:now(),updated_at:now()};
    world.artifacts.push(artifact);
  }
  // The reference exists before the upsert so every client's backlinks agree.
  const message={id:world.nextMessage++,thread_id:request.thread_id,author:"agent",body:`Artifact: ${artifact.title}`,ref:null,attachments:[],tool_calls:[],mentions:[],artifact_ids:[artifact.id],ts:now()};
  world.messages.push(message);
  world.artifactOperations.set(request.operation_id,{input,id:artifact.id});
  broadcast(world,{type:"artifact_upsert",artifact:artifactSummary(world,artifact)});
  broadcast(world,{type:"msg",message});
  return {...artifactSummary(world,artifact),content:artifact.content};
}
function handle(world, ws, frame) {
  switch (frame.type) {
    case "list_artifacts": {
      if(frame.thread_id!=null) threadFor(world,frame.thread_id);
      const artifacts=world.artifacts.map(row=>artifactSummary(world,row)).filter(row=>frame.thread_id==null || row.thread_ids.includes(frame.thread_id));
      send(ws,{type:"artifacts_listed",client_id:frame.client_id,artifacts}); return;
    }
    case "open_artifact": {
      const artifact=artifactFor(world,frame.artifact_id);
      send(ws,{type:"artifact_opened",client_id:frame.client_id,artifact:{...artifactSummary(world,artifact),content:artifact.content}}); return;
    }
    case "create_thread": {
      if (frame.history_id !== world.history_id) throw new Error("History changed. Open the Thread again.");
      if (!("parent_thread_id" in frame) || (frame.parent_thread_id !== null && !Number.isSafeInteger(frame.parent_thread_id))) throw new Error("parent_thread_id is required and must be null or an ID");
      if (frame.parent_thread_id !== null) threadFor(world, frame.parent_thread_id);
      if (!frame.title?.trim()) throw new Error("Thread title must not be empty");
      const prior = world.requests.get(frame.client_id);
      if (prior) { if (prior.type !== "thread_created" || prior.thread.title !== frame.title.trim() || prior.thread.parent_thread_id !== frame.parent_thread_id) throw new Error("client_id already used"); send(ws, prior); return; }
      const thread = makeThread(world.nextThread++, frame.title.trim(), frame.parent_thread_id);
      world.threads.push(thread);
      const result = { type: "thread_created", client_id: frame.client_id, thread };
      world.requests.set(frame.client_id, result);
      broadcast(world, { type: "thread_upsert", thread });
      send(ws, result);
      return;
    }
    case "open_thread": {
      const thread = threadFor(world, frame.thread_id);
      const rows = world.messages.filter(row => row.thread_id === thread.id && (frame.before_id == null || row.id < frame.before_id));
      send(ws, { type: "thread_opened", client_id: frame.client_id, detail: { related_items: world.relatedItems.filter(item => item.thread_id === thread.id), brief: { text: "", artifact_ids: [] }, thread: summary(world, thread), messages: rows.slice(-100), turns: world.turns.filter(row => row.thread_id === thread.id), activities: world.activities.filter(row => row.thread_id === thread.id), has_more: rows.length > 100 } });
      return;
    }
    case "add_thread_related":
    case "remove_thread_related": {
      if (frame.history_id !== world.history_id) throw new Error("History changed. Open the Thread again.");
      const thread = threadFor(world, frame.thread_id);
      let target, title = null;
      if (frame.type === "add_thread_related") {
        target = frame.target;
        if (target?.kind === "url") {
          // eslint-disable-next-line no-control-regex
          if (typeof target.url !== "string" || !/^https?:\/\//i.test(target.url) || /[\s\u0000-\u001f\u007f]/u.test(target.url) || Buffer.byteLength(target.url) > 4096) throw new Error("Invalid web URL");
          const url = new URL(target.url);
          if (!url.hostname || url.username || url.password || Buffer.byteLength(url.href) > 4096) throw new Error("Invalid web URL");
          target = { kind: "url", url: url.href }; title = frame.title?.trim() || null;
          // eslint-disable-next-line no-control-regex
          if (title && ([...title].length > 200 || /[\u0000-\u001f\u007f]/u.test(title))) throw new Error("Invalid item title");
        } else if (target?.kind === "thread") {
          if (target.history_id !== world.history_id) throw new Error("Thread reference belongs to another history");
          threadFor(world, target.thread_id);
          if (frame.title != null) throw new Error("Thread references use their current title");
          target = { kind: "thread", history_id: world.history_id, thread_id: target.thread_id };
        } else throw new Error("Unknown Related target");
      }
      const payload = JSON.stringify([frame.type, frame.history_id, thread.id, target, title, frame.item_id]);
      const receipt = world.relatedReceipts.get(frame.client_id);
      if (receipt !== undefined && receipt !== payload) throw new Error("client_id already used for different content");
      let changed = false;
      if (receipt === undefined) {
        if (frame.type === "add_thread_related") {
          if (!world.relatedItems.some(item => item.thread_id === thread.id && JSON.stringify(item.target) === JSON.stringify(target))) {
            if (world.relatedItems.filter(item => item.thread_id === thread.id).length >= 100) throw new Error("This Thread already has 100 saved references");
            world.relatedItems.push({ id: world.nextRelatedItem++, thread_id: thread.id, target, title, created_at: now() }); changed = true;
          }
        } else {
          const count = world.relatedItems.length;
          world.relatedItems = world.relatedItems.filter(item => item.thread_id !== thread.id || item.id !== frame.item_id);
          changed = count !== world.relatedItems.length;
        }
        world.relatedReceipts.set(frame.client_id, payload);
      }
      if (changed) updateThread(world, thread, {});
      broadcast(world, { type: "thread_related_changed", client_id: frame.client_id, history_id: world.history_id, thread_id: thread.id, revision: thread.revision, items: world.relatedItems.filter(item => item.thread_id === thread.id) });
      return;
    }
    case "send_thread_message": {
      if (frame.history_id !== world.history_id) throw new Error("History changed. Open the Thread again.");
      threadFor(world, frame.thread_id);
      if (!Array.isArray(frame.artifact_ids)) throw new Error("artifact_ids is required");
      const references = [...new Set(frame.artifact_ids)].sort((a,b) => a-b);
      if (references.length > 16 || references.some(id => !Number.isSafeInteger(id) || id < 0)) throw new Error("Invalid artifact references");
      for (const id of references) artifactFor(world, id);
      for (const id of frame.mentions ?? []) threadFor(world, id);
      const prior = world.requests.get(frame.client_id);
      if (prior) {
        if (prior.type !== "msg" || prior.message.thread_id !== frame.thread_id || prior.message.body !== frame.body || JSON.stringify(prior.message.artifact_ids) !== JSON.stringify(references)) throw new Error("client_id already used for different content");
        send(ws, prior); return;
      }
      const attachments = (frame.attachments ?? []).map(id => {
        const blob = world.blobs.get(id);
        if (!blob) throw new Error(`Unknown attachment ${id}`);
        return blob.info;
      });
      if (!frame.body?.trim() && attachments.length === 0) throw new Error("Message must not be empty");
      const message = addMessage(world, frame.thread_id, "owner", frame.body, { client_id: frame.client_id, attachments, mentions: frame.mentions ?? [], artifact_ids: references });
      world.requests.set(frame.client_id, { type: "msg", message });
      for (const id of references) broadcast(world, { type: "artifact_upsert", artifact: artifactSummary(world, artifactFor(world, id)) });
      startTurn(world, message);
      return;
    }
    case "thread_action": {
      if (frame.history_id !== world.history_id) throw new Error("History changed. Open the Thread again.");
      const thread = threadFor(world, frame.thread_id);
      if (frame.action === "pin" && thread.parent_thread_id !== null) throw new Error("Only top-level threads can be pinned");
      if (["pin", "unpin", "set_icon", "set_showcase"].includes(frame.action) && frame.expected_revision !== thread.revision) throw new Error("Thread changed; retry with its current revision");
      if (frame.action === "set_showcase") {
        if (!Object.hasOwn(frame.data, "artifact_id") || Object.keys(frame.data).length !== 1) throw new Error("artifact_id is required and must be the only field");
        const id = frame.data.artifact_id;
        if (id !== null && (!Number.isSafeInteger(id) || id < 1)) throw new Error("Invalid artifact ID");
        if (id !== null) artifactFor(world, id);
        const previous = thread.showcased_artifact_id;
        if (id === previous) return;
        updateThread(world, thread, { showcased_artifact_id: id });
        for (const affected of new Set([previous, id])) if (affected != null) {
          const artifact = artifactFor(world, affected); artifact.updated_at = now();
          broadcast(world, { type: "artifact_upsert", artifact: artifactSummary(world, artifact) });
        }
        return;
      }
      if (frame.action === "set_icon") {
        const patch = {};
        if (!Object.hasOwn(frame.data ?? {}, "icon")) return;
        if (Object.hasOwn(frame.data ?? {}, "icon")) {
          const icon = frame.data.icon;
          if (icon !== null && (typeof icon !== "string" || !icon.trim() || /\p{Cc}|\u2028|\u2029/u.test(icon) || Array.from(icon).length > 16 || Buffer.byteLength(icon, "utf8") > 64)) throw new Error("Invalid thread icon");
          patch.icon = icon;
        }
        updateThread(world, thread, patch); return;
      }
      const patches = { pin: { pinned_at: thread.pinned_at ?? now() }, unpin: { pinned_at: null }, settle: { settled_at: now() }, reopen: { settled_at: null }, archive: { archived_at: now() }, unarchive: { archived_at: null }, read: { read: true }, snooze: { snoozed_until: frame.data?.until }, unsnooze: { snoozed_until: null } };
      if (patches[frame.action]) { updateThread(world, thread, patches[frame.action]); return; }
      if (frame.expected_revision !== thread.revision) throw new Error("This instrument has changed. Refresh before acting.");
      throw new Error("This mock thread has no generated action; use the scripted Rust host to test instruments.");
    }
    case "cancel_turn":
      if (frame.history_id !== world.history_id) throw new Error("History changed. Open the Thread again.");
      for (const turn of world.turns.filter(row => row.thread_id === frame.thread_id && row.state === "running")) {
        clearTimeout(world.timers.get(turn.id)); world.timers.delete(turn.id);
        Object.assign(turn, { state: "cancelled", finished_at: now() });
        broadcast(world, { type: "thread_turn", turn });
      }
      runNext(world);
      return;
    case "upload_blob": {
      const prior = world.requests.get(frame.client_id);
      if (prior) { send(ws, prior); return; }
      const buffer = Buffer.from(frame.data_b64, "base64");
      if (buffer.length > 15 * 1024 * 1024) throw new Error("Attachment exceeds 15 MiB");
      const info = { id: randomUUID(), name: frame.name, mime: frame.mime, size: buffer.length };
      world.blobs.set(info.id, { info, buffer });
      const result = { type: "blob_ok", client_id: frame.client_id, blob: info };
      world.requests.set(frame.client_id, result); send(ws, result); return;
    }
    case "get_blob_url": {
      const blob = world.blobs.get(frame.blob_id);
      if (!blob) throw new Error("Unknown attachment");
      const ticket = randomUUID();
      const expires = Math.floor(Date.now() / 1000) + 300;
      blobGrants.set(ticket, { blob, expires });
      send(ws, { type: "blob_url", client_id: frame.client_id, blob_id: frame.blob_id, url: `/blob/${frame.blob_id}?ticket=${ticket}`, expires_at: expires }); return;
    }
    default: throw new Error(`Unsupported mock command: ${frame.type}`);
  }
}
const server = createServer(async (req, res) => {
  const url = new URL(req.url, `http://${req.headers.host}`);
  if (url.pathname.startsWith("/blob/")) {
    const grant = blobGrants.get(url.searchParams.get("ticket"));
    if (!grant || grant.expires < Date.now() / 1000 || grant.blob.info.id !== decodeURIComponent(url.pathname.slice("/blob/".length))) { res.writeHead(403); res.end("forbidden"); return; }
    res.writeHead(200, { "Content-Type": grant.blob.info.mime, "Cache-Control": "no-store" }); res.end(grant.blob.buffer); return;
  }
  const token = req.headers.authorization?.replace(/^Bearer /, "");
  if (!accepts(token)) { res.writeHead(401); res.end("unauthorized"); return; }
  if (url.pathname === "/debug/publish-artifact" && req.method === "POST") {
    try {
      let bytes=0; const chunks=[];
      for await(const chunk of req) { bytes+=chunk.length; if(bytes>2*1048576) throw new Error("Fixture too large"); chunks.push(chunk); }
      const artifact=publishArtifactFixture(worldFor(token),JSON.parse(Buffer.concat(chunks).toString()));
      res.writeHead(200,{"Content-Type":"application/json"}); res.end(JSON.stringify(artifact));
    } catch(error) { res.writeHead(400,{"Content-Type":"application/json"}); res.end(JSON.stringify({error:error.message})); }
    return;
  }
  if (url.pathname === "/api/plugins" && req.method === "GET") {
    res.writeHead(200, { "Content-Type": "application/json" }); res.end('{"plugins":[]}'); return;
  }
  res.writeHead(404); res.end("not found");
});
new WebSocketServer({ server }).on("connection", ws => {
  let world;
  ws.on("close", () => world?.clients.delete(ws));
  ws.on("message", raw => {
    let frame;
    try {
      frame = JSON.parse(raw.toString());
      if (!world) {
        if (frame.type !== "hello" || !accepts(frame.auth?.static_token)) { send(ws, { type: "error", detail: "Authenticated hello required" }); ws.close(1008); return; }
        world = worldFor(frame.auth.static_token); world.clients.add(ws);
        send(ws, { type: "hello_ok", history_id: world.history_id, model: null, subagent_models: null, prompts: null, providers: null, threads: world.threads.map(thread => summary(world, thread)), processes: [], views: [], host_version: "thread-development-mock" });
        return;
      }
      handle(world, ws, frame);
    } catch (error) { send(ws, { type: "error", client_id: frame?.client_id ?? null, detail: error.message }); }
  });
});
server.listen(port, "127.0.0.1", () => console.log(`[mock-server] Thread host listening on 127.0.0.1:${server.address().port}`));
