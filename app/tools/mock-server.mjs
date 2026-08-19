#!/usr/bin/env node
// Dev harness: a tiny in-memory host implementing the current task protocol,
// with a scripted "agent" so the PWA can be
// developed/demoed without the Rust host. Not durable — restart resets all
// state. Serves blob content over HTTP on the same port the WS runs on.
// Run via `npm run dev:mock` (mock + vite together) or `npm run mock-server`.
import { createServer } from "node:http";
import { randomUUID } from "node:crypto";
import { AsyncLocalStorage } from "node:async_hooks";
import { WebSocketServer } from "ws";

const PORT = Number(process.env.MOCK_PORT ?? 8787);
// Development should not make people hunt for a pretend secret. By default the
// mock accepts any non-empty token. Set MOCK_TOKEN only when a test or demo
// explicitly needs to exercise rejection with one exact value.
const TOKEN = process.env.MOCK_TOKEN;
const REPLAY_LIMIT = 200;
const ARCHIVED_REPLAY_LIMIT = 20;
const MAX_BLOB_BYTES = 15 * 1024 * 1024;
const MAX_ACTION_DATA_BYTES = 8 * 1024;
const MAX_ACTIONS = 16;
const MAX_FIELDS = 16;
const MAX_FIELD_STRING_BYTES = 1024;
const MAX_CHOICES = 16;
const MAX_ACTION_NAME_BYTES = 64;
const REPLY_DELAY_MS = Number(process.env.MOCK_REPLY_MS ?? 1200);

// Every accepted development token owns an isolated in-memory world. This
// keeps concurrent runbooks independent while preserving same-token reconnect
// semantics. AsyncLocalStorage carries that world through reply timers.
const tenantContext = new AsyncLocalStorage();
const tenants = new Map();

function currentTenant() {
  const tenant = tenantContext.getStore();
  if (!tenant) throw new Error("mock tenant context is unavailable");
  return tenant;
}

function scopedCollection(key, target) {
  return new Proxy(target, {
    get(_target, property) {
      const collection = currentTenant()[key];
      const value = Reflect.get(collection, property, collection);
      return typeof value === "function" ? value.bind(collection) : value;
    },
    set(_target, property, value) {
      return Reflect.set(currentTenant()[key], property, value);
    },
  });
}

/** @type {{id:number, author:'owner'|'agent', body:string, ref:number|null, ts:string, attachments:object[], tool_calls:object[], mentions:number[]}[]} */
const messages = scopedCollection("messages", []);
const events = scopedCollection("events", []);
// Declarative, per-Task generated-instrument state. The mock does not know
// about deploys: each flow supplies stages and action/data transitions.
const eventFlows = scopedCollection("eventFlows", new Map());
/** @type {{id:string, kind:'subagent'|'monitor', label:string, agent:string|null, model:string|null, state:string, started_ts:string, last_event_ts:string, summary:string|null}[]} */
const processes = scopedCollection("processes", []);
const seenClientIds = scopedCollection("seenClientIds", new Map());
const blobs = scopedCollection("blobs", new Map());
const clients = scopedCollection("clients", new Set());
const runtime = new Proxy({}, {
  get(_target, property) {
    return currentTenant()[property];
  },
  set(_target, property, value) {
    currentTenant()[property] = value;
    return true;
  },
});

function tenantForToken(token) {
  let tenant = tenants.get(token);
  if (tenant) return tenant;
  tenant = {
    token,
    messages: [],
    events: [],
    eventFlows: new Map(),
    processes: [],
    nextMsgId: 1,
    nextEventId: 1,
    nextProcSeq: 1,
    seenClientIds: new Map(),
    blobs: new Map(),
    turnActive: false,
    turnTimers: [],
    queuedNextTurn: [],
    clients: new Set(),
  };
  tenants.set(token, tenant);
  tenantContext.run(tenant, () => {
    if (process.env.MOCK_SEED !== "none") {
      seedProcesses();
      seedEvents();
    }
  });
  return tenant;
}

function now() {
  return new Date().toISOString();
}
function log(...args) {
  console.log(`[mock-server]`, ...args);
}
function acceptsToken(token) {
  return typeof token === "string" && token.length > 0 && (!TOKEN || token === TOKEN);
}
function broadcast(frame) {
  const json = JSON.stringify(frame);
  for (const ws of clients) if (ws.readyState === ws.OPEN) ws.send(json);
}

function addMessage(author, body, ref, attachments = [], toolCalls = [], mentions = []) {
  const message = {
    id: runtime.nextMsgId++,
    author,
    body,
    ref: ref ?? null,
    ts: now(),
    attachments,
    tool_calls: toolCalls,
    mentions,
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
function upsertEvent(item) {
  const idx = events.findIndex((event) => event.id === item.id);
  if (idx === -1) events.push(item);
  else events[idx] = item;
  broadcast({ type: "event_upsert", event: item });
}

function later(fn, ms) {
  const t = setTimeout(fn, ms);
  runtime.turnTimers.push(t);
  return t;
}
function clearTurnTimers() {
  for (const t of runtime.turnTimers) clearTimeout(t);
  runtime.turnTimers = [];
}

/** Finish the active turn, then drain the next_turn queue (one turn each). */
function finishTurn() {
  setActivity("idle", null);
  runtime.turnActive = false;
  runtime.turnTimers = [];
  drainQueue();
}

function drainQueue() {
  if (runtime.turnActive || runtime.queuedNextTurn.length === 0) return;
  const next = runtime.queuedNextTurn.shift(); // claim it
  log("claimed queued message", next.messageId);
  const message = messages.find((m) => m.id === next.messageId);
  if (message) startReplyTurn(message);
}

/** Kick off a scripted agent reply for an owner message. */
function startReplyTurn(ownerMessage) {
  runtime.turnActive = true;
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
    const procId = `proc-${runtime.nextProcSeq++}`;
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
        // Independent follow-up: a typed task lands when it finishes.
        upsertEvent({
          id: runtime.nextEventId++,
          kind: "judgment",
          source: { kind: "subagent", ref: procId },
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
          ui: [
            { type: "heading", text: "Review the delegated diff" },
            { type: "text", tone: "muted", text: "The sub-agent finished and the diff is ready." },
            {
              type: "optionList",
              action: "choose",
              options: [
                { key: "approve", label: "Approve", recommended: true },
                { key: "reject", label: "Reject" },
              ],
            },
          ],
          status: "open",
          read: false,
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
    const procId = `proc-${runtime.nextProcSeq++}`;
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
        "Monitor is live — I'll alert you here if the health check starts failing.",
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
  const message = addMessage(
    "owner",
    frame.body,
    frame.ref,
    attachments,
    [],
    frame.mentions ?? [],
  );
  seenClientIds.set(frame.client_id, message.id);

  // mode=next_turn while a turn is running: hold for the current turn to finish.
  // (mode=send during a turn is treated as normal ingress — Early Injection is
  // indistinguishable from a plain reply in this mock.)
  if (frame.mode === "next_turn" && runtime.turnActive) {
    runtime.queuedNextTurn.push({ clientId: frame.client_id, messageId: message.id });
    log("queued next_turn message", message.id);
    return;
  }

  if (runtime.turnActive) {
    // Plain send mid-turn = Early Injection: it joins the running turn (already
    // echoed to the conversation above), the mock does not spawn a separate reply.
    log("early-injected into active turn", message.id);
    return;
  }
  startReplyTurn(message);
}

function handleCancelTurn() {
  if (!runtime.turnActive) return; // no-op if idle
  clearTurnTimers();
  addMessage("agent", "_Turn cancelled._", null);
  finishTurn();
}

function handleCancelQueued(ws, frame) {
  const idx = runtime.queuedNextTurn.findIndex((q) => q.clientId === frame.client_id);
  if (idx === -1) {
    // Not queued (never was, or already claimed/replied).
    ws.send(JSON.stringify({ type: "error", detail: "already claimed", client_id: frame.client_id }));
    return;
  }
  const [removed] = runtime.queuedNextTurn.splice(idx, 1);
  // Drop it from history — it never reached the Agent.
  const mi = messages.findIndex((m) => m.id === removed.messageId);
  if (mi !== -1) messages.splice(mi, 1);
  seenClientIds.delete(frame.client_id);
  broadcast({ type: "msg_removed", id: removed.messageId });
  log("cancelled queued message", removed.messageId);
}

function handleResolvePing(frame) {
  // Legacy client input remains accepted, but the visible mock contract is
  // event-native and therefore always publishes an event_upsert.
  const item = events.find((event) => event.id === frame.ping_id);
  if (!item || item.status !== "open") return;
  upsertEvent({ ...item, status: "done" });
}

function handleReadPing(frame) {
  const item = events.find((event) => event.id === frame.ping_id);
  if (!item || item.read === true) return; // idempotent
  upsertEvent({ ...item, read: true });
}

function applyEventFlowStage(item, flow, stageIndex) {
  const stage = flow.stages[stageIndex];
  if (!stage) return false;
  flow.current = stageIndex;
  upsertEvent({
    ...item,
    ...(stage.description ? { description: stage.description } : {}),
    ui: stage.ui,
    status: stage.status ?? "open",
  });
  return true;
}

function transitionFor(stage, frame) {
  return stage.transitions?.find((transition) =>
    transition.action === frame.action
      && (transition.choice === undefined || transition.choice === frame.data?.choice));
}

function actionDataObject(action, data) {
  if (!data || typeof data !== "object" || Array.isArray(data)) {
    throw new Error(`Task action \`${action}\` data must be an object`);
  }
  if (Buffer.byteLength(JSON.stringify(data)) > MAX_ACTION_DATA_BYTES) {
    throw new Error(`Task action data exceeds ${MAX_ACTION_DATA_BYTES} bytes`);
  }
  return data;
}

function validateGeneratedAction(ui, action, data) {
  const object = actionDataObject(action, data);
  const actions = new Map();
  const fields = new Map();
  const stack = Array.isArray(ui) ? [...ui] : [ui];
  while (stack.length > 0) {
    const node = stack.pop();
    if (!node || typeof node !== "object" || Array.isArray(node)) continue;
    if (node.type === "optionList" || node.type === "submit") {
      const declared = typeof node.action === "string"
        ? node.action
        : node.type === "optionList" ? "choose" : "submit";
      if (Buffer.byteLength(declared) > MAX_ACTION_NAME_BYTES) {
        throw new Error(`Task action name exceeds ${MAX_ACTION_NAME_BYTES} bytes`);
      }
      if (actions.size >= MAX_ACTIONS) throw new Error(`Task UI exceeds ${MAX_ACTIONS} actions`);
      if (actions.has(declared)) throw new Error(`duplicate Task action declaration \`${declared}\``);
      const options = new Map();
      if (node.type === "optionList") {
        const declaredOptions = Array.isArray(node.options) ? node.options : [];
        if (declaredOptions.length > MAX_CHOICES) {
          throw new Error(`Task option list exceeds ${MAX_CHOICES} choices`);
        }
        for (const option of declaredOptions) {
          if (options.has(option.key)) throw new Error(`duplicate Task option key \`${option.key}\``);
          options.set(option.key, option.label);
        }
      }
      actions.set(declared, {
        settles: node.settles !== false,
        kind: node.type === "optionList" ? "options" : "form",
        options,
      });
    } else if (node.type === "field") {
      if (fields.size >= MAX_FIELDS) throw new Error(`Task UI exceeds ${MAX_FIELDS} fields`);
      if (node.kind !== undefined && node.kind !== "text") {
        throw new Error(`Task field kind must be \`text\``);
      }
      if (fields.has(node.name)) throw new Error(`duplicate Task field name \`${node.name}\``);
      fields.set(node.name, { required: node.required === true });
    }
    if (Array.isArray(node.children)) stack.push(...node.children);
  }
  const spec = actions.get(action);
  if (!spec) throw new Error(`action \`${action}\` is not declared by the current Task UI`);
  if (spec.kind === "options") {
    const keys = Object.keys(object);
    const unknown = keys.find((key) => key !== "choice" && key !== "label");
    if (unknown) throw new Error(`unknown Task action data field \`${unknown}\``);
    if (typeof object.choice !== "string") throw new Error("Task action requires data.choice");
    if (Buffer.byteLength(object.choice) > MAX_FIELD_STRING_BYTES) {
      throw new Error(`Task action data.choice exceeds ${MAX_FIELD_STRING_BYTES} bytes`);
    }
    const label = spec.options.get(object.choice);
    if (typeof label !== "string") {
      throw new Error(`unknown Task action choice: ${object.choice}`);
    }
    if (object.label !== undefined) {
      if (typeof object.label !== "string") throw new Error("Task action data.label must be a string");
      if (Buffer.byteLength(object.label) > MAX_FIELD_STRING_BYTES) {
        throw new Error(`Task action data.label exceeds ${MAX_FIELD_STRING_BYTES} bytes`);
      }
      if (object.label !== label) {
        throw new Error(`Task action data.label does not match choice \`${object.choice}\``);
      }
    }
    return { settles: spec.settles, choiceLabel: label };
  }
  const keys = Object.keys(object);
  if (keys.length > MAX_FIELDS) throw new Error(`Task action data exceeds ${MAX_FIELDS} fields`);
  const unknown = keys.find((key) => !fields.has(key));
  if (unknown) throw new Error(`unknown Task action data field \`${unknown}\``);
  for (const [name, field] of fields) {
    const value = object[name];
    if (value === undefined) {
      if (field.required) throw new Error(`Task action requires data.${name}`);
      continue;
    }
    if (typeof value !== "string") throw new Error(`Task action data.${name} must be a string`);
    if (Buffer.byteLength(value) > MAX_FIELD_STRING_BYTES) {
      throw new Error(`Task action data.${name} exceeds ${MAX_FIELD_STRING_BYTES} bytes`);
    }
    if (field.required && value.length === 0) {
      throw new Error(`Task action requires non-empty data.${name}`);
    }
  }
  return { settles: spec.settles, choiceLabel: null };
}

function validateEmptyLifecycleData(action, data) {
  if (data === undefined || data === null) return;
  const object = actionDataObject(action, data);
  if (Object.keys(object).length !== 0) throw new Error(`event ${action} data must be empty`);
}

function applyEventAction(frame) {
  const item = events.find((event) => event.id === frame.event_id);
  if (!item) throw new Error(`unknown event: ${frame.event_id}`);
  const flow = eventFlows.get(item.id);
  if (frame.action === "reopen") {
    validateEmptyLifecycleData(frame.action, frame.data);
    if (flow) {
      applyEventFlowStage(item, flow, flow.reopenStage ?? Math.max(0, flow.current - 1));
    } else {
      upsertEvent({ ...item, status: "open" });
    }
    log("event_action", frame.event_id, frame.action, JSON.stringify(frame.data));
    return;
  }
  if (["dismiss", "archive", "unarchive", "unsnooze"].includes(frame.action)) {
    validateEmptyLifecycleData(frame.action, frame.data);
    const next = frame.action === "dismiss"
      ? { status: "done" }
      : frame.action === "archive"
        ? { archived: true }
        : frame.action === "unarchive"
          ? { archived: false }
          : { snoozed_until: null };
    upsertEvent({ ...item, ...next });
    log("event_action", frame.event_id, frame.action, JSON.stringify(frame.data));
    return;
  }
  if (frame.action === "snooze") {
    const object = actionDataObject(frame.action, frame.data);
    if (Object.keys(object).length !== 1 || typeof object.until !== "string"
      || Buffer.byteLength(object.until) > MAX_FIELD_STRING_BYTES
      || !Number.isFinite(Date.parse(object.until)) || Date.parse(object.until) <= Date.now()) {
      throw new Error("snooze requires data.until as a future RFC3339 timestamp");
    }
    upsertEvent({ ...item, snoozed_until: object.until });
    log("event_action", frame.event_id, frame.action, JSON.stringify(frame.data));
    return;
  }
  if (item.status !== "open") throw new Error("only an open Task can accept generated actions");
  const validated = validateGeneratedAction(item.ui, frame.action, frame.data);
  if (flow) {
    const transition = transitionFor(flow.stages[flow.current], frame);
    if (transition && applyEventFlowStage(item, flow, transition.to)) {
      log("event_action", frame.event_id, frame.action, JSON.stringify(frame.data));
      return;
    }
  }
  if (validated.settles) upsertEvent({ ...item, status: "done" });
  log("event_action", frame.event_id, frame.action, JSON.stringify(frame.data));
}

function handleEventAction(ws, frame) {
  try {
    applyEventAction(frame);
  } catch (error) {
    const detail = error instanceof Error ? error.message : String(error);
    ws.send(JSON.stringify({ type: "error", detail }));
    log("event_action_rejected", frame.event_id, frame.action, detail);
  }
}

// --- Plugin tier (dev stand-in for the Host's plugin registry) --------------
// One in-memory plugin, matching the in-repo `plugins/hello` UI, so the whole
// browser half (the roster, the enable/settings writes, a plugin's own routes,
// and plugin_push) is exercisable without the Rust Host. The UI itself is
// compiled into the app by Vite and is never served from here. State is
// process-wide, not per-tenant: a dev fixture, not a durability model.
const pluginState = {
  hello: {
    id: "hello",
    label: "Hello Plugin",
    version: "0.1.0",
    enabled: true,
    error: null,
    settings: [
      { key: "greeting", label: "Greeting word", kind: "string", default: "Hello" },
      { key: "shout", label: "Shout it", kind: "boolean", default: false },
      { key: "token", label: "API token", kind: "secret" },
    ],
    values: { greeting: "Hello", shout: false, token: null },
  },
};

function pluginInfo(p) {
  return {
    id: p.id,
    label: p.label,
    version: p.version,
    state: p.error ? "errored" : p.enabled ? "running" : "disabled",
    error: p.error,
    settings: p.settings,
    // Secrets are reported as a sentinel, never in clear.
    values: Object.fromEntries(
      Object.entries(p.values).map(([k, v]) => {
        const spec = p.settings.find((s) => s.key === k);
        if (spec?.kind !== "secret") return [k, v];
        return [k, v ? "<set>" : null];
      }),
    ),
  };
}

/** The hello plugin's OWN route. The Rust side mounts a per-plugin router under
 * /api/plugins/<id>/…; this is the dev stand-in for hello's. */
let greetCount = 0;
function helloGreet(plugin, params) {
  const word = plugin.values.greeting || "Hello";
  const text = `${word}, ${params?.name ?? "world"}!`;
  greetCount += 1;
  return { text: plugin.values.shout ? text.toUpperCase() : text, count: greetCount };
}

function bearerToken(req) {
  const header = req.headers.authorization ?? "";
  return header.startsWith("Bearer ") ? header.slice("Bearer ".length) : null;
}

function sendJson(res, status, body) {
  const payload = JSON.stringify(body);
  res.writeHead(status, {
    "Content-Type": "application/json",
    "Content-Length": Buffer.byteLength(payload),
    "Cache-Control": "no-store",
  });
  res.end(payload);
}

async function readJsonBody(req) {
  const chunks = [];
  for await (const chunk of req) chunks.push(chunk);
  if (chunks.length === 0) return {};
  try {
    return JSON.parse(Buffer.concat(chunks).toString("utf8"));
  } catch {
    return {};
  }
}

/** Push a frame to every connected client of every tenant (dev convenience). */
function broadcastAll(frame) {
  const json = JSON.stringify(frame);
  for (const tenant of tenants.values()) {
    for (const ws of tenant.clients) if (ws.readyState === ws.OPEN) ws.send(json);
  }
}

let tickCount = 0;
setInterval(() => {
  if (!pluginState.hello.enabled) return;
  tickCount += 1;
  broadcastAll({ type: "plugin_push", plugin: "hello", topic: "tick", data: { count: tickCount } });
}, 5000).unref();

/** Returns true when the request was a plugin route and has been answered. */
async function handlePluginRoute(req, res, url) {
  if (!url.pathname.startsWith("/api/plugins")) return false;
  if (!acceptsToken(bearerToken(req))) {
    sendJson(res, 401, { error: "unauthorized" });
    return true;
  }

  if (req.method === "GET" && url.pathname === "/api/plugins") {
    sendJson(res, 200, { plugins: Object.values(pluginState).map(pluginInfo) });
    return true;
  }

  // `greet` is hello's own route; `enabled`/`settings` are the Host's roster
  // administration for any plugin.
  const action = url.pathname.match(/^\/api\/plugins\/([^/]+)\/(greet|enabled|settings)$/);
  const plugin = action ? pluginState[decodeURIComponent(action[1])] : null;
  if (req.method === "POST" && action && plugin) {
    const body = await readJsonBody(req);
    if (action[2] === "greet") {
      sendJson(res, 200, helloGreet(plugin, body));
    } else if (action[2] === "enabled") {
      plugin.enabled = Boolean(body.enabled);
      log("plugin", plugin.id, plugin.enabled ? "enabled" : "disabled");
      sendJson(res, 200, { ok: true });
    } else {
      for (const [key, value] of Object.entries(body.values ?? {})) {
        if (key in plugin.values) plugin.values[key] = value;
      }
      log("plugin", plugin.id, "settings saved");
      sendJson(res, 200, { ok: true });
    }
    return true;
  }

  sendJson(res, 404, { error: "not found" });
  return true;
}

// --- HTTP (blob content, plugin tier) + WS on the same port ------------------
const httpServer = createServer((req, res) => {
  const url = new URL(req.url ?? "/", `http://${req.headers.host}`);
  void handlePluginRoute(req, res, url).then((handled) => {
    if (!handled) handleBlobRoute(req, res, url);
  });
});

function handleBlobRoute(req, res, url) {
  const match = url.pathname.match(/^\/blob\/(.+)$/);
  if (req.method === "GET" && match) {
    const token = url.searchParams.get("token");
    if (!acceptsToken(token)) {
      res.writeHead(403);
      res.end("forbidden");
      return;
    }
    const blob = tenants.get(token)?.blobs.get(decodeURIComponent(match[1]));
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
}

const wss = new WebSocketServer({ server: httpServer });

wss.on("connection", (ws) => {
  let helloed = false;
  let tenant = null;
  log("connection opened");

  ws.on("close", () => {
    tenant?.clients.delete(ws);
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
      if (!acceptsToken(frame.token)) {
        ws.send(JSON.stringify({ type: "error", detail: "invalid token" }));
        ws.close(1008, "invalid token");
        return;
      }
      helloed = true;
      tenant = tenantForToken(frame.token);
      tenant.clients.add(ws);
      tenantContext.run(tenant, () => {
      const lastSeen = frame.last_seen_msg_id;
      const replayMessages =
        lastSeen === null || lastSeen === undefined
          ? messages.slice(-REPLAY_LIMIT)
          : messages.filter((m) => m.id > lastSeen);
      const openEvents = events.filter((event) => event.status === "open");
      const doneEvents = events
        .filter((event) => event.status !== "open")
        .slice(-ARCHIVED_REPLAY_LIMIT);
      ws.send(
        JSON.stringify({
          type: "hello_ok",
          latest_msg_id: runtime.nextMsgId - 1,
          messages: replayMessages,
          pings: [], // Legacy wire field; Tasks are carried by `events`.
          events: [...openEvents, ...doneEvents],
          processes: processesForHello(),
          model: {
            current: { id: "gpt-5.6-sol", variant: "medium" },
            available: [
              {
                id: "gpt-5.6-sol",
                label: "GPT-5.6 Sol",
                variants: ["low", "medium", "high", "xhigh", "max"],
                default_variant: "medium",
              },
            ],
          },
        }),
      );
      log("hello ok, replayed", replayMessages.length, "messages");
      });
      return;
    }

    tenantContext.run(tenant, () => {
    switch (frame.type) {
      case "send_message":
        handleSendMessage(frame);
        break;
      case "upload_blob":
        handleUploadBlob(ws, frame);
        break;
      case "cancel_turn":
        handleCancelTurn();
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
      case "event_action":
        handleEventAction(ws, frame);
        break;
      case "hello":
        ws.send(JSON.stringify({ type: "error", detail: "hello already sent" }));
        break;
      default:
        ws.send(JSON.stringify({ type: "error", detail: `unknown frame type: ${frame.type}` }));
    }
    });
  });
});

/** Seed a couple of processes so the Processes tab is populated on first load.
 * Backdated timestamps exercise the "started X ago" / newest-first ordering. */
function seedProcesses() {
  const ago = (mins) => new Date(Date.now() - mins * 60_000).toISOString();
  processes.push({
    id: `proc-${runtime.nextProcSeq++}`,
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
    id: `proc-${runtime.nextProcSeq++}`,
    kind: "subagent",
    label: "Draft release notes for v1.4",
    agent: "writer",
    model: "fable-5",
    state: "done",
    started_ts: ago(20),
    last_event_ts: ago(11),
    summary: "done — release notes drafted (2 revisions)",
  });
  processes.push({
    id: `proc-${runtime.nextProcSeq++}`,
    kind: "subagent",
    label: "Check the release candidate against the deployment runbook",
    agent: "release-reviewer",
    model: "gpt-5.5",
    state: "running",
    started_ts: ago(8),
    last_event_ts: ago(1),
    summary: "verifying rollback and health-check steps…",
  });
}

/** Seed a short transcript and three typed Tasks so the default task world has
 * meaningful open and settled states on first load (dev/demo only). */
function seedEvents() {
  const ago = (mins) => new Date(Date.now() - mins * 60_000).toISOString();
  const push = (author, body, ref = null, ts = now()) => {
    const m = { id: runtime.nextMsgId++, author, body, ref, ts, attachments: [], tool_calls: [], mentions: [] };
    messages.push(m);
    return m;
  };
  push("owner", "morning — anything need me?", null, ago(30));
  const a1 = push("agent", "Deploy of build 4821 is staged and green. Ship it to prod now?", messages.at(-1).id, ago(29));
  const a2 = push("agent", "The auth refactor branch is ready to merge — want me to open the PR?", a1.id, ago(20));
  const deploy = {
    id: runtime.nextEventId++,
    kind: "judgment",
    source: { kind: "monitor", ref: "deploy-watch" },
    name: "deploy-4821",
    description: "Ship the staged prod build?",
    content: "**Deploy build 4821 to prod?**\n\nTests are green and the staging smoke passed.",
    anchor: a1.id,
    requires_response: true,
    quick_replies: [
      { value: "ship it", label: "Ship it" },
      { value: "hold off", label: "Hold off" },
    ],
    blocking: true,
    ui: [
      { type: "eyebrow", tone: "accent", text: "Production boundary" },
      { type: "heading", text: "Ship build 4821 to production?" },
      { type: "text", tone: "muted", text: "Staging smoke and required checks are green." },
      {
        type: "optionList",
        action: "advance",
        settles: false,
        options: [
          { key: "A", label: "Ship now", detail: "Promote the staged artifact", recommended: true },
          { key: "B", label: "Hold", detail: "Leave production unchanged" },
        ],
      },
    ],
    status: "open",
    read: false,
    ts: ago(29),
  };
  events.push(deploy);
  eventFlows.set(deploy.id, {
    current: 0,
    reopenStage: 1,
    stages: [
      {
        status: "open",
        description: "Ship the staged prod build?",
        ui: deploy.ui,
        transitions: [{ action: "advance", choice: "A", to: 1 }],
      },
      {
        status: "open",
        description: "Canary is healthy — promote production?",
        ui: [
          { type: "eyebrow", tone: "accent", text: "Canary checkpoint" },
          { type: "heading", text: "Canary is healthy. Promote production?" },
          { type: "status", state: "success", label: "5% canary · 0 errors · p95 184ms" },
          { type: "text", tone: "muted", text: "The staged artifact has passed the live canary window." },
          {
            type: "optionList",
            action: "choose",
            options: [
              { key: "A", label: "Promote to 100%", detail: "Complete the production rollout", recommended: true },
              { key: "B", label: "Roll back canary", detail: "Return production to the previous build" },
            ],
          },
        ],
        transitions: [
          { action: "choose", choice: "A", to: 2 },
          { action: "choose", choice: "B", to: 3 },
        ],
      },
      {
        status: "done",
        description: "Build 4821 is live and healthy",
        ui: [
          { type: "eyebrow", tone: "accent", text: "Production complete" },
          { type: "heading", text: "Build 4821 is live" },
          { type: "status", state: "success", label: "100% · healthy" },
        ],
      },
      {
        status: "done",
        description: "Canary rolled back; production unchanged",
        ui: [
          { type: "eyebrow", text: "Production unchanged" },
          { type: "heading", text: "Canary rolled back" },
          { type: "status", state: "neutral", label: "Previous build remains live" },
        ],
      },
    ],
  });
  events.push({
    id: runtime.nextEventId++,
    kind: "judgment",
    source: { kind: "agent", ref: "hirsel" },
    name: "auth-pr",
    description: "Open the PR for the auth refactor branch",
    content: "Auth refactor branch is ready — I can open the PR whenever you like.",
    anchor: a2.id,
    requires_response: true,
    quick_replies: [],
    ui: [
      { type: "heading", text: "Open the auth refactor PR?" },
      { type: "text", tone: "muted", text: "The branch is ready for review." },
      { type: "field", name: "reviewer", label: "Reviewer", placeholder: "required", required: true },
      { type: "submit", action: "submit", label: "Open PR" },
    ],
    status: "open",
    read: true,
    ts: ago(20),
  });
  events.push({
    id: runtime.nextEventId++,
    kind: "summary",
    source: { kind: "scheduled", ref: "nightly-backup" },
    name: "nightly-backup",
    description: "Nightly backup verified — 0 errors",
    content: "Nightly backup completed and verified. Nothing needed from you.",
    anchor: a2.id,
    requires_response: false,
    quick_replies: [],
    ui: [
      { type: "heading", text: "Nightly backup verified" },
      { type: "status", state: "success", label: "0 errors" },
    ],
    status: "done",
    read: true,
    ts: ago(40),
  });
}

httpServer.listen(PORT, () => {
  const auth = TOKEN ? `token: ${TOKEN}` : "any non-empty token";
  log(`listening on ws://localhost:${PORT} + http blobs at /blob/:id (${auth})`);
});
