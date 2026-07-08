#!/usr/bin/env node
// Dev harness: a tiny in-memory WS server implementing PROTOCOL.md, with a
// scripted "agent" so the PWA can be developed/demoed without the Rust host.
// Not durable, not multi-session-aware beyond client_id dedupe - restart
// resets all state. Run via `npm run dev:mock` (mock server + vite together)
// or `npm run mock-server` alone.
import { WebSocketServer } from "ws";

const PORT = Number(process.env.MOCK_PORT ?? 8787);
const TOKEN = process.env.MOCK_TOKEN ?? "dev-token";
const REPLAY_LIMIT = 200;
const ARCHIVED_REPLAY_LIMIT = 20;

/** @type {{id:number, author:'owner'|'agent', body:string, ref:number|null, ts:string}[]} */
const messages = [];
/** @type {{id:number, content:string, anchor:number, requires_response:boolean, quick_replies:{value:string,label:string}[], status:'open'|'archived', ts:string}[]} */
const inbox = [];
let nextMsgId = 1;
let nextInboxId = 1;
/** client_id -> assigned message id, so resends after reconnect dedupe. */
const seenClientIds = new Map();

const clients = new Set();

function now() {
  return new Date().toISOString();
}

function log(...args) {
  console.log(`[mock-server]`, ...args);
}

function broadcast(frame) {
  const json = JSON.stringify(frame);
  for (const ws of clients) {
    if (ws.readyState === ws.OPEN) ws.send(json);
  }
}

function addMessage(author, body, ref) {
  const message = { id: nextMsgId++, author, body, ref: ref ?? null, ts: now() };
  messages.push(message);
  broadcast({ type: "msg", message });
  return message;
}

function setActivity(state, text) {
  broadcast({ type: "agent_activity", state, text: text ?? null });
}

function upsertInbox(item) {
  const idx = inbox.findIndex((i) => i.id === item.id);
  if (idx === -1) inbox.push(item);
  else inbox[idx] = item;
  broadcast({ type: "inbox_upsert", item });
}

function findOpenInboxByAnchor(anchorId) {
  return inbox.find((i) => i.status === "open" && i.anchor === anchorId);
}

/** Any reply that refs an open Inbox Item's anchor gets a generic
 * acknowledgement + the item archives - independent of the delegate script,
 * since this is how every quick-reply / anchor-refed reply resolves. */
function acknowledgeInboxResponse(item, ownerMessage) {
  setActivity("thinking", "Noting your response…");
  setTimeout(() => {
    addMessage("agent", `Got it — noted: "${ownerMessage.body}".`, ownerMessage.id);
    setActivity("idle", null);
    upsertInbox({ ...item, status: "archived" });
  }, 1000);
}

function runDelegateScript(ownerMessage) {
  setActivity("thinking", "Delegating to a sub-agent…");
  setTimeout(() => {
    const reply = addMessage(
      "agent",
      "On it — kicking off a sub-agent for that. I'll update you here when it's done.",
      ownerMessage.id,
    );
    setActivity("idle", null);

    setTimeout(() => {
      const item = {
        id: nextInboxId++,
        content:
          "**Sub-agent finished the delegated task.**\n\nDiff is ready — approve to merge, or reject to discard.",
        anchor: reply.id,
        requires_response: true,
        quick_replies: [
          { value: "approve", label: "Approve" },
          { value: "reject", label: "Reject" },
        ],
        status: "open",
        ts: now(),
      };
      upsertInbox(item);
    }, 3000);
  }, 1000);
}

function runEchoScript(ownerMessage) {
  setActivity("thinking", "Thinking…");
  setTimeout(() => {
    addMessage("agent", `Echo: ${ownerMessage.body}`, ownerMessage.id);
    setActivity("idle", null);
  }, 1000);
}

function handleSendMessage(frame) {
  const already = seenClientIds.get(frame.client_id);
  if (already !== undefined) {
    // Resend after reconnect: host already has this one, just re-affirm it
    // rather than creating a duplicate or re-running the scripted agent.
    const existing = messages.find((m) => m.id === already);
    if (existing) broadcast({ type: "msg", message: existing });
    return;
  }

  const message = addMessage("owner", frame.body, frame.ref);
  seenClientIds.set(frame.client_id, message.id);

  if (message.ref !== null) {
    const item = findOpenInboxByAnchor(message.ref);
    if (item) {
      acknowledgeInboxResponse(item, message);
      return;
    }
  }

  if (message.body.trim().toLowerCase() === "delegate") {
    runDelegateScript(message);
  } else {
    runEchoScript(message);
  }
}

function handleArchiveItem(frame) {
  const item = inbox.find((i) => i.id === frame.item_id);
  if (!item || item.status === "archived") return;
  upsertInbox({ ...item, status: "archived" });
}

const wss = new WebSocketServer({ port: PORT });

wss.on("connection", (ws) => {
  let helloed = false;
  clients.add(ws);
  log("connection opened");

  ws.on("close", () => {
    clients.delete(ws);
    log("connection closed");
  });

  ws.on("message", (raw) => {
    let frame;
    try {
      frame = JSON.parse(raw.toString());
    } catch {
      ws.send(JSON.stringify({ type: "error", detail: "invalid JSON" }));
      ws.close(1002, "invalid JSON");
      return;
    }

    if (!helloed) {
      if (frame.type !== "hello") {
        ws.send(JSON.stringify({ type: "error", detail: "hello must be the first frame" }));
        ws.close(1002, "expected hello first");
        return;
      }
      if (frame.token !== TOKEN) {
        ws.send(JSON.stringify({ type: "error", detail: "invalid token" }));
        ws.close(1008, "invalid token");
        return;
      }

      helloed = true;
      const lastSeen = frame.last_seen_msg_id;
      const replayMessages =
        lastSeen === null || lastSeen === undefined
          ? messages.slice(-REPLAY_LIMIT)
          : messages.filter((m) => m.id > lastSeen);
      const openItems = inbox.filter((i) => i.status === "open");
      const archivedItems = inbox
        .filter((i) => i.status === "archived")
        .slice(-ARCHIVED_REPLAY_LIMIT);

      ws.send(
        JSON.stringify({
          type: "hello_ok",
          latest_msg_id: nextMsgId - 1,
          messages: replayMessages,
          inbox: [...openItems, ...archivedItems],
        }),
      );
      log("hello ok, replayed", replayMessages.length, "messages");
      return;
    }

    switch (frame.type) {
      case "send_message":
        handleSendMessage(frame);
        break;
      case "archive_item":
        handleArchiveItem(frame);
        break;
      case "hello":
        ws.send(JSON.stringify({ type: "error", detail: "hello already sent" }));
        break;
      default:
        ws.send(JSON.stringify({ type: "error", detail: `unknown frame type: ${frame.type}` }));
    }
  });
});

log(`listening on ws://localhost:${PORT} (token: ${TOKEN})`);
