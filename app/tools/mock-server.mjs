#!/usr/bin/env node
// Dev harness: a tiny in-memory host implementing PROTOCOL.md (v1 through
// v2.0 side chats / ADR-0008), with a scripted "agent" so the PWA can be
// developed/demoed without the Rust host. Not durable — restart resets all
// state. Serves blob content over HTTP on the same port the WS runs on.
// Run via `npm run dev:mock` (mock + vite together) or `npm run mock-server`.
import { createServer } from "node:http";
import { randomUUID } from "node:crypto";
import { WebSocketServer } from "ws";

const PORT = Number(process.env.MOCK_PORT ?? 8787);
const TOKEN = process.env.MOCK_TOKEN ?? "dev-token";
const REPLAY_LIMIT = 200;
const ARCHIVED_REPLAY_LIMIT = 20;
const MAX_BLOB_BYTES = 15 * 1024 * 1024;
const REPLY_DELAY_MS = Number(process.env.MOCK_REPLY_MS ?? 1200);
const SIDECHAT_TTL_MS = Number(process.env.HIRSEL_SIDECHAT_TTL_SECS ?? 86400) * 1000;

/** @type {{id:number, author:'owner'|'agent', body:string, ref:number|null, ts:string, attachments:object[], tool_calls:object[]}[]} */
const messages = [];
const inbox = [];
/** @type {{id:string, kind:'subagent'|'monitor', label:string, agent:string|null, model:string|null, state:string, started_ts:string, last_event_ts:string, summary:string|null}[]} */
const processes = [];
let nextMsgId = 1;
let nextInboxId = 1;
let nextProcSeq = 1;
const seenClientIds = new Map(); // client_id -> assigned message id
const blobs = new Map(); // blob id -> { id, name, mime, size, buffer }

// --- side chats (v2.0 / ADR-0008) -------------------------------------------
/** @type {Map<string, {sc:string, itemId:number, messages:object[], nextMsgId:number, turnActive:boolean, turnTimers:NodeJS.Timeout[], ttlTimer:NodeJS.Timeout|null}>} */
const sideChats = new Map(); // sc -> side chat state
const sideChatByItem = new Map(); // item_id -> sc (idempotent open/resume, one live side chat per item)
const sideSeenClientIds = new Map(); // client_id -> { sc, messageId } (per-side-chat send idempotency)

// --- turn model (for v1.2 mode/cancel) --------------------------------------
let turnActive = false;
let turnTimers = [];
/** Messages sent with mode:"next_turn" while a turn was active, awaiting their
 * own turn. Each: { clientId, messageId }. "Claimed" once dequeued. */
let queuedNextTurn = [];

const clients = new Set();

function now() {
  return new Date().toISOString();
}
function log(...args) {
  console.log(`[mock-server]`, ...args);
}
function broadcast(frame) {
  const json = JSON.stringify(frame);
  for (const ws of clients) if (ws.readyState === ws.OPEN) ws.send(json);
}

function addMessage(author, body, ref, attachments = [], toolCalls = []) {
  const message = {
    id: nextMsgId++,
    author,
    body,
    ref: ref ?? null,
    ts: now(),
    attachments,
    tool_calls: toolCalls,
  };
  messages.push(message);
  broadcast({ type: "msg", message });
  return message;
}
function setActivity(state, text) {
  broadcast({ type: "agent_activity", state, text: text ?? null });
}

const TERMINAL_STATES = new Set(["done", "failed", "cancelled", "abandoned"]);

/** Upsert a process by id and broadcast the change (v1.4). */
function upsertProcess(patch) {
  const idx = processes.findIndex((p) => p.id === patch.id);
  const base =
    idx === -1
      ? { agent: null, model: null, summary: null, started_ts: now(), last_event_ts: now() }
      : processes[idx];
  const proc = { ...base, ...patch, last_event_ts: patch.last_event_ts ?? now() };
  if (idx === -1) processes.push(proc);
  else processes[idx] = proc;
  broadcast({ type: "process_upsert", process: proc });
  return proc;
}

/** Emit one ephemeral timeline event for the running turn (v1.5). */
function emitTurnEvent(seq, event) {
  broadcast({ type: "turn_event", seq, event });
}

/** hello_ok processes slice: all non-terminal + the last 10 terminal. */
function processesForHello() {
  const nonTerminal = processes.filter((p) => !TERMINAL_STATES.has(p.state));
  const terminal = processes.filter((p) => TERMINAL_STATES.has(p.state)).slice(-10);
  return [...nonTerminal, ...terminal];
}
function upsertInbox(item) {
  const idx = inbox.findIndex((i) => i.id === item.id);
  if (idx === -1) inbox.push(item);
  else inbox[idx] = item;
  broadcast({ type: "ping_upsert", ping: item });
}
function findOpenInboxByAnchor(anchorId) {
  return inbox.find((i) => i.status === "open" && i.anchor === anchorId);
}

/** hello_ok side_chats slice: just the {sc, ping_id} refs (v2.0). */
function sideChatsForHello() {
  return [...sideChats.values()].map((s) => ({ sc: s.sc, ping_id: s.itemId }));
}

/** (Re)arm the TTL-close timer for a side chat; any activity resets the clock. */
function armSideTtl(sc) {
  const sideChat = sideChats.get(sc);
  if (!sideChat) return;
  if (sideChat.ttlTimer) clearTimeout(sideChat.ttlTimer);
  sideChat.ttlTimer = setTimeout(() => closeSideChat(sc, "ttl-reap"), SIDECHAT_TTL_MS);
}

/** Tear down a side chat and tell the client, regardless of why (confirm,
 * discard, or a TTL reap) — the client distinguishes "expected" from
 * "host-initiated" by whether IT asked for the close (see the reducer). */
function closeSideChat(sc, reason) {
  const sideChat = sideChats.get(sc);
  if (!sideChat) return;
  for (const t of sideChat.turnTimers) clearTimeout(t);
  if (sideChat.ttlTimer) clearTimeout(sideChat.ttlTimer);
  sideChats.delete(sc);
  sideChatByItem.delete(sideChat.itemId);
  broadcast({ type: "side_chat_closed", sc });
  log("side chat closed:", sc, `(${reason})`);
}

function addSideMessage(sideChat, author, body, ref = null) {
  const message = { id: sideChat.nextMsgId++, author, body, ref, ts: now(), attachments: [], tool_calls: [] };
  sideChat.messages.push(message);
  broadcast({ type: "msg", message, sc: sideChat.sc });
  return message;
}
function setSideActivity(sideChat, state, text) {
  broadcast({ type: "agent_activity", state, text: text ?? null, sc: sideChat.sc });
}
function emitSideTurnEvent(sideChat, seq, event) {
  broadcast({ type: "turn_event", seq, event, sc: sideChat.sc });
}
function laterSide(sideChat, fn, ms) {
  const t = setTimeout(fn, ms);
  sideChat.turnTimers.push(t);
  return t;
}
function finishSideTurn(sideChat) {
  setSideActivity(sideChat, "idle", null);
  sideChat.turnActive = false;
  sideChat.turnTimers = [];
}

/** `open_side_chat` is idempotent per item (protocol v2.0): resuming answers
 * with the SAME sc + transcript so far; a fresh one gets messages: [] — the
 * seed (item + anchor + recent chat) lives in the side session's prompt
 * layer, never as fake transcript rows. */
function handleOpenSideChat(ws, frame) {
  const pingId = frame.ping_id;
  const existingSc = sideChatByItem.get(pingId);
  if (existingSc) {
    const sideChat = sideChats.get(existingSc);
    ws.send(
      JSON.stringify({
        type: "side_chat_open",
        sc: sideChat.sc,
        ping_id: sideChat.itemId,
        messages: sideChat.messages,
      }),
    );
    log("side chat resumed", sideChat.sc, "for ping", pingId);
    return;
  }
  const sc = `side:${randomUUID()}`;
  const sideChat = {
    sc,
    itemId: pingId,
    messages: [],
    nextMsgId: 1,
    turnActive: false,
    turnTimers: [],
    ttlTimer: null,
  };
  sideChats.set(sc, sideChat);
  sideChatByItem.set(pingId, sc);
  armSideTtl(sc);
  ws.send(JSON.stringify({ type: "side_chat_open", sc, ping_id: pingId, messages: [] }));
  log("side chat opened", sc, "for ping", pingId);
}

/** Scripted side-agent reply. Mirrors startReplyTurn's demo hooks (scoped to
 * this side chat) plus a debug-only "ttl-close" body that simulates a
 * host-side reap immediately, so the reconnect-gone / terminal-state path is
 * drivable without waiting out the real TTL. */
function startSideReplyTurn(sideChat, ownerMessage) {
  sideChat.turnActive = true;
  armSideTtl(sideChat.sc); // any activity resets the TTL clock
  const trimmed = ownerMessage.body.trim().toLowerCase();

  if (trimmed === "ttl-close") {
    sideChat.turnActive = false;
    closeSideChat(sideChat.sc, "ttl-close command");
    return;
  }

  if (trimmed === "timeline") {
    setSideActivity(sideChat, "thinking", "Working through it…");
    laterSide(
      sideChat,
      () => emitSideTurnEvent(sideChat, 1, { kind: "prose", text: "Let me check the seeded context. " }),
      300,
    );
    laterSide(
      sideChat,
      () => emitSideTurnEvent(sideChat, 2, { kind: "tool_start", id: "s1", name: "read_context", summary: null }),
      700,
    );
    laterSide(
      sideChat,
      () =>
        emitSideTurnEvent(sideChat, 3, {
          kind: "tool_done",
          id: "s1",
          name: "read_context",
          ok: true,
          summary: "item + anchor + last 20 messages",
        }),
      1300,
    );
    laterSide(
      sideChat,
      () => emitSideTurnEvent(sideChat, 4, { kind: "prose", text: "Here's a reasonable take on it." }),
      1700,
    );
    laterSide(
      sideChat,
      () => {
        addSideMessage(sideChat, "agent", "Here's a reasonable take on it.", ownerMessage.id);
        finishSideTurn(sideChat);
      },
      2000,
    );
    return;
  }

  setSideActivity(sideChat, "thinking", "Thinking…");
  laterSide(
    sideChat,
    () => {
      addSideMessage(sideChat, "agent", `Echo (side): ${ownerMessage.body || "(no text)"}`, ownerMessage.id);
      finishSideTurn(sideChat);
    },
    REPLY_DELAY_MS,
  );
}

function handleSideSendMessage(frame) {
  const sideChat = sideChats.get(frame.sc);
  if (!sideChat) return; // unknown/already-closed scope — a real host errors; the mock just drops it.
  const already = sideSeenClientIds.get(frame.client_id);
  if (already) {
    const existing = sideChat.messages.find((m) => m.id === already.messageId);
    if (existing) broadcast({ type: "msg", message: existing, sc: sideChat.sc });
    return;
  }
  const message = addSideMessage(sideChat, "owner", frame.body, frame.ref ?? null);
  sideSeenClientIds.set(frame.client_id, { sc: frame.sc, messageId: message.id });
  if (!sideChat.turnActive) startSideReplyTurn(sideChat, message);
}

function handleSideCancelTurn(frame) {
  const sideChat = sideChats.get(frame.sc);
  if (!sideChat || !sideChat.turnActive) return;
  for (const t of sideChat.turnTimers) clearTimeout(t);
  sideChat.turnTimers = [];
  addSideMessage(sideChat, "agent", "_Turn cancelled._", null);
  finishSideTurn(sideChat);
}

/** A plausible-looking scripted draft: leans on the side chat's own last owner
 * line if there is one, otherwise the item content — never the empty string,
 * since the confirm sheet must always have something to show/edit. */
function draftConclusion(sideChat) {
  const item = inbox.find((i) => i.id === sideChat.itemId);
  const lastOwnerLine = [...sideChat.messages].reverse().find((m) => m.author === "owner");
  const base = lastOwnerLine ? lastOwnerLine.body : item ? item.content.replace(/\s+/g, " ") : "Sounds good.";
  return `Based on our side chat: ${base}`;
}

function handleConcludeSideChat(frame) {
  const sideChat = sideChats.get(frame.sc);
  if (!sideChat) return;
  setSideActivity(sideChat, "thinking", "Drafting your reply…");
  laterSide(
    sideChat,
    () => {
      const text = draftConclusion(sideChat);
      setSideActivity(sideChat, "idle", null);
      broadcast({ type: "conclusion_draft", sc: sideChat.sc, text });
    },
    REPLY_DELAY_MS,
  );
}

/** `confirm_conclusion`: post the Owner's (possibly-edited) reply as a normal
 * anchor-refed MAIN chat message (idempotent client_id keeps a resend from
 * double-posting), archive the item (idempotent — a no-op if the Agent
 * already archived it), then discard the side session + transcript. */
function handleConfirmConclusion(frame) {
  const sideChat = sideChats.get(frame.sc);
  if (!sideChat) return;
  const item = inbox.find((i) => i.id === sideChat.itemId);
  const clientId = `side-conclude:${frame.sc}`;
  if (!seenClientIds.has(clientId)) {
    const message = addMessage("owner", frame.text, item ? item.anchor : null);
    seenClientIds.set(clientId, message.id);
    if (item && item.status === "open") upsertInbox({ ...item, status: "done" });
  }
  closeSideChat(frame.sc, "concluded");
}

function handleDiscardSideChat(frame) {
  closeSideChat(frame.sc, "discarded");
}

function later(fn, ms) {
  const t = setTimeout(fn, ms);
  turnTimers.push(t);
  return t;
}
function clearTurnTimers() {
  for (const t of turnTimers) clearTimeout(t);
  turnTimers = [];
}

/** Finish the active turn, then drain the next_turn queue (one turn each). */
function finishTurn() {
  setActivity("idle", null);
  turnActive = false;
  turnTimers = [];
  drainQueue();
}

function drainQueue() {
  if (turnActive || queuedNextTurn.length === 0) return;
  const next = queuedNextTurn.shift(); // claim it
  log("claimed queued message", next.messageId);
  const message = messages.find((m) => m.id === next.messageId);
  if (message) startReplyTurn(message);
}

/** Kick off a scripted agent reply for an owner message. */
function startReplyTurn(ownerMessage) {
  turnActive = true;
  log("turn start", ownerMessage.id, JSON.stringify(ownerMessage.body.slice(0, 24)));
  // Test/demo hook: a long-running turn so queue/cancel windows are comfortable.
  if (ownerMessage.body.trim().toLowerCase() === "hold") {
    setActivity("thinking", "Working on a long task…");
    later(() => {
      addMessage("agent", "Done holding.", ownerMessage.id);
      finishTurn();
    }, 15000);
    return;
  }
  if (ownerMessage.body.trim().toLowerCase() === "delegate") {
    // Spawn a sub-agent Runtime Process that runs alongside the turn: it goes
    // running (with progress-summary updates) → done, broadcasting each step.
    const procId = `proc-${nextProcSeq++}`;
    const label = "Review the auth refactor and open a PR";
    upsertProcess({
      id: procId,
      kind: "subagent",
      label,
      agent: "code-reviewer",
      model: "gpt-5.5",
      state: "running",
      summary: "starting up…",
    });
    setActivity("thinking", "Delegating to a sub-agent…");
    later(() => {
      const reply = addMessage(
        "agent",
        "On it — kicking off a sub-agent for that. I'll update you here when it's done.",
        ownerMessage.id,
      );
      finishTurn();
      // The sub-agent keeps working after the turn commits.
      setTimeout(() => upsertProcess({ id: procId, summary: "reading changed files (3)…" }), 2000);
      setTimeout(() => upsertProcess({ id: procId, summary: "writing review notes…" }), 5000);
      setTimeout(() => {
        upsertProcess({ id: procId, state: "done", summary: "done — 3 files reviewed, PR opened" });
        // Independent follow-up: an inbox item lands when it finishes.
        upsertInbox({
          id: nextInboxId++,
          name: "review-diff",
          description: "Sub-agent finished — approve or reject the diff",
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
        });
      }, 8000);
    }, REPLY_DELAY_MS);
  } else if (ownerMessage.body.trim().toLowerCase() === "timeline") {
    // A scripted thinking window streaming a full v1.5 timeline — prose →
    // tool_start/done → prose → reasoning → prose — then committing a reply
    // stamped with the matching tool_calls summary (the turn-details fallback).
    setActivity("thinking", "Working through it…");
    later(() => emitTurnEvent(1, { kind: "prose", text: "Let me look into that. " }), 300);
    later(() => emitTurnEvent(2, { kind: "prose", text: "First I'll check the reducer." }), 700);
    later(
      () =>
        emitTurnEvent(3, {
          kind: "tool_start",
          id: "t1",
          name: "read_file",
          summary: "src/store/reducer.ts",
        }),
      1100,
    );
    later(
      () => emitTurnEvent(4, { kind: "tool_done", id: "t1", name: "read_file", ok: true, summary: "read 142 lines" }),
      1900,
    );
    later(
      () =>
        emitTurnEvent(5, {
          kind: "prose",
          text: "The reducer looks right — the handler is wired correctly.",
        }),
      2300,
    );
    later(
      () =>
        emitTurnEvent(6, {
          kind: "reasoning",
          text: "seq ordering guarantees the tool row lands between the two prose blocks even if frames arrive out of order.",
        }),
      2700,
    );
    later(
      () =>
        emitTurnEvent(7, {
          kind: "prose",
          text: "Everything checks out — no changes needed.",
        }),
      3100,
    );
    later(() => {
      addMessage(
        "agent",
        "Checked the reducer — the handler is wired correctly, no changes needed.",
        ownerMessage.id,
        [],
        [{ name: "read_file", ok: true }],
      );
      finishTurn();
    }, 3600);
  } else if (ownerMessage.body.trim().toLowerCase() === "monitor") {
    // Create a monitor Runtime Process (running), then have it "fire" once.
    const procId = `proc-${nextProcSeq++}`;
    upsertProcess({
      id: procId,
      kind: "monitor",
      label: "curl -sf https://api.example.com/health",
      agent: null,
      model: null,
      state: "running",
      summary: "every 60s — no wakes yet",
    });
    setActivity("thinking", "Setting up a monitor…");
    later(() => {
      addMessage(
        "agent",
        "Monitor is live — I'll ping you here if the health check starts failing.",
        ownerMessage.id,
      );
      finishTurn();
      // It fires a few seconds later (condition met).
      setTimeout(
        () =>
          upsertProcess({
            id: procId,
            summary: "every 60s — fired: exit 22, tail: 'HTTP 503 Service Unavailable'",
          }),
        4000,
      );
    }, REPLY_DELAY_MS);
  } else {
    setActivity("thinking", "Thinking…");
    later(() => {
      const noun = ownerMessage.attachments.length > 0 ? ` (+${ownerMessage.attachments.length} attachment${ownerMessage.attachments.length > 1 ? "s" : ""})` : "";
      addMessage("agent", `Echo: ${ownerMessage.body || "(no text)"}${noun}`, ownerMessage.id);
      finishTurn();
    }, REPLY_DELAY_MS);
  }
}

function acknowledgeInboxResponse(item, ownerMessage) {
  // ADR-0009: the Owner replying to an item's Anchor resolves it to `done`
  // automatically, host-side, the moment the reply lands — before (and
  // independent of) the Agent's acknowledgment turn. This is the mechanical
  // reply-resolves rule the client mirrors optimistically.
  if (item.status === "open") upsertInbox({ ...item, status: "done" });
  turnActive = true;
  setActivity("thinking", "Noting your response…");
  later(() => {
    addMessage("agent", `Got it — noted: "${ownerMessage.body}".`, ownerMessage.id);
    finishTurn();
  }, 1000);
}

function resolveAttachments(ids) {
  return (ids ?? [])
    .map((id) => {
      const b = blobs.get(id);
      return b ? { id: b.id, name: b.name, mime: b.mime, size: b.size } : null;
    })
    .filter(Boolean);
}

function handleUploadBlob(ws, frame) {
  const buffer = Buffer.from(frame.data_b64 ?? "", "base64");
  if (buffer.length > MAX_BLOB_BYTES) {
    ws.send(JSON.stringify({ type: "error", detail: "blob exceeds 15 MB", client_id: frame.client_id }));
    return;
  }
  const id = randomUUID();
  const blob = { id, name: frame.name, mime: frame.mime, size: buffer.length };
  blobs.set(id, { ...blob, buffer });
  ws.send(JSON.stringify({ type: "blob_ok", client_id: frame.client_id, blob }));
  log("stored blob", frame.name, `${buffer.length}B`);
}

function handleSendMessage(frame) {
  const already = seenClientIds.get(frame.client_id);
  if (already !== undefined) {
    const existing = messages.find((m) => m.id === already);
    if (existing) broadcast({ type: "msg", message: existing });
    return;
  }

  const attachments = resolveAttachments(frame.attachments);
  const message = addMessage("owner", frame.body, frame.ref, attachments);
  seenClientIds.set(frame.client_id, message.id);

  // Anchored replies resolve their inbox item regardless of mode.
  if (message.ref !== null) {
    const item = findOpenInboxByAnchor(message.ref);
    if (item) {
      acknowledgeInboxResponse(item, message);
      return;
    }
  }

  // mode=next_turn while a turn is running: hold for the current turn to finish.
  // (mode=send during a turn is treated as normal ingress — Early Injection is
  // indistinguishable from a plain reply in this mock.)
  if (frame.mode === "next_turn" && turnActive) {
    queuedNextTurn.push({ clientId: frame.client_id, messageId: message.id });
    log("queued next_turn message", message.id);
    return;
  }

  if (turnActive) {
    // Plain send mid-turn = Early Injection: it joins the running turn (already
    // echoed to chat above), the mock does not spawn a separate reply.
    log("early-injected into active turn", message.id);
    return;
  }
  startReplyTurn(message);
}

function handleCancelTurn() {
  if (!turnActive) return; // no-op if idle
  clearTurnTimers();
  addMessage("agent", "_Turn cancelled._", null);
  finishTurn();
}

function handleCancelQueued(ws, frame) {
  const idx = queuedNextTurn.findIndex((q) => q.clientId === frame.client_id);
  if (idx === -1) {
    // Not queued (never was, or already claimed/replied).
    ws.send(JSON.stringify({ type: "error", detail: "already claimed", client_id: frame.client_id }));
    return;
  }
  const [removed] = queuedNextTurn.splice(idx, 1);
  // Drop it from history — it never reached the Agent.
  const mi = messages.findIndex((m) => m.id === removed.messageId);
  if (mi !== -1) messages.splice(mi, 1);
  seenClientIds.delete(frame.client_id);
  broadcast({ type: "msg_removed", id: removed.messageId });
  log("cancelled queued message", removed.messageId);
}

function handleResolvePing(frame) {
  // ADR-0009: "Mark done" — the Ping's terminal `done` state.
  const item = inbox.find((i) => i.id === frame.ping_id);
  if (!item || item.status !== "open") return;
  upsertInbox({ ...item, status: "done" });
}

function handleReadPing(frame) {
  const item = inbox.find((i) => i.id === frame.ping_id);
  if (!item || item.read === true) return; // idempotent
  upsertInbox({ ...item, read: true });
}

// --- HTTP (blob content) + WS on the same port ------------------------------
const httpServer = createServer((req, res) => {
  const url = new URL(req.url ?? "/", `http://${req.headers.host}`);
  const match = url.pathname.match(/^\/blob\/(.+)$/);
  if (req.method === "GET" && match) {
    if (url.searchParams.get("token") !== TOKEN) {
      res.writeHead(403);
      res.end("forbidden");
      return;
    }
    const blob = blobs.get(decodeURIComponent(match[1]));
    if (!blob) {
      res.writeHead(404);
      res.end("not found");
      return;
    }
    res.writeHead(200, {
      "Content-Type": blob.mime || "application/octet-stream",
      "Content-Length": blob.buffer.length,
      "Access-Control-Allow-Origin": "*",
      "Cache-Control": "no-store",
    });
    res.end(blob.buffer);
    return;
  }
  res.writeHead(404);
  res.end("not found");
});

const wss = new WebSocketServer({ server: httpServer });

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
      const doneItems = inbox
        .filter((i) => i.status !== "open")
        .slice(-ARCHIVED_REPLAY_LIMIT);
      ws.send(
        JSON.stringify({
          type: "hello_ok",
          latest_msg_id: nextMsgId - 1,
          messages: replayMessages,
          pings: [...openItems, ...doneItems],
          processes: processesForHello(),
          side_chats: sideChatsForHello(),
        }),
      );
      log("hello ok, replayed", replayMessages.length, "messages");
      return;
    }

    switch (frame.type) {
      case "send_message":
        // v2.0: sc present routes to that side chat; absent is main (byte-
        // identical to the pre-v2.0 wire shape).
        if (frame.sc) handleSideSendMessage(frame);
        else handleSendMessage(frame);
        break;
      case "upload_blob":
        handleUploadBlob(ws, frame);
        break;
      case "cancel_turn":
        if (frame.sc) handleSideCancelTurn(frame);
        else handleCancelTurn();
        break;
      case "cancel_queued":
        handleCancelQueued(ws, frame);
        break;
      case "resolve_ping":
        handleResolvePing(frame);
        break;
      case "read_ping":
        handleReadPing(frame);
        break;
      case "open_side_chat":
        handleOpenSideChat(ws, frame);
        break;
      case "conclude_side_chat":
        handleConcludeSideChat(frame);
        break;
      case "confirm_conclusion":
        handleConfirmConclusion(frame);
        break;
      case "discard_side_chat":
        handleDiscardSideChat(frame);
        break;
      case "hello":
        ws.send(JSON.stringify({ type: "error", detail: "hello already sent" }));
        break;
      default:
        ws.send(JSON.stringify({ type: "error", detail: `unknown frame type: ${frame.type}` }));
    }
  });
});

/** Seed a couple of processes so the Processes tab is populated on first load.
 * Backdated timestamps exercise the "started X ago" / newest-first ordering. */
function seedProcesses() {
  const ago = (mins) => new Date(Date.now() - mins * 60_000).toISOString();
  processes.push({
    id: `proc-${nextProcSeq++}`,
    kind: "monitor",
    label: "tail -n0 -F /var/log/deploy.log",
    agent: null,
    model: null,
    state: "running",
    started_ts: ago(42),
    last_event_ts: ago(3),
    summary: "every 30s — fired: 'deploy complete: build 4821'",
  });
  processes.push({
    id: `proc-${nextProcSeq++}`,
    kind: "subagent",
    label: "Draft release notes for v1.4",
    agent: "writer",
    model: "fable-5",
    state: "done",
    started_ts: ago(20),
    last_event_ts: ago(11),
    summary: "done — release notes drafted (2 revisions)",
  });
}

if (process.env.MOCK_SEED !== "none") seedProcesses();

/** Seed a short chat + two open Inbox Items so the Tray, Side Chats, and the
 * Done section have content on first load (dev/demo only). */
function seedInbox() {
  const ago = (mins) => new Date(Date.now() - mins * 60_000).toISOString();
  const push = (author, body, ref = null, ts = now()) => {
    const m = { id: nextMsgId++, author, body, ref, ts, attachments: [], tool_calls: [] };
    messages.push(m);
    return m;
  };
  push("owner", "morning — anything need me?", null, ago(30));
  const a1 = push("agent", "Deploy of build 4821 is staged and green. Ship it to prod now?", messages.at(-1).id, ago(29));
  const a2 = push("agent", "The auth refactor branch is ready to merge — want me to open the PR?", a1.id, ago(20));
  inbox.push({
    id: nextInboxId++,
    name: "deploy-4821",
    description: "Ship the staged prod build?",
    content: "**Deploy build 4821 to prod?**\n\nTests are green and the staging smoke passed.",
    anchor: a1.id,
    requires_response: true,
    quick_replies: [
      { value: "ship it", label: "Ship it" },
      { value: "hold off", label: "Hold off" },
    ],
    status: "open",
    ts: ago(29),
  });
  inbox.push({
    id: nextInboxId++,
    name: "auth-pr",
    description: "Open the PR for the auth refactor branch",
    content: "Auth refactor branch is ready — I can open the PR whenever you like.",
    anchor: a2.id,
    requires_response: false,
    quick_replies: [],
    status: "open",
    read: true,
    ts: ago(20),
  });
  inbox.push({
    id: nextInboxId++,
    name: "nightly-backup",
    description: "Nightly backup verified — 0 errors",
    content: "Nightly backup completed and verified. Nothing needed from you.",
    anchor: a2.id,
    requires_response: false,
    quick_replies: [],
    status: "done",
    read: true,
    ts: ago(40),
  });
}

if (process.env.MOCK_SEED !== "none") seedInbox();

httpServer.listen(PORT, () => {
  log(`listening on ws://localhost:${PORT} + http blobs at /blob/:id (token: ${TOKEN})`);
});
