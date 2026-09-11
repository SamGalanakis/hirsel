import assert from "node:assert/strict";
import { execFileSync, spawn } from "node:child_process";
import { createWriteStream } from "node:fs";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { createServer } from "node:net";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { chromium } from "../app/node_modules/playwright/index.mjs";
import { WebSocket } from "../app/node_modules/ws/wrapper.mjs";

const repo = fileURLToPath(new URL("..", import.meta.url));
const requested = process.argv[2] ?? "all";
const scenarios = requested === "all"
  ? ["chat-chronology", "tool-execution", "artifact-creation", "artifact-presentation"]
  : [requested];
const knownScenarios = new Set(["chat-chronology", "tool-execution", "artifact-creation", "artifact-presentation", "native-lash-worker"]);
for (const scenario of scenarios) assert(knownScenarios.has(scenario), `Unknown product runbook: ${scenario}`);
if (scenarios.includes("native-lash-worker")) {
  assert(process.env.OPENROUTER_API_KEY?.trim(), "native-lash-worker requires OPENROUTER_API_KEY; no model call was started");
}

const runId = `${new Date().toISOString().replaceAll(/[:.]/g, "-")}-${process.pid}`;
const evidenceRoot = process.env.HIRSEL_RUNBOOK_ARTIFACTS
  ?? join("/tmp", `hirsel-product-runbooks-${runId}`);
const privateEvidenceValues = [process.env.OPENROUTER_API_KEY].filter(value => value);
await mkdir(evidenceRoot, { recursive: true });
console.log(`Product runbook evidence: ${evidenceRoot}`);

function git(...args) {
  return execFileSync("git", args, { cwd: repo, encoding: "utf8" }).trim();
}

function cargoTargetDirectory() {
  const metadata = JSON.parse(execFileSync(
    "cargo",
    ["metadata", "--no-deps", "--format-version", "1"],
    { cwd: repo, encoding: "utf8" },
  ));
  return metadata.target_directory;
}

async function unusedPort() {
  const server = createServer();
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  const address = server.address();
  assert(address && typeof address !== "string");
  assert.notEqual(address.port, 3076);
  await new Promise((resolve, reject) => server.close(error => error ? reject(error) : resolve()));
  return address.port;
}

async function poll(label, predicate, timeoutMs = 180_000) {
  const deadline = Date.now() + timeoutMs;
  let lastError;
  while (Date.now() < deadline) {
    try {
      const value = await predicate();
      if (value) return value;
    } catch (error) {
      lastError = error;
    }
    await new Promise(resolve => setTimeout(resolve, 200));
  }
  throw new Error(`${label} timed out${lastError ? `: ${lastError.message}` : ""}`);
}

function parseFrame(payload) {
  try {
    return JSON.parse(payload.toString());
  } catch {
    return null;
  }
}

function sanitizeEvidence(value) {
  if (Array.isArray(value)) return value.map(sanitizeEvidence);
  if (typeof value === "string") {
    return privateEvidenceValues.reduce(
      (sanitized, secret) => sanitized.replaceAll(secret, "<redacted>"),
      value,
    );
  }
  if (!value || typeof value !== "object") return value;
  return Object.fromEntries(Object.entries(value).map(([key, nested]) => [
    key,
    /(^|_)(auth|authorization|token|api_key|secret|key_tail)$/i.test(key)
      ? "<redacted>"
      : sanitizeEvidence(nested),
  ]));
}

async function sanitizeEvidenceFile(path) {
  let contents;
  try {
    contents = await readFile(path, "utf8");
  } catch (error) {
    if (error?.code === "ENOENT") return;
    throw error;
  }
  const sanitized = sanitizeEvidence(contents);
  if (sanitized !== contents) await writeFile(path, sanitized);
}

function latestFrame(frames, predicate) {
  return frames.findLast(row => row.direction === "received" && predicate(row.frame));
}

function terminal(state) {
  return ["completed", "failed", "cancelled", "interrupted"].includes(state);
}

async function waitForTurn(frames, turnId, predicate, label) {
  return poll(label, () => latestFrame(
    frames,
    frame => frame.type === "thread_turn" && frame.turn.id === turnId && predicate(frame.turn),
  )?.frame.turn);
}

async function openThread(url, token, threadId) {
  return new Promise((resolve, reject) => {
    const socket = new WebSocket(`${url.replace(/^http/, "ws")}/ws`);
    const clientId = `runbook-open-${crypto.randomUUID()}`;
    const timer = setTimeout(() => {
      socket.close();
      reject(new Error(`open_thread ${threadId} timed out`));
    }, 10_000);
    socket.on("error", reject);
    socket.on("open", () => socket.send(JSON.stringify({ type: "hello", auth: { static_token: token } })));
    socket.on("message", raw => {
      const frame = parseFrame(raw);
      if (frame?.type === "hello_ok") {
        socket.send(JSON.stringify({ type: "open_thread", client_id: clientId, thread_id: threadId }));
      } else if (frame?.type === "thread_opened" && frame.client_id === clientId) {
        clearTimeout(timer);
        socket.close();
        resolve(frame.detail);
      } else if (frame?.type === "error") {
        clearTimeout(timer);
        socket.close();
        reject(new Error(frame.detail));
      }
    });
  });
}

function sqliteJson(database, sql) {
  const output = execFileSync("sqlite3", ["-readonly", "-json", database, sql], { encoding: "utf8" }).trim();
  return output ? JSON.parse(output) : [];
}

function storeSnapshot(dataDir, threadId) {
  const database = join(dataDir, "hirsel.sqlite");
  const timelineEvents = sqliteJson(database, `SELECT e.turn_id,e.seq,e.event FROM thread_turn_events e JOIN thread_turns t ON t.id=e.turn_id WHERE t.thread_id=${threadId} ORDER BY e.turn_id,e.seq`)
    .map(row => ({ turn_id: row.turn_id, seq: row.seq, event: JSON.parse(row.event) }));
  return {
    schemaVersion: sqliteJson(database, "SELECT user_version AS version FROM pragma_user_version")[0]?.version,
    messages: sqliteJson(database, `SELECT id,thread_id,author,body,ref,ts,tool_calls FROM chat_messages WHERE thread_id=${threadId} ORDER BY id`),
    turns: sqliteJson(database, `SELECT id,thread_id,owner_message_id,agent_message_id,state,started_at,finished_at FROM thread_turns WHERE thread_id=${threadId} ORDER BY id`),
    timelineEvents,
    activities: sqliteJson(database, `SELECT id,thread_id,turn_id,kind,data,ts FROM thread_activities WHERE thread_id=${threadId} ORDER BY id`),
    artifacts: sqliteJson(database, `SELECT a.id,a.title,json_extract(a.kind,'$') AS kind,a.mime,a.filename,a.content,a.created_at,a.updated_at FROM artifacts a WHERE EXISTS (SELECT 1 FROM message_artifacts ma JOIN chat_messages m ON m.id=ma.message_id WHERE ma.artifact_id=a.id AND m.thread_id=${threadId}) ORDER BY a.id`),
    messageArtifacts: sqliteJson(database, `SELECT ma.message_id,ma.artifact_id FROM message_artifacts ma JOIN chat_messages m ON m.id=ma.message_id WHERE m.thread_id=${threadId} ORDER BY ma.message_id,ma.artifact_id`),
  };
}

function nativeWorkerStoreSnapshot(dataDir, parentThreadId, childThreadId) {
  const database = join(dataDir, "hirsel.sqlite");
  return {
    threads: sqliteJson(database, `SELECT id,kind,parent_thread_id,title,settled_at,archived_at,revision FROM threads WHERE id IN (${parentThreadId},${childThreadId}) ORDER BY id`),
    turns: sqliteJson(database, `SELECT t.id,t.thread_id,t.requester_thread_id,t.requester_turn_id,t.owner_message_id,t.agent_message_id,t.state,t.started_at,t.finished_at,e.config AS accepted_execution FROM thread_turns t LEFT JOIN thread_turn_execution e ON e.turn_id=t.id WHERE t.thread_id IN (${parentThreadId},${childThreadId}) ORDER BY t.id`),
    activities: sqliteJson(database, `SELECT id,thread_id,turn_id,kind,data,ts FROM thread_activities WHERE thread_id IN (${parentThreadId},${childThreadId}) ORDER BY id`),
    delegations: sqliteJson(database, `SELECT requester_turn_id,operation_id,payload,child_thread_id,child_turn_id FROM thread_delegations WHERE child_thread_id=${childThreadId} ORDER BY requester_turn_id,operation_id`),
    reports: sqliteJson(database, `SELECT child_turn_id,operation_id,report_seq,payload,activity_id FROM thread_reports WHERE child_turn_id IN (SELECT id FROM thread_turns WHERE thread_id=${childThreadId}) ORDER BY child_turn_id,report_seq`),
    pendingReportOutbox: sqliteJson(database, `SELECT id,client_id,thread_id,report_triggered FROM thread_requests WHERE thread_id=${parentThreadId} AND report_triggered=1 ORDER BY id`),
    executionPreference: sqliteJson(database, `SELECT thread_id,config FROM thread_execution_preferences WHERE thread_id=${childThreadId}`),
    nativeWorkerMeta: sqliteJson(database, `SELECT key,value FROM meta WHERE key LIKE 'thread:${childThreadId}:native_worker_%' ORDER BY key`),
  };
}

async function domSnapshot(page) {
  return page.locator('main[data-thread-id]').evaluate(main => {
    const visible = element => Boolean(element.offsetWidth || element.offsetHeight || element.getClientRects().length);
    const entries = [...main.querySelectorAll(":scope [data-message-id], :scope [data-execution-turn], :scope [data-activity-id]")]
      .filter(element => !element.parentElement?.closest("[data-message-id], [data-execution-turn], [data-activity-id]"))
      .map((element, index) => ({
        index,
        messageId: element.getAttribute("data-message-id"),
        turnId: element.getAttribute("data-execution-turn"),
        activityId: element.getAttribute("data-activity-id"),
        role: element.getAttribute("aria-label"),
        text: element.textContent?.trim() ?? "",
        visible: visible(element),
        workDetails: [...element.querySelectorAll('[data-slot="work-details"]')].map(details => ({
          open: details.open,
          visible: visible(details),
        })),
        timeline: [...element.querySelectorAll('[data-slot="timeline"] > li')].map(row => ({
          slot: row.getAttribute("data-slot"),
          toolCallId: row.getAttribute("data-tool-call-id"),
          text: row.textContent?.trim() ?? "",
          visible: visible(row),
          result: row.querySelector('[data-slot="tool-result"]')?.textContent?.trim() ?? null,
        })),
      }));
    return {
      entries,
      artifactIds: [...main.querySelectorAll("[data-artifact-ref]")].map(element => element.getAttribute("data-artifact-ref")),
      scroll: {
        clientWidth: document.documentElement.clientWidth,
        scrollWidth: document.documentElement.scrollWidth,
      },
    };
  });
}

async function scrollAndScreenshot(page, path) {
  await page.locator('[data-slot="thread-scroll"]').evaluate(element => { element.scrollTop = element.scrollHeight; });
  await page.screenshot({ path, fullPage: true });
}

async function capture(label, context) {
  const { page, scenarioDir, dataDir, url, token, threadId } = context;
  const [dom, detail] = await Promise.all([
    domSnapshot(page),
    openThread(url, token, threadId),
  ]);
  const store = storeSnapshot(dataDir, threadId);
  await Promise.all([
    writeFile(join(scenarioDir, `${label}-dom.json`), `${JSON.stringify(dom, null, 2)}\n`),
    writeFile(join(scenarioDir, `${label}-thread.json`), `${JSON.stringify(detail, null, 2)}\n`),
    writeFile(join(scenarioDir, `${label}-store.json`), `${JSON.stringify(store, null, 2)}\n`),
    scrollAndScreenshot(page, join(scenarioDir, `${label}.png`)),
  ]);
  return { dom, detail, store };
}

async function captureNativeWorker(label, context, threadId, parentThreadId, childThreadId) {
  const snapshot = await capture(label, { ...context, threadId });
  const nativeStore = nativeWorkerStoreSnapshot(context.dataDir, parentThreadId, childThreadId);
  await writeFile(join(context.scenarioDir, `${label}-native-store.json`), `${JSON.stringify(nativeStore, null, 2)}\n`);
  return { ...snapshot, nativeStore };
}

async function createThread(page, nonce) {
  const emptyState = page.locator('[data-slot="thread-empty"]');
  const emptyStateTitle = emptyState.getByLabel("First space or task title", { exact: true });
  let creationSurface = emptyState;
  let titleInput = emptyStateTitle;
  if (!(await emptyStateTitle.isVisible())) {
    await page.getByRole("button", { name: "Spaces and Tasks", exact: true }).click();
    creationSurface = page.locator('[data-slot="thread-drawer"]');
    await creationSurface.waitFor({ state: "visible" });
    titleInput = creationSurface.getByLabel("New space or task title", { exact: true });
  }
  await titleInput.waitFor({ state: "visible" });
  const title = `Runbook ${nonce}`;
  await titleInput.fill(title);
  await creationSurface.getByRole("button", { name: "New Space", exact: true }).click();
  await page.locator('[data-slot="thread-context"] h1').filter({ hasText: title }).waitFor();
  const match = new URL(page.url()).pathname.match(/^\/t\/(\d+)$/);
  assert(match, `Thread creation did not navigate: ${page.url()}`);
  return Number(match[1]);
}

async function sendOwnerMessage(page, frames, threadId, body) {
  const offset = frames.length;
  await page.locator(`main[data-thread-id="${threadId}"] textarea`).fill(body);
  await page.getByRole("button", { name: "Send", exact: true }).click();
  const owner = await poll("Owner message acknowledgement", () => latestFrame(
    frames.slice(offset),
    frame => frame.type === "msg" && frame.message.thread_id === threadId
      && frame.message.author === "owner" && frame.message.body === body,
  )?.frame.message);
  return { owner, offset };
}

async function waitForOwnerTurn(frames, threadId, ownerId) {
  return poll("turn acceptance", () => latestFrame(
    frames,
    frame => frame.type === "thread_turn" && frame.turn.thread_id === threadId
      && frame.turn.owner_message_id === ownerId,
  )?.frame.turn);
}

async function sendMessage(page, frames, threadId, body) {
  const { owner } = await sendOwnerMessage(page, frames, threadId, body);
  const turn = await poll("turn acceptance", () => latestFrame(
    frames,
    frame => frame.type === "thread_turn" && frame.turn.thread_id === threadId
      && frame.turn.owner_message_id === owner.id,
  )?.frame.turn);
  return { owner, turnId: turn.id };
}

function turnEvents(frames, turnId) {
  return frames
    .filter(row => row.direction === "received" && row.frame.type === "turn_event" && row.frame.turn_id === turnId)
    .map(row => row.frame);
}

function liveTimeline(frames, turnId) {
  return turnEvents(frames, turnId).map(({ seq, event }) => ({ seq, event }));
}

function durableTimeline(detail, turnId) {
  const timeline = detail.turn_timelines.find(candidate => candidate.turn_id === turnId);
  assert(timeline, `turn ${turnId} has no canonical durable timeline`);
  return timeline.events;
}

function storeTimeline(store, turnId) {
  return store.timelineEvents
    .filter(record => record.turn_id === turnId)
    .map(({ seq, event }) => ({ seq, event }));
}

function assertTimelineSurfaces(snapshot, frames, turnIds) {
  assert.equal(snapshot.store.schemaVersion, 6, "runbook store is not durable schema 6");
  for (const turnId of turnIds) {
    const live = liveTimeline(frames, turnId);
    assert(live.length > 0, `turn ${turnId} streamed no timeline events`);
    assert.deepEqual(live.map(record => record.seq), [...new Set(live.map(record => record.seq))].sort((a, b) => a - b), `turn ${turnId} live event sequence is duplicated or unordered`);
    assert.deepEqual(durableTimeline(snapshot.detail, turnId), live, `turn ${turnId} open_thread timeline differs from the live stream`);
    assert.deepEqual(storeTimeline(snapshot.store, turnId), live, `turn ${turnId} SQLite timeline differs from the live stream`);
  }
}

function agentReply(detail, turn) {
  const message = detail.messages.find(candidate => candidate.id === turn.agent_message_id);
  assert(message?.author === "agent", `turn ${turn.id} has no durable Agent reply`);
  return message;
}

function timelineProjection(dom) {
  return dom.entries.map(entry => ({
    messageId: entry.messageId,
    turnId: entry.turnId,
    activityId: entry.activityId,
    timeline: entry.timeline,
  }));
}

function renderedMarkdownText(text) {
  return text.trim().replace(/^(\*{1,3}|_{1,3})([\s\S]*)\1$/, "$2");
}

async function expandInlineTools(page, callIds) {
  for (const callId of callIds) {
    const row = page.locator(`[data-slot="timeline-tool"][data-tool-call-id="${callId}"]`).first();
    await row.waitFor({ state: "visible", timeout: 10_000 });
    const toggle = row.locator("button[aria-expanded]").first();
    if (await toggle.count() && await toggle.getAttribute("aria-expanded") === "false") await toggle.click();
  }
}

function assertTimelineRendered(dom, turn, events) {
  const entry = dom.entries.find(candidate => candidate.messageId === String(turn.agent_message_id));
  assert(entry, `turn ${turn.id} has no rendered completed entry`);
  const expectedToolIds = [];
  for (const { event } of events) {
    if (event.kind === "tool_start" && !expectedToolIds.includes(event.id)) expectedToolIds.push(event.id);
    if (event.kind === "tool_done" && !expectedToolIds.includes(event.id)) expectedToolIds.push(event.id);
    if ((event.kind === "reasoning" || event.kind === "prose") && event.text.trim()) {
      assert(entry.text.includes(renderedMarkdownText(event.text)), `turn ${turn.id} omits ${event.kind} content from the DOM`);
    }
    if (event.kind === "tool_start" && event.input?.text) {
      const row = entry.timeline.find(candidate => candidate.toolCallId === event.id);
      assert(row?.result?.includes(event.input.text), `tool ${event.id} input payload is absent from the expanded DOM row`);
    }
    if (event.kind === "tool_done" && event.result?.text) {
      const row = entry.timeline.find(candidate => candidate.toolCallId === event.id);
      assert(row?.result?.includes(event.result.text), `tool ${event.id} result payload is absent from the expanded DOM row`);
    }
  }
  assert.deepEqual(entry.timeline.filter(row => row.slot === "timeline-tool").map(row => row.toolCallId), expectedToolIds, `turn ${turn.id} rendered tool row order differs from its canonical events`);
}

function assertReasoningIntegrity(dom, turn, events) {
  const entry = dom.entries.find(candidate => candidate.messageId === String(turn.agent_message_id));
  assert(entry, `turn ${turn.id} has no rendered completed entry`);
  const reasoning = events
    .filter(({ event }) => event.kind === "reasoning" && event.text.trim())
    .map(({ event }) => {
      assert.equal(event.text.includes("****"), false, `turn ${turn.id} reasoning contains joined duplicate emphasis`);
      return renderedMarkdownText(event.text);
    });
  const rendered = entry.timeline
    .filter(row => row.slot === "timeline-reasoning")
    .map(row => row.text);
  assert.deepEqual(rendered, reasoning, `turn ${turn.id} does not render each reasoning phrase exactly once`);
}

function toolPair(frames, turnId, namePattern) {
  const events = turnEvents(frames, turnId);
  const started = events.find(frame => frame.event.kind === "tool_start" && namePattern.test(frame.event.name));
  assert(started, `turn ${turnId} has no matching tool_start`);
  const done = events.find(frame => frame.event.kind === "tool_done" && frame.event.id === started.event.id);
  assert(done, `tool ${started.event.id} has no matching tool_done`);
  return { started, done };
}

async function requireInlineTool(page, callId, expectedText) {
  const row = page.locator(`[data-slot="timeline-tool"][data-tool-call-id="${callId}"]`).first();
  await row.waitFor({ state: "visible", timeout: 10_000 });
  const hiddenByTurn = await row.evaluate(element => Boolean(element.closest('details[data-slot="work-details"]')));
  assert.equal(hiddenByTurn, false, `tool ${callId} is hidden inside whole-turn Work details`);
  const toggle = row.locator("button").first();
  if (await toggle.count() && !(await row.getByText(expectedText, { exact: false }).count())) await toggle.click();
  await row.getByText(expectedText, { exact: false }).waitFor({ timeout: 10_000 });
}

function payloadText(event, field) {
  const payload = event[field];
  assert(payload && typeof payload.text === "string" && typeof payload.truncated === "boolean", `${event.kind}.${field} is not a bounded payload`);
  assert.equal(payload.truncated, false, `${event.kind}.${field} was truncated`);
  return payload.text;
}

async function waitForStableDom(page) {
  let prior;
  let repeats = 0;
  return poll("stable conversation order", async () => {
    const snapshot = await domSnapshot(page);
    const identity = JSON.stringify(snapshot.entries.map(entry => [entry.messageId, entry.turnId, entry.activityId, entry.text]));
    if (identity === prior) repeats += 1;
    else { prior = identity; repeats = 0; }
    return repeats >= 3 ? snapshot : null;
  }, 15_000);
}

async function runChat(context) {
  const { page, frames, nonce, threadId } = context;
  const firstMarker = `HIRSEL-CHAT-FIRST-${nonce}`;
  const secondMarker = `HIRSEL-CHAT-SECOND-${nonce}`;
  const firstPrompt = `Use shell.run once with cmd "sleep 8; printf '${firstMarker}'". After the tool returns, end with the exact marker ${firstMarker}.`;
  const secondPrompt = `Use shell.run once with cmd "sleep 3; printf '${secondMarker}'". After the tool returns, end with the exact marker ${secondMarker}.`;
  const first = await sendMessage(page, frames, threadId, firstPrompt);
  await waitForTurn(frames, first.turnId, turn => turn.state === "running", "first turn running");
  await page.getByRole("button", { name: "Stop the agent", exact: true }).waitFor();
  const second = await sendOwnerMessage(page, frames, threadId, secondPrompt);
  await poll("second request queued", () => latestFrame(
    frames.slice(second.offset),
    frame => frame.type === "thread_upsert" && frame.thread.id === threadId
      && frame.thread.queued_turn_count >= 1,
  ));
  const queuedTurn = await waitForOwnerTurn(frames, threadId, second.owner.id);
  assert.equal(queuedTurn.state, "queued", "accepted second turn was not published as queued");
  assert.equal(latestFrame(frames, frame => frame.type === "thread_turn" && frame.turn.id === first.turnId && terminal(frame.turn.state)), undefined, "first turn finished before queued acknowledgement");
  const assertQueued = snapshot => {
    const entry = snapshot.dom.entries.find(candidate => candidate.turnId === String(queuedTurn.id));
    assert(entry?.visible, "accepted queued turn has no visible row");
    assert.match(entry.text, /Queued/, "accepted queued turn is not labelled Queued");
    const detailTurn = snapshot.detail.turns.find(turn => turn.id === queuedTurn.id);
    assert.equal(detailTurn?.owner_message_id, second.owner.id);
    assert.equal(detailTurn?.state, "queued");
    const storedTurn = snapshot.store.turns.find(turn => turn.id === queuedTurn.id);
    assert.equal(storedTurn?.owner_message_id, second.owner.id);
    assert.equal(storedTurn?.state, "queued");
  };
  assertQueued(await capture("10-queued", context));
  await page.reload({ waitUntil: "domcontentloaded" });
  await page.locator(`[data-execution-turn="${queuedTurn.id}"]`).getByText("Queued", { exact: true }).waitFor();
  assertQueued(await capture("11-queued-reloaded", context));

  const firstTerminal = await waitForTurn(frames, first.turnId, turn => terminal(turn.state), "first turn terminal");
  assert.equal(firstTerminal.state, "completed");
  const secondTurn = await waitForOwnerTurn(frames, threadId, second.owner.id);
  await waitForTurn(frames, secondTurn.id, turn => turn.state === "running", "second turn running");
  await page.getByText(firstMarker, { exact: false }).last().waitFor();
  const handoff = await capture("20-handoff", context);
  assert.match(agentReply(handoff.detail, firstTerminal).body, new RegExp(firstMarker));
  const firstReplyIndex = handoff.dom.entries.findIndex(entry => entry.messageId === String(firstTerminal.agent_message_id));
  const secondWorkIndex = handoff.dom.entries.findIndex(entry => entry.turnId === String(secondTurn.id));
  assert(firstReplyIndex >= 0, "first Agent reply is not rendered at handoff");
  assert(secondWorkIndex >= 0, "second running turn has no stable DOM identity");
  assert(firstReplyIndex < secondWorkIndex, "newer working row renders above the older reply");
  const secondWork = handoff.dom.entries[secondWorkIndex];
  assert.equal(secondWork.workDetails.length, 0, "running work is collapsed behind whole-turn Work details");

  const secondTerminal = await waitForTurn(frames, secondTurn.id, turn => terminal(turn.state), "second turn terminal");
  assert.equal(secondTerminal.state, "completed");
  await page.getByText(secondMarker, { exact: false }).last().waitFor();
  await waitForStableDom(page);
  const settled = await capture("30-settled", context);
  assert.equal(settled.detail.messages.filter(message => message.author === "owner").length, 2);
  assert.equal(settled.detail.messages.filter(message => message.author === "agent").length, 2);
  assert.equal(settled.detail.turns.filter(turn => terminal(turn.state)).length, 2);
  assert.equal(settled.store.messages.length, 4);
  assert.equal(settled.store.turns.length, 2);
  const firstTool = toolPair(frames, first.turnId, /shell[._]run/);
  const secondTool = toolPair(frames, secondTurn.id, /shell[._]run/);
  const collapsedTools = [firstTool, secondTool].map(tool => {
    const row = settled.dom.entries.flatMap(entry => entry.timeline).find(candidate => candidate.toolCallId === tool.started.event.id);
    assert(row?.visible, `tool ${tool.started.event.id} has no visible collapsed row`);
    assert(row.text.includes(tool.started.event.summary), `tool ${tool.started.event.id} lost its command/subject summary`);
    assert.match(row.text, /Succeeded/, `tool ${tool.started.event.id} has no plain success outcome`);
    assert.doesNotMatch(row.text, /ok status 0/, `tool ${tool.started.event.id} exposes transport status instead of a plain outcome`);
    return row.text;
  });
  assert.notEqual(collapsedTools[0], collapsedTools[1], "completed tool summaries are not meaningfully distinct");
  for (const [turn, marker, tool] of [
    [firstTerminal, firstMarker, firstTool],
    [secondTerminal, secondMarker, secondTool],
  ]) {
    assert.equal(tool.done.event.ok, true);
    assert.match(payloadText(tool.started.event, "input"), new RegExp(marker));
    assert.match(payloadText(tool.done.event, "result"), new RegExp(marker));
    assert.match(agentReply(settled.detail, turn).body, new RegExp(marker));
  }
  await expandInlineTools(page, [firstTool.started.event.id, secondTool.started.event.id]);
  await waitForStableDom(page);
  const settledExpanded = await capture("30-settled-expanded", context);
  assertTimelineSurfaces(settledExpanded, frames, [first.turnId, secondTurn.id]);
  assertTimelineRendered(settledExpanded.dom, firstTerminal, durableTimeline(settledExpanded.detail, first.turnId));
  assertTimelineRendered(settledExpanded.dom, secondTerminal, durableTimeline(settledExpanded.detail, secondTurn.id));
  assertReasoningIntegrity(settledExpanded.dom, firstTerminal, durableTimeline(settledExpanded.detail, first.turnId));
  assertReasoningIntegrity(settledExpanded.dom, secondTerminal, durableTimeline(settledExpanded.detail, secondTurn.id));
  const before = timelineProjection(settledExpanded.dom);
  await page.reload({ waitUntil: "domcontentloaded" });
  await page.locator(`[data-message-id="${secondTerminal.agent_message_id}"]`).getByText(secondMarker, { exact: true }).waitFor();
  await expandInlineTools(page, [firstTool.started.event.id, secondTool.started.event.id]);
  const reloaded = await capture("31-reloaded", context);
  const after = timelineProjection(reloaded.dom);
  assert.deepEqual(after, before);
  assert.deepEqual(reloaded.detail.turn_timelines, settledExpanded.detail.turn_timelines);
  assert.deepEqual(reloaded.store.timelineEvents, settledExpanded.store.timelineEvents);
  assertTimelineSurfaces(reloaded, frames, [first.turnId, secondTurn.id]);
  assertTimelineRendered(reloaded.dom, firstTerminal, durableTimeline(reloaded.detail, first.turnId));
  assertTimelineRendered(reloaded.dom, secondTerminal, durableTimeline(reloaded.detail, secondTurn.id));
  assertReasoningIntegrity(reloaded.dom, firstTerminal, durableTimeline(reloaded.detail, first.turnId));
  assertReasoningIntegrity(reloaded.dom, secondTerminal, durableTimeline(reloaded.detail, secondTurn.id));
  return {
    markers: [firstMarker, secondMarker],
    turnIds: [first.turnId, secondTurn.id],
    orderedTimeline: after,
    liveTurnEvents: [first.turnId, secondTurn.id].map(turnId => ({ turnId, events: turnEvents(frames, turnId) })),
    durableTurnTimelines: reloaded.detail.turn_timelines,
    durableStoreEvents: reloaded.store.timelineEvents,
  };
}

async function runTools(context) {
  const { page, frames, nonce, threadId } = context;
  const successMarker = `HIRSEL-TOOL-SUCCESS-${nonce}`;
  const successPrompt = `Call shell.run exactly once with cmd "printf '${successMarker}'". Then report the exact stdout marker ${successMarker}.`;
  const success = await sendMessage(page, frames, threadId, successPrompt);
  const successTerminal = await waitForTurn(frames, success.turnId, turn => terminal(turn.state), "success turn terminal");
  assert.equal(successTerminal.state, "completed");
  const successCapture = await capture("10-success", context);
  assert.match(agentReply(successCapture.detail, successTerminal).body, new RegExp(successMarker));
  const successTool = toolPair(frames, success.turnId, /shell[._]run/);
  assert.equal(successTool.done.event.ok, true);
  assert.match(payloadText(successTool.started.event, "input"), new RegExp(successMarker));
  assert.match(payloadText(successTool.done.event, "result"), new RegExp(successMarker));
  await requireInlineTool(page, successTool.started.event.id, successMarker);

  const failureMarker = `HIRSEL-TOOL-EXPECTED-FAILURE-${nonce}`;
  const missingDir = `/tmp/hirsel-runbook-missing-${nonce}`;
  const failurePrompt = `Call shell.run exactly once with cmd "pwd" and cwd "${missingDir}". It must fail because that directory does not exist. After observing the failed tool result, report the exact marker ${failureMarker} and do not claim the command succeeded.`;
  const failure = await sendMessage(page, frames, threadId, failurePrompt);
  const failureTerminal = await waitForTurn(frames, failure.turnId, turn => terminal(turn.state), "failure turn terminal");
  assert.equal(failureTerminal.state, "completed");
  const failureCapture = await capture("20-failure", context);
  assert.match(agentReply(failureCapture.detail, failureTerminal).body, new RegExp(failureMarker));
  const failureTool = toolPair(frames, failure.turnId, /shell[._]run/);
  assert.equal(failureTool.done.event.ok, false);
  assert.match(payloadText(failureTool.started.event, "input"), new RegExp(missingDir));
  assert.match(payloadText(failureTool.done.event, "result"), /No such file or directory/);
  await requireInlineTool(page, failureTool.started.event.id, "No such file or directory");

  await expandInlineTools(page, [successTool.started.event.id, failureTool.started.event.id]);
  const final = await capture("30-crosscheck", context);
  assert.equal(final.detail.messages.filter(message => message.author === "owner").length, 2);
  assert.equal(final.detail.messages.filter(message => message.author === "agent").length, 2);
  assert.equal(final.store.turns.length, 2);
  const durableCalls = final.detail.messages.flatMap(message => message.tool_calls ?? []);
  for (const pair of [successTool, failureTool]) {
    assert(durableCalls.some(call => call.id === pair.started.event.id && call.ok === pair.done.event.ok));
  }
  assertTimelineSurfaces(final, frames, [success.turnId, failure.turnId]);
  assertTimelineRendered(final.dom, successTerminal, durableTimeline(final.detail, success.turnId));
  assertTimelineRendered(final.dom, failureTerminal, durableTimeline(final.detail, failure.turnId));
  return {
    successMarker,
    failureMarker,
    calls: [successTool.done.event, failureTool.done.event],
  };
}

async function runArtifact(context) {
  const { page, frames, nonce, threadId, scenarioDir } = context;
  const title = `Runbook receipt ${nonce}`;
  const filename = `receipt-${nonce}.txt`;
  const content = `HIRSEL-ARTIFACT-${nonce}\n`;
  const prompt = `Create exactly one reusable file artifact via artifacts.create. Use title "${title}", kind "file", MIME "text/plain", filename "${filename}", and exact UTF-8 content "${content.replace("\n", "\\n")}". Do not merely describe it. Then confirm briefly.`;
  const turn = await sendMessage(page, frames, threadId, prompt);
  const completed = await waitForTurn(frames, turn.turnId, value => terminal(value.state), "artifact turn terminal");
  assert.equal(completed.state, "completed");
  await page.getByText(title, { exact: true }).first().waitFor();
  await capture("10-created", context);
  const tool = toolPair(frames, turn.turnId, /artifacts[._]create/);
  assert.equal(tool.done.event.ok, true);
  assert.match(payloadText(tool.started.event, "input"), new RegExp(title));
  assert.match(payloadText(tool.done.event, "result"), new RegExp(title));
  const upsert = latestFrame(frames, frame => frame.type === "artifact_upsert" && frame.artifact.title === title)?.frame.artifact;
  assert(upsert, "creation emitted no matching artifact_upsert");
  const created = await openThread(context.url, context.token, threadId);
  const agent = created.messages.find(message => message.id === completed.agent_message_id);
  assert(agent?.artifact_ids?.includes(upsert.id), "Agent message does not reference the created artifact");
  const card = page.locator(`[data-message-id="${agent.id}"] [data-artifact-ref="${upsert.id}"]`);
  await card.waitFor({ state: "visible" });

  await page.getByRole("button", { name: "All artifacts", exact: true }).click();
  await page.getByRole("heading", { name: "All artifacts", exact: true }).waitFor();
  const listRow = page.locator(`[data-slot="artifact-list"] [data-artifact-ref="${upsert.id}"]`);
  await listRow.filter({ hasText: title }).waitFor();
  await page.screenshot({ path: join(scenarioDir, "20-listed.png"), fullPage: true });
  await listRow.click();
  const preview = page.locator('[data-slot="artifact-preview"]');
  await preview.waitFor({ state: "visible" });
  await page.frameLocator('[data-slot="artifact-preview"] iframe').getByText(content.trim(), { exact: true }).waitFor();
  await page.screenshot({ path: join(scenarioDir, "21-preview.png"), fullPage: true });
  const downloadPromise = page.waitForEvent("download");
  await preview.getByRole("button", { name: "Download", exact: true }).click();
  const download = await downloadPromise;
  assert.equal(download.suggestedFilename(), filename);
  const downloadPath = join(scenarioDir, filename);
  await download.saveAs(downloadPath);
  assert.equal(await readFile(downloadPath, "utf8"), content);

  await preview.getByRole("button", { name: "Back to conversation", exact: true }).click();
  await page.getByRole("button", { name: "Back to conversation", exact: true }).click();
  await page.reload({ waitUntil: "domcontentloaded" });
  await page.locator(`[data-message-id="${agent.id}"] [data-artifact-ref="${upsert.id}"]`).waitFor();
  await expandInlineTools(page, [tool.started.event.id]);
  const reloaded = await capture("30-reloaded", context);
  const stored = reloaded.store.artifacts.find(artifact => artifact.id === upsert.id);
  assert.deepEqual(
    { title: stored?.title, kind: stored?.kind, mime: stored?.mime, filename: stored?.filename, content: stored?.content },
    { title, kind: "file", mime: "text/plain", filename, content },
  );
  assertTimelineSurfaces(reloaded, frames, [turn.turnId]);
  assertTimelineRendered(reloaded.dom, completed, durableTimeline(reloaded.detail, turn.turnId));

  const naturalOffset = frames.length;
  const natural = await sendMessage(page, frames, threadId, "Make a picture of a cat artifact");
  const naturalCompleted = await waitForTurn(frames, natural.turnId, value => terminal(value.state), "natural cat artifact turn terminal");
  assert.equal(naturalCompleted.state, "completed");
  const naturalTool = toolPair(frames, natural.turnId, /artifacts[._]create/);
  assert.equal(naturalTool.done.event.ok, true);
  const naturalUpserts = frames.slice(naturalOffset)
    .filter(row => row.direction === "received" && row.frame.type === "artifact_upsert")
    .map(row => row.frame.artifact)
    .filter(artifact => artifact.id !== upsert.id);
  const naturalArtifacts = [...new Map(naturalUpserts.map(artifact => [artifact.id, artifact])).values()];
  assert.equal(naturalArtifacts.length, 1, "natural cat request did not create exactly one new artifact");
  const cat = naturalArtifacts[0];
  assert.match(payloadText(naturalTool.started.event, "input"), /cat/i);
  assert.match(payloadText(naturalTool.done.event, "result"), new RegExp(cat.title));
  const catDetail = await openThread(context.url, context.token, threadId);
  const catAgent = agentReply(catDetail, naturalCompleted);
  assert(catAgent.artifact_ids?.includes(cat.id), "natural cat Agent reply does not reference its artifact");
  const storedCatBeforePreview = storeSnapshot(context.dataDir, threadId).artifacts.find(artifact => artifact.id === cat.id);
  assert(storedCatBeforePreview, "natural cat artifact is absent from SQLite");
  assert(/<svg\b|<canvas\b|<img\b|createElement\s*\(\s*["'](?:svg|canvas|img)["']/i.test(storedCatBeforePreview.content), "natural cat artifact has no graphical image surface");
  const catCard = page.locator(`[data-message-id="${catAgent.id}"] [data-artifact-ref="${cat.id}"]`);
  await catCard.waitFor({ state: "visible" });
  await catCard.click();
  const catPreview = page.locator('[data-slot="artifact-preview"]');
  await catPreview.waitFor({ state: "visible" });
  await page.frameLocator('[data-slot="artifact-preview"] iframe').locator("body").waitFor({ state: "visible" });
  await page.screenshot({ path: join(scenarioDir, "40-natural-cat-preview.png"), fullPage: true });
  assert(await page.frameLocator('[data-slot="artifact-preview"] iframe').locator("svg, canvas, img").count() > 0, `natural cat preview rendered ${cat.kind}/${cat.mime} as source instead of an image surface`);
  await catPreview.getByRole("button", { name: "Back to conversation", exact: true }).click();
  await expandInlineTools(page, [tool.started.event.id, naturalTool.started.event.id]);
  const naturalCapture = await capture("41-natural-cat", context);
  const storedCat = naturalCapture.store.artifacts.find(artifact => artifact.id === cat.id);
  assert.deepEqual(
    { id: storedCat?.id, title: storedCat?.title, kind: storedCat?.kind, mime: storedCat?.mime, content: storedCat?.content },
    { id: cat.id, title: cat.title, kind: cat.kind, mime: cat.mime, content: storedCatBeforePreview.content },
  );
  assertTimelineSurfaces(naturalCapture, frames, [turn.turnId, natural.turnId]);
  assertTimelineRendered(naturalCapture.dom, naturalCompleted, durableTimeline(naturalCapture.detail, natural.turnId));
  return {
    exact: { turnId: turn.turnId, toolCallId: tool.started.event.id, artifactId: upsert.id, title, filename, content, downloadPath },
    naturalCat: { prompt: "Make a picture of a cat artifact", turnId: natural.turnId, toolCallId: naturalTool.started.event.id, artifactId: cat.id, title: cat.title, kind: cat.kind, mime: cat.mime },
  };
}

function presentationArtifacts(nonce) {
  const marker = `HIRSEL-PRESENTATION-${nonce}`;
  return [
    {
      format: "html",
      title: `Presentation HTML ${nonce}`,
      kind: "html",
      mime: "text/html",
      filename: `presentation-${nonce}.html`,
      content: ` \n<!doctype html><html><body><main><h1>${marker}-HTML</h1><script>parent.postMessage("${marker}-HTML-RENDERED","*")</script></main></body></html>\n`,
      renderedText: `${marker}-HTML`,
      sideEffect: `${marker}-HTML-RENDERED`,
    },
    {
      format: "markdown",
      title: `Presentation Markdown ${nonce}`,
      kind: "file",
      mime: "text/markdown",
      filename: `presentation-${nonce}.md`,
      content: ` \n# ${marker}-MARKDOWN\n\n**Rendered** markdown with <literal-source>.\n`,
      renderedText: `${marker}-MARKDOWN`,
    },
    {
      format: "svg",
      title: `Presentation SVG ${nonce}`,
      kind: "file",
      mime: "image/svg+xml",
      filename: `presentation-${nonce}.svg`,
      content: ` \n<svg xmlns="http://www.w3.org/2000/svg" width="120" height="80" viewBox="0 0 120 80"><title>${marker}-SVG</title><rect width="120" height="80" rx="12" fill="#17324d"/><circle cx="38" cy="40" r="19" fill="#f5b942"/><path d="M67 56L88 22l20 34z" fill="#64c4a6"/></svg>\n`,
      renderedText: `${marker}-SVG`,
      size: { width: 120, height: 80 },
    },
    {
      format: "solid",
      title: `Presentation Solid ${nonce}`,
      kind: "solid",
      mime: "text/jsx",
      filename: `presentation-${nonce}.jsx`,
      content: ` \nexport default function App(){parent.postMessage("${marker}-SOLID-RENDERED","*");return <main><h1>${marker}-SOLID</h1><button onClick={e=>e.currentTarget.textContent="Pressed"}>Ready</button></main>}\n`,
      renderedText: `${marker}-SOLID`,
      sideEffect: `${marker}-SOLID-RENDERED`,
    },
  ];
}

async function presentationRendered(panel, expected) {
  const frame = panel.frameLocator("iframe");
  if (expected.format === "svg") {
    const image = frame.locator(`img[alt="${expected.title}"]`);
    await image.waitFor({ state: "visible" });
    assert.deepEqual(await image.evaluate(node => ({ width: node.naturalWidth, height: node.naturalHeight })), expected.size);
  } else {
    await frame.getByRole("heading", { name: expected.renderedText, exact: true }).waitFor({ state: "visible" });
  }
}

async function presentationDownload(page, panel, buttonName, expected, suffix) {
  const pending = page.waitForEvent("download");
  await panel.getByRole("button", { name: buttonName, exact: true }).click();
  const download = await pending;
  assert.equal(download.suggestedFilename(), expected.filename);
  const path = join(page.__presentationEvidenceDir, `${suffix}-${expected.filename}`);
  await download.saveAs(path);
  assert.deepEqual(await readFile(path), Buffer.from(expected.content), `${expected.format} download bytes differ`);
  return path;
}

async function presentationFit(page, panel) {
  const metrics = await panel.evaluate(node => ({
    pageOverflow: document.documentElement.scrollWidth > window.innerWidth,
    panelOverflow: node.scrollWidth > node.clientWidth,
    viewportWidth: window.innerWidth,
    controls: [...node.querySelectorAll("header button")]
      .filter(button => button.getClientRects().length > 0)
      .map(button => { const box = button.getBoundingClientRect(); return { label: button.getAttribute("aria-label") ?? button.textContent?.trim(), left: box.left, right: box.right }; }),
  }));
  assert.equal(metrics.pageOverflow, false, "presentation page has horizontal overflow");
  assert.equal(metrics.panelOverflow, false, "presentation panel has horizontal overflow");
  for (const control of metrics.controls) assert(control.left >= 0 && control.right <= metrics.viewportWidth, `${control.label} is outside the viewport`);
  return metrics;
}

async function exercisePresentationSurface(page, panel, expected, label, buttonName, sideEffects, workerRequests) {
  const rendered = panel.getByRole("button", { name: "Rendered", exact: true });
  const source = panel.getByRole("button", { name: "Source", exact: true });
  await rendered.waitFor({ state: "visible" });
  assert.equal(await rendered.getAttribute("aria-pressed"), "true", `${label} did not default to Rendered`);
  await presentationRendered(panel, expected);
  if (expected.sideEffect) await poll(`${label} rendered side effect`, () => sideEffects.includes(expected.sideEffect), 5_000);
  await page.screenshot({ path: join(page.__presentationEvidenceDir, `${label}-rendered.png`), fullPage: true });
  const renderedDownload = await presentationDownload(page, panel, buttonName, expected, `${label}-rendered`);

  const effectsBeforeSource = sideEffects.length;
  const workersBeforeSource = workerRequests.length;
  await source.focus();
  await source.press("Enter");
  assert.equal(await source.getAttribute("aria-pressed"), "true");
  assert.equal(await source.evaluate(node => node === document.activeElement), true, `${label} Source lost keyboard focus`);
  const pre = panel.locator('[data-slot="artifact-source"]');
  await pre.waitFor({ state: "visible" });
  assert.equal(await pre.textContent(), expected.content, `${label} Source differs from exact stored content`);
  assert.equal(await panel.locator("iframe").count(), 0, `${label} Source mounted a renderer`);
  assert.equal(await pre.locator("html,svg,script,img,canvas,button").count(), 0, `${label} Source interpreted markup`);
  assert.equal(sideEffects.length, effectsBeforeSource, `${label} Source executed an artifact side effect`);
  assert.equal(workerRequests.length, workersBeforeSource, `${label} Source started the compiler worker`);
  await page.screenshot({ path: join(page.__presentationEvidenceDir, `${label}-source.png`), fullPage: true });
  const sourceDownload = await presentationDownload(page, panel, buttonName, expected, `${label}-source`);
  const fit = await presentationFit(page, panel);

  await rendered.focus();
  await rendered.press("Enter");
  assert.equal(await rendered.getAttribute("aria-pressed"), "true");
  assert.equal(await rendered.evaluate(node => node === document.activeElement), true, `${label} Rendered lost keyboard focus`);
  await presentationRendered(panel, expected);
  return { renderedDownload, sourceDownload, fit };
}

async function runArtifactPresentation(context) {
  const { page, frames, nonce, threadId, scenarioDir } = context;
  page.__presentationEvidenceDir = scenarioDir;
  const expected = presentationArtifacts(nonce);
  const prompt = `Create exactly four reusable artifacts by calling artifacts.create exactly four times. Use these exact JSON fields and exact UTF-8 content, including leading whitespace and trailing newlines: ${JSON.stringify(expected.map(({ format: _format, renderedText: _renderedText, sideEffect: _sideEffect, size: _size, ...artifact }) => artifact))}. Do not edit, show, or merely describe them. After all four calls succeed, confirm briefly.`;
  const offset = frames.length;
  const request = await sendMessage(page, frames, threadId, prompt);
  const completed = await waitForTurn(frames, request.turnId, turn => terminal(turn.state), "presentation artifact turn terminal");
  assert.equal(completed.state, "completed");
  const events = turnEvents(frames, request.turnId);
  const starts = events.filter(frame => frame.event.kind === "tool_start" && /artifacts[._]create/.test(frame.event.name));
  const dones = events.filter(frame => frame.event.kind === "tool_done" && starts.some(start => start.event.id === frame.event.id));
  assert.equal(starts.length, 4, "presentation turn did not call artifacts.create exactly four times");
  assert.equal(dones.length, 4, "presentation turn did not finish four artifacts.create calls");
  assert(dones.every(frame => frame.event.ok), "a presentation artifacts.create call failed");
  const upserts = [...new Map(frames.slice(offset)
    .filter(row => row.direction === "received" && row.frame.type === "artifact_upsert")
    .map(row => [row.frame.artifact.id, row.frame.artifact])).values()];
  assert.equal(upserts.length, 4, "presentation turn did not emit exactly four artifact upserts");
  await expandInlineTools(page, starts.map(frame => frame.event.id));
  const initial = await capture("10-created-formats", context);
  const stored = expected.map(item => {
    const artifact = initial.store.artifacts.find(candidate => candidate.title === item.title);
    assert(artifact, `${item.format} artifact is absent from SQLite`);
    assert.deepEqual(
      { title: artifact.title, kind: artifact.kind, mime: artifact.mime, filename: artifact.filename, content: artifact.content },
      { title: item.title, kind: item.kind, mime: item.mime, filename: item.filename, content: item.content },
    );
    return { ...item, id: artifact.id };
  });
  assertTimelineSurfaces(initial, frames, [request.turnId]);
  assertTimelineRendered(initial.dom, completed, durableTimeline(initial.detail, request.turnId));
  assertReasoningIntegrity(initial.dom, completed, durableTimeline(initial.detail, request.turnId));

  const sideEffects = [];
  const workerRequests = [];
  page.on("console", () => {});
  await page.exposeFunction("recordPresentationSideEffect", value => sideEffects.push(value));
  await page.evaluate(() => addEventListener("message", event => window.recordPresentationSideEffect(event.data)));
  page.on("request", requestEvent => { if (/compiler\.worker/i.test(requestEvent.url())) workerRequests.push(requestEvent.url()); });
  const results = [];
  for (const viewport of [{ width: 1440, height: 900 }, { width: 390, height: 844 }]) {
    await page.setViewportSize(viewport);
    const phone = viewport.width < 1024;
    for (const artifact of stored) {
      const card = page.locator(`[data-artifact-ref="${artifact.id}"]`).first();
      await card.waitFor({ state: "visible" });
      await card.click();
      let panel = page.locator('[data-slot="artifact-preview"]');
      await panel.waitFor({ state: "visible" });
      const preview = await exercisePresentationSurface(page, panel, artifact, `${phone ? "phone" : "desktop"}-${artifact.format}-preview`, "Download", sideEffects, workerRequests);
      await panel.getByRole("button", { name: "Back to conversation", exact: true }).click();
      await card.click();
      panel = page.locator('[data-slot="artifact-preview"]');
      await panel.waitFor({ state: "visible" });
      assert.equal(await panel.getByRole("button", { name: "Rendered", exact: true }).getAttribute("aria-pressed"), "true", `${artifact.format} reopened preview did not reset to Rendered`);
      await presentationRendered(panel, artifact);
      await panel.getByRole("button", { name: "Back to conversation", exact: true }).click();

      await card.locator("..").getByRole("button", { name: "Artifact actions", exact: true }).click();
      await page.getByRole("menuitem", { name: "Showcase in this thread", exact: true }).click();
      if (phone) await page.getByRole("button", { name: "Show showcase", exact: true }).click();
      panel = page.locator('[data-slot="thread-showcase"]');
      await panel.waitFor({ state: "visible" });
      const showcase = await exercisePresentationSurface(page, panel, artifact, `${phone ? "phone" : "desktop"}-${artifact.format}-showcase`, "Download showcase", sideEffects, workerRequests);
      if (phone) await panel.getByRole("button", { name: "Back to conversation", exact: true }).click();
      results.push({ viewport, format: artifact.format, artifactId: artifact.id, preview, showcase });
    }
  }

  await page.reload({ waitUntil: "domcontentloaded" });
  const last = stored.at(-1);
  const lastCard = page.locator(`[data-artifact-ref="${last.id}"]`).first();
  await lastCard.waitFor({ state: "visible" });
  await lastCard.click();
  let panel = page.locator('[data-slot="artifact-preview"]');
  await panel.waitFor({ state: "visible" });
  assert.equal(await panel.getByRole("button", { name: "Rendered", exact: true }).getAttribute("aria-pressed"), "true");
  await presentationRendered(panel, last);
  await panel.getByRole("button", { name: "Back to conversation", exact: true }).click();
  await page.getByRole("button", { name: "Show showcase", exact: true }).click();
  panel = page.locator('[data-slot="thread-showcase"]');
  await panel.waitFor({ state: "visible" });
  assert.equal(await panel.getByRole("button", { name: "Rendered", exact: true }).getAttribute("aria-pressed"), "true");
  await presentationRendered(panel, last);
  await page.screenshot({ path: join(scenarioDir, "90-phone-reloaded.png"), fullPage: true });
  const final = await capture("91-final-crosscheck", context);
  assertTimelineSurfaces(final, frames, [request.turnId]);
  return { turnId: request.turnId, toolCallIds: starts.map(frame => frame.event.id), artifacts: stored, results, sideEffects, workerRequests };
}

async function prepareNativeWorkerFixture(scenarioDir, nonce) {
  const fixtureDir = join(scenarioDir, "fixture");
  const passMarker = `FOCUSED_TEST_PASS_${nonce}`;
  const summaryMarker = `WORKER_SUMMARY_${nonce}`;
  await mkdir(fixtureDir, { recursive: true });
  await Promise.all([
    writeFile(join(fixtureDir, "calculator.mjs"), "export function add(left, right) {\n  return left - right;\n}\n"),
    writeFile(join(fixtureDir, "test-calculator.mjs"), `import assert from "node:assert/strict";\nimport { add } from "./calculator.mjs";\n\nassert.equal(add(2, 3), 5, "add must sum both operands");\nawait new Promise(resolve => setTimeout(resolve, 20_000));\nconsole.log("${passMarker}");\n`),
  ]);
  return { fixtureDir, passMarker, summaryMarker };
}

async function selectThreadInBrowser(page, threadId, parentThreadId = null) {
  let row = page.locator(`[data-thread-row="${threadId}"]`);
  if (!(await row.count()) || !(await row.first().isVisible())) {
    const opener = page.getByRole("button", { name: /^(Spaces and Tasks|Browse Spaces and Tasks)$/ }).first();
    if (await opener.count()) await opener.click();
  }
  row = page.locator(`[data-thread-row="${threadId}"]`);
  if ((!(await row.count()) || !(await row.first().isVisible())) && parentThreadId !== null) {
    const parentRow = page.locator(`[data-thread-row="${parentThreadId}"]`).first();
    const expand = parentRow.locator("xpath=..").getByRole("button", { name: /^Expand / });
    if (await expand.count()) await expand.first().click();
  }
  row = page.locator(`[data-thread-row="${threadId}"]`).first();
  await row.waitFor({ state: "visible", timeout: 10_000 });
  await row.click();
  await page.locator(`main[data-thread-id="${threadId}"]`).waitFor({ state: "visible" });
  assert.equal(new URL(page.url()).pathname, `/t/${threadId}`);
}

function toolEvents(frames, turnId) {
  return turnEvents(frames, turnId)
    .map(frame => frame.event)
    .filter(event => event.kind === "tool_start" || event.kind === "tool_done");
}

function startedTools(frames, turnId) {
  return toolEvents(frames, turnId).filter(event => event.kind === "tool_start");
}

function completedTool(frames, turnId, toolStart) {
  const done = toolEvents(frames, turnId).find(event => event.kind === "tool_done" && event.id === toolStart.id);
  assert(done, `tool ${toolStart.id} has no matching result`);
  return done;
}

function parseStoredJson(row, field) {
  assert(row?.[field], `missing stored ${field}`);
  return JSON.parse(row[field]);
}

function executionStarted(store, turnId) {
  const row = store.activities.find(activity => activity.turn_id === turnId && activity.kind === "execution_started");
  assert(row, `turn ${turnId} has no execution_started activity`);
  return parseStoredJson(row, "data");
}

function assertNativeWorkerExecution(store, turnId, fixtureDir) {
  const row = store.turns.find(turn => turn.id === turnId);
  const execution = parseStoredJson(row, "accepted_execution");
  assert.equal(execution.backend, "lash_worker");
  assert.equal(execution.provider.id, "openrouter");
  assert.equal(execution.provider.base_url, "https://openrouter.ai/api/v1");
  assert.match(execution.provider.revision, /^sha256:[a-f0-9]{64}$/);
  assert.equal(execution.model, "deepseek/deepseek-v4.1-flash");
  assert.equal(execution.variant, "default");
  assert.equal(execution.cwd, fixtureDir);
  assert.equal(execution.tool_profile, "hirsel.native-coding.v1");
  return execution;
}

function assertNativeToolCatalog(store, childThreadId) {
  const names = store.nativeWorkerMeta.find(row => row.key === `thread:${childThreadId}:native_worker_tool_names`);
  assert.deepEqual(JSON.parse(names?.value ?? "null"), ["edit", "exec_command", "read", "write"]);
  const preference = parseStoredJson(store.executionPreference[0], "config");
  assert.equal(preference.backend, "lash_worker");
}

function assertChildTaskOpen(store, parentThreadId, childThreadId) {
  assert.equal(store.threads.length, 2, "isolated run contains unrelated Threads");
  const child = store.threads.find(thread => thread.id === childThreadId);
  assert.deepEqual(
    { kind: child?.kind, parent: child?.parent_thread_id, settled: child?.settled_at, archived: child?.archived_at },
    { kind: "task", parent: parentThreadId, settled: null, archived: null },
  );
}

async function runNativeLashWorker(context, fixture) {
  const { page, frames, nonce, threadId: parentThreadId, scenarioDir } = context;
  const childTitle = `Native worker ${nonce}`;
  const parentMarker = `PARENT_DELEGATED_${nonce}`;
  const finalMarker = `WORKER_FIXED_${nonce}`;
  const followupMarker = `FOLLOWUP_CONTEXT_CONFIRMED_${nonce}`;
  const contextMarker = `PRIVATE_ASSIGNMENT_FACT_${nonce}`;
  const summaryText = `${fixture.summaryMarker}: changed calculator.mjs; focused test passed (${fixture.passMarker}).`;
  const brief = [
    `Work only in ${fixture.fixtureDir}.`,
    `Retain this private assignment fact for a later follow-up: ${contextMarker}.`,
    "Do not write that private assignment fact or marker into any file, and do not include it in this turn's final response.",
    "Use read to inspect calculator.mjs and test-calculator.mjs.",
    "Run exec_command with exactly `node test-calculator.mjs` and observe the focused test fail without changing the test.",
    "Use edit to replace the unique incorrect expression `left - right` with `left + right` in calculator.mjs.",
    `Use write to create worker-summary.txt with exactly: ${summaryText}`,
    "Run exec_command again with exactly `node test-calculator.mjs` and observe it pass.",
    `Return a concise changed-files and checks summary ending with exactly ${finalMarker}.`,
  ].join(" ");
  const parentPrompt = [
    "Delegate exactly one new child Task using threads_delegate.",
    `Use title ${JSON.stringify(childTitle)}, agent "lash", cwd ${JSON.stringify(fixture.fixtureDir)}, and artifact_ids [].`,
    "Omit provider_id, model, and variant so the native worker defaults are exercised.",
    `Use this exact assignment brief: ${JSON.stringify(brief)}.`,
    `After the delegation is accepted, do not inspect or follow up with the child; end your reply with exactly ${parentMarker}.`,
  ].join(" ");

  const parent = await sendMessage(page, frames, parentThreadId, parentPrompt);
  const childThread = await poll("native child Task publication", () => latestFrame(
    frames,
    frame => frame.type === "thread_upsert" && frame.thread.parent_thread_id === parentThreadId && frame.thread.title === childTitle,
  )?.frame.thread);
  const childThreadId = childThread.id;
  const firstTurn = await poll("native child turn acceptance", () => latestFrame(
    frames,
    frame => frame.type === "thread_turn" && frame.turn.thread_id === childThreadId
      && frame.turn.requester_thread_id === parentThreadId && frame.turn.requester_turn_id === parent.turnId,
  )?.frame.turn);
  await waitForTurn(frames, firstTurn.id, turn => turn.state === "running", "native child turn running");
  await selectThreadInBrowser(page, childThreadId, parentThreadId);

  const secondCommand = await poll("passing focused test command started", () => {
    const commands = startedTools(frames, firstTurn.id).filter(event => event.name === "exec_command");
    if (commands.length < 2) return null;
    const candidate = commands[1];
    return toolEvents(frames, firstTurn.id).some(event => event.kind === "tool_done" && event.id === candidate.id) ? null : candidate;
  });
  await selectThreadInBrowser(page, parentThreadId);
  const parentComposer = page.locator(`main[data-thread-id="${parentThreadId}"] textarea`);
  const draftMarker = `RESPONSIVE_DRAFT_${nonce}`;
  await parentComposer.fill(draftMarker);
  assert.equal(await parentComposer.inputValue(), draftMarker);
  assert.equal(await parentComposer.isEnabled(), true);
  const responsive = await captureNativeWorker("10-parent-responsive", context, parentThreadId, parentThreadId, childThreadId);
  assert.equal(responsive.dom.entries.some(entry => entry.text.includes(parentPrompt)), true);
  assert.equal(latestFrame(frames, frame => frame.type === "thread_turn" && frame.turn.id === firstTurn.id)?.frame.turn.state, "running");
  assert.equal(toolEvents(frames, firstTurn.id).some(event => event.kind === "tool_done" && event.id === secondCommand.id), false, "passing command ended before responsiveness evidence");
  await parentComposer.fill("");
  await selectThreadInBrowser(page, childThreadId, parentThreadId);

  const firstTerminal = await waitForTurn(frames, firstTurn.id, turn => terminal(turn.state), "native child initial turn terminal");
  assert.equal(firstTerminal.state, "completed");
  const parentTerminal = await waitForTurn(frames, parent.turnId, turn => terminal(turn.state), "parent delegation turn terminal");
  assert.equal(parentTerminal.state, "completed");
  assert.match(agentReply(await openThread(context.url, context.token, parentThreadId), parentTerminal).body, new RegExp(parentMarker));
  const firstStarts = startedTools(frames, firstTurn.id);
  assert.deepEqual([...new Set(firstStarts.map(event => event.name))].sort(), ["edit", "exec_command", "read", "write"]);
  const firstCommand = firstStarts.find(event => event.name === "exec_command");
  const edit = firstStarts.find(event => event.name === "edit");
  const write = firstStarts.find(event => event.name === "write");
  assert(firstCommand && edit && write);
  const orderedNames = firstStarts.map(event => event.name);
  assert(orderedNames.indexOf("read") < orderedNames.indexOf("exec_command"));
  assert(orderedNames.indexOf("exec_command") < orderedNames.indexOf("edit"));
  assert(orderedNames.indexOf("edit") < orderedNames.indexOf("write"));
  assert(orderedNames.lastIndexOf("write") < orderedNames.lastIndexOf("exec_command"));
  assert.match(payloadText(completedTool(frames, firstTurn.id, firstCommand), "result"), /AssertionError|add must sum both operands|exit(?:ed|_code)?.*[1-9]|status.*[1-9]/i);
  assert.match(payloadText(completedTool(frames, firstTurn.id, secondCommand), "result"), new RegExp(fixture.passMarker));
  assert.match(payloadText(edit, "input"), /left - right/);
  assert.match(payloadText(write, "input"), new RegExp(fixture.summaryMarker));
  assert.equal(payloadText(edit, "input").includes(contextMarker), false, "edit persisted the private context marker");
  assert.equal(payloadText(write, "input").includes(contextMarker), false, "write persisted the private context marker");
  const firstToolIds = firstStarts.map(event => event.id);
  await expandInlineTools(page, firstToolIds);
  const firstCapture = await captureNativeWorker("20-initial-complete", context, childThreadId, parentThreadId, childThreadId);
  assertTimelineSurfaces(firstCapture, frames, [firstTurn.id]);
  assertTimelineRendered(firstCapture.dom, firstTerminal, durableTimeline(firstCapture.detail, firstTurn.id));
  assertReasoningIntegrity(firstCapture.dom, firstTerminal, durableTimeline(firstCapture.detail, firstTurn.id));
  assert(durableTimeline(firstCapture.detail, firstTurn.id).some(record => record.event.kind === "reasoning"));
  assert(durableTimeline(firstCapture.detail, firstTurn.id).some(record => record.event.kind === "prose"));
  const initialReply = agentReply(firstCapture.detail, firstTerminal).body;
  assert.match(initialReply, new RegExp(finalMarker));
  assert.equal(initialReply.includes(contextMarker), false, "initial reply echoed the private context marker");
  assertNativeToolCatalog(firstCapture.nativeStore, childThreadId);
  assertNativeWorkerExecution(firstCapture.nativeStore, firstTurn.id, fixture.fixtureDir);
  assertChildTaskOpen(firstCapture.nativeStore, parentThreadId, childThreadId);
  const firstSession = executionStarted(firstCapture.nativeStore, firstTurn.id);
  assert.deepEqual(
    { agent: firstSession.agent, provider: firstSession.provider_id, model: firstSession.model },
    { agent: "lash", provider: "openrouter", model: "deepseek/deepseek-v4.1-flash" },
  );
  const reportsBeforeFollowup = firstCapture.nativeStore.reports.length;
  assert.equal(reportsBeforeFollowup, 1, "initial child turn did not create exactly one terminal parent report");

  const followupPrompt = `Continue this same Task. Without rerunning tests or rereading calculator.mjs or test-calculator.mjs, use read exactly once on worker-summary.txt. Then identify the source file changed in the prior turn and whether its focused test passed. Also recall the private assignment fact from the initial brief and include its exact marker in your reply; its value is intentionally not repeated here. End with exactly ${followupMarker}.`;
  assert.equal(followupPrompt.includes(contextMarker), false, "follow-up prompt repeated the context answer");
  const followup = await sendMessage(page, frames, childThreadId, followupPrompt);
  const followupTerminal = await waitForTurn(frames, followup.turnId, turn => terminal(turn.state), "native child follow-up terminal");
  assert.equal(followupTerminal.state, "completed");
  assert.equal(followupTerminal.requester_thread_id, parentThreadId);
  const followupStarts = startedTools(frames, followup.turnId);
  assert.deepEqual(followupStarts.map(event => event.name), ["read"]);
  assert.match(payloadText(followupStarts[0], "input"), /worker-summary\.txt/);
  assert.equal(payloadText(followupStarts[0], "input").includes(contextMarker), false, "follow-up read input contained the context answer");
  assert.equal(payloadText(completedTool(frames, followup.turnId, followupStarts[0]), "result").includes(contextMarker), false, "worker summary leaked the context answer");
  await expandInlineTools(page, followupStarts.map(event => event.id));
  const followupCapture = await captureNativeWorker("30-followup-complete", context, childThreadId, parentThreadId, childThreadId);
  assertTimelineSurfaces(followupCapture, frames, [firstTurn.id, followup.turnId]);
  assertTimelineRendered(followupCapture.dom, followupTerminal, durableTimeline(followupCapture.detail, followup.turnId));
  const followupReply = agentReply(followupCapture.detail, followupTerminal).body;
  assert.match(followupReply, /calculator\.mjs/);
  assert.match(followupReply, /pass/i);
  assert.match(followupReply, new RegExp(contextMarker));
  assert.match(followupReply, new RegExp(followupMarker));
  assertNativeWorkerExecution(followupCapture.nativeStore, followup.turnId, fixture.fixtureDir);
  assertNativeToolCatalog(followupCapture.nativeStore, childThreadId);
  assertChildTaskOpen(followupCapture.nativeStore, parentThreadId, childThreadId);
  const followupSession = executionStarted(followupCapture.nativeStore, followup.turnId);
  assert.equal(followupSession.session_id, firstSession.session_id, "follow-up did not retain the native worker session");
  assert.equal(followupCapture.nativeStore.reports.length, reportsBeforeFollowup + 1, "follow-up did not add exactly one terminal parent report");
  assert.deepEqual(
    followupCapture.nativeStore.reports.map(report => report.child_turn_id).sort((a, b) => a - b),
    [firstTurn.id, followup.turnId].sort((a, b) => a - b),
  );
  const childReports = followupCapture.nativeStore.activities.filter(activity => activity.thread_id === parentThreadId && activity.kind === "child_report");
  assert.equal(childReports.length, 2, "parent has missing or duplicate child report activities");

  const [source, test, summary] = await Promise.all([
    readFile(join(fixture.fixtureDir, "calculator.mjs"), "utf8"),
    readFile(join(fixture.fixtureDir, "test-calculator.mjs"), "utf8"),
    readFile(join(fixture.fixtureDir, "worker-summary.txt"), "utf8"),
  ]);
  assert.match(source, /return left \+ right;/);
  assert.equal(summary.trimEnd(), summaryText);
  for (const [name, content] of [["calculator.mjs", source], ["test-calculator.mjs", test], ["worker-summary.txt", summary]]) {
    assert.equal(content.includes(contextMarker), false, `${name} persisted the private context marker`);
  }
  await writeFile(join(scenarioDir, "fixture-final.json"), `${JSON.stringify({ source, test, summary }, null, 2)}\n`);

  return {
    parentThreadId,
    parentTurnId: parent.turnId,
    childThreadId,
    initialWorkerTurnId: firstTurn.id,
    followupWorkerTurnId: followup.turnId,
    provider: "openrouter",
    model: "deepseek/deepseek-v4.1-flash",
    variant: "default",
    workerModelTurns: 2,
    toolCallIds: {
      initial: firstToolIds,
      followup: followupStarts.map(event => event.id),
    },
    parentReportActivityIds: childReports.map(activity => activity.id),
    objectiveGates: {
      acceptedDefault: "PASS",
      isolatedFourToolCatalog: "PASS",
      failingFixedPassing: "PASS",
      chronologicalThreeSurfaceEvidence: "PASS",
      parentResponsiveDuringCommand: "PASS",
      sameTaskFollowupContext: "PASS",
      oneTerminalReportPerWorkerTurn: "PASS",
      taskRemainsOpen: "PASS",
    },
    judgedScorecard: {
      status: "NOT_JUDGED",
      instruction: "Inspect 10-parent-responsive.png, 20-initial-complete.png, and 30-followup-complete.png plus their DOM, thread, native-store, and frames evidence before assigning a product verdict.",
    },
  };
}

async function stopProcess(child) {
  if (!child || child.exitCode !== null || child.signalCode !== null) return;
  try { process.kill(-child.pid, "SIGTERM"); } catch { return; }
  await Promise.race([
    new Promise(resolve => child.once("exit", resolve)),
    new Promise(resolve => setTimeout(resolve, 5_000)),
  ]);
  if (child.exitCode === null && child.signalCode === null) {
    try { process.kill(-child.pid, "SIGKILL"); } catch { /* already stopped */ }
  }
}

async function runScenario(scenario) {
  const scenarioDir = join(evidenceRoot, scenario);
  const dataDir = join(scenarioDir, "state");
  await mkdir(dataDir, { recursive: true });
  const token = `runbook-${crypto.randomUUID()}`;
  const nonce = `${scenario.replaceAll("-", "").slice(0, 8)}-${crypto.randomUUID().slice(0, 8)}`;
  const nativeFixture = scenario === "native-lash-worker"
    ? await prepareNativeWorkerFixture(scenarioDir, nonce)
    : null;
  const port = await unusedPort();
  const url = `http://127.0.0.1:${port}`;
  const binary = join(cargoTargetDirectory(), "debug", "hirsel-host");
  const logStream = createWriteStream(join(scenarioDir, "host.log"), { flags: "a" });
  const host = spawn(binary, [], {
    cwd: repo,
    detached: true,
    stdio: ["ignore", "pipe", "pipe"],
    env: {
      ...process.env,
      HIRSEL_TOKEN: token,
      HIRSEL_AGENT: "lash",
      HIRSEL_DRIVER: "real",
      HIRSEL_PROVIDER: "codex",
      HIRSEL_MODEL: "gpt-5.6-sol",
      HIRSEL_DEBUG: "1",
      HIRSEL_IROH: "0",
      HIRSEL_DATA_DIR: dataDir,
      HIRSEL_CONFIG: join(dataDir, "hirsel.toml"),
      HIRSEL_TEMPLATES_DIR: join(repo, "templates"),
      HIRSEL_APP_DIR: join(repo, "app", "dist"),
      HIRSEL_LISTEN: `127.0.0.1:${port}`,
      RUST_LOG: "hirsel_host=info,tower_http=info",
    },
  });
  host.stdout.pipe(logStream, { end: false });
  host.stderr.pipe(logStream, { end: false });
  let browser;
  let page;
  const frames = [];
  const browserErrors = [];
  const manifest = {
    scenario,
    runId,
    nonce,
    sourceHead: git("rev-parse", "HEAD"),
    sourceTree: git("rev-parse", "HEAD^{tree}"),
    dirty: git("status", "--short"),
    url,
    port,
    dataDir,
    initialModelCallBudget: scenario === "artifact-presentation" ? 1 : 2,
    workerModelTurnBudget: scenario === "native-lash-worker" ? 2 : 0,
    fixtureDir: nativeFixture?.fixtureDir ?? null,
  };
  await writeFile(join(scenarioDir, "manifest.json"), `${JSON.stringify(manifest, null, 2)}\n`);
  const result = {
    ...manifest,
    objectiveStatus: "ABORT",
    scorecardStatus: "NOT_JUDGED",
    startedAt: new Date().toISOString(),
  };
  try {
    await poll("isolated Host readiness", async () => {
      if (host.exitCode !== null || host.signalCode !== null) throw new Error(`Host exited (${host.exitCode ?? host.signalCode})`);
      return (await fetch(`${url}/readyz`)).ok;
    }, 60_000);
    browser = await chromium.launch({
      headless: true,
      executablePath: process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH
        ?? "/home/sam/.cache/ms-playwright/chromium_headless_shell-1234/chrome-headless-shell-linux64/chrome-headless-shell",
    });
    page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
    page.on("pageerror", error => browserErrors.push({ type: "pageerror", message: error.message }));
    page.on("console", message => {
      if (message.type() === "error") browserErrors.push({ type: "console", message: message.text() });
    });
    page.on("websocket", socket => {
      socket.on("framesent", event => {
        const frame = parseFrame(event.payload);
        if (frame?.type === "hello") frames.push({ at: new Date().toISOString(), direction: "sent", frame: { type: "hello", auth: "<redacted>" } });
        else if (frame) frames.push({ at: new Date().toISOString(), direction: "sent", frame });
      });
      socket.on("framereceived", event => {
        const frame = parseFrame(event.payload);
        if (frame) frames.push({ at: new Date().toISOString(), direction: "received", frame });
      });
    });
    await page.addInitScript(value => {
      if (window === window.top) localStorage.setItem("hirsel.token", value);
    }, token);
    await page.goto(url, { waitUntil: "domcontentloaded" });
    const hello = await poll("hello_ok", () => latestFrame(frames, frame => frame.type === "hello_ok")?.frame);
    result.servedModel = hello.model?.current ?? null;
    result.hostVersion = hello.host_version;
    const threadId = await createThread(page, nonce);
    const context = { page, frames, nonce, threadId, scenarioDir, dataDir, url, token };
    const empty = await capture("00-empty", context);
    assert.equal(empty.dom.entries.length, 0);
    assert.equal(empty.detail.messages.length, 0);
    assert.equal(empty.detail.turns.length, 0);
    assert.equal(empty.store.messages.length, 0);
    assert.equal(empty.store.turns.length, 0);
    assert.equal(empty.store.schemaVersion, 6);
    assert.deepEqual(empty.store.timelineEvents, []);
    assert.deepEqual(empty.detail.turn_timelines, []);
    if (scenario === "artifact-creation" || scenario === "artifact-presentation") assert.equal(empty.store.artifacts.length, 0);

    result.detail = scenario === "chat-chronology"
      ? await runChat(context)
      : scenario === "tool-execution"
        ? await runTools(context)
        : scenario === "artifact-creation"
          ? await runArtifact(context)
          : scenario === "artifact-presentation"
            ? await runArtifactPresentation(context)
            : await runNativeLashWorker(context, nativeFixture);
    assert.deepEqual(browserErrors, [], `browser errors: ${JSON.stringify(browserErrors)}`);
    result.objectiveStatus = "OBJECTIVE_PASS";
  } catch (error) {
    result.error = error instanceof Error ? `${error.name}: ${error.message}` : String(error);
    if (page) {
      try {
        await page.screenshot({ path: join(scenarioDir, "ABORT.png"), fullPage: true });
        const match = new URL(page.url()).pathname.match(/^\/t\/(\d+)$/);
        if (match) await capture("ABORT", { page, scenarioDir, dataDir, url, token, threadId: Number(match[1]) });
      } catch (captureError) {
        result.captureError = captureError instanceof Error ? captureError.message : String(captureError);
      }
    }
  } finally {
    result.browserErrors = browserErrors;
    result.finishedAt = new Date().toISOString();
    await writeFile(
      join(scenarioDir, "frames.ndjson"),
      frames.map(row => JSON.stringify(sanitizeEvidence(row))).join("\n") + (frames.length ? "\n" : ""),
    );
    await writeFile(join(scenarioDir, "result.json"), `${JSON.stringify(sanitizeEvidence(result), null, 2)}\n`);
    await browser?.close();
    await stopProcess(host);
    await new Promise(resolve => logStream.end(resolve));
    await Promise.all([
      sanitizeEvidenceFile(join(dataDir, "hirsel.toml")),
      sanitizeEvidenceFile(join(scenarioDir, "host.log")),
    ]);
  }
  console.log(`${scenario}: ${result.objectiveStatus}${result.error ? ` — ${result.error}` : ""}`);
  return result;
}

const results = [];
for (const scenario of scenarios) results.push(await runScenario(scenario));
await writeFile(join(evidenceRoot, "summary.json"), `${JSON.stringify(sanitizeEvidence(results), null, 2)}\n`);
if (results.some(result => result.objectiveStatus !== "OBJECTIVE_PASS")) process.exitCode = 1;
