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
  ? ["chat-chronology", "tool-execution", "artifact-creation"]
  : [requested];
const knownScenarios = new Set(["chat-chronology", "tool-execution", "artifact-creation"]);
for (const scenario of scenarios) assert(knownScenarios.has(scenario), `Unknown product runbook: ${scenario}`);

const runId = `${new Date().toISOString().replaceAll(/[:.]/g, "-")}-${process.pid}`;
const evidenceRoot = process.env.HIRSEL_RUNBOOK_ARTIFACTS
  ?? join("/tmp", `hirsel-product-runbooks-${runId}`);
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
  const output = execFileSync("sqlite3", ["-json", database, sql], { encoding: "utf8" }).trim();
  return output ? JSON.parse(output) : [];
}

function storeSnapshot(dataDir, threadId) {
  const database = join(dataDir, "hirsel.sqlite");
  return {
    messages: sqliteJson(database, `SELECT id,thread_id,author,body,ref,ts,tool_calls FROM chat_messages WHERE thread_id=${threadId} ORDER BY id`),
    turns: sqliteJson(database, `SELECT id,thread_id,owner_message_id,agent_message_id,state,started_at,finished_at FROM thread_turns WHERE thread_id=${threadId} ORDER BY id`),
    activities: sqliteJson(database, `SELECT id,thread_id,turn_id,kind,data,ts FROM thread_activities WHERE thread_id=${threadId} ORDER BY id`),
    artifacts: sqliteJson(database, `SELECT a.id,a.title,json_extract(a.kind,'$') AS kind,a.mime,a.filename,a.content,a.created_at,a.updated_at FROM artifacts a WHERE EXISTS (SELECT 1 FROM message_artifacts ma JOIN chat_messages m ON m.id=ma.message_id WHERE ma.artifact_id=a.id AND m.thread_id=${threadId}) ORDER BY a.id`),
    messageArtifacts: sqliteJson(database, `SELECT ma.message_id,ma.artifact_id FROM message_artifacts ma JOIN chat_messages m ON m.id=ma.message_id WHERE m.thread_id=${threadId} ORDER BY ma.message_id,ma.artifact_id`),
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

async function createThread(page, nonce) {
  await page.getByRole("button", { name: "Threads", exact: true }).click();
  const drawer = page.locator('[data-slot="thread-drawer"]');
  await drawer.getByLabel("New thread title", { exact: true }).waitFor();
  const title = `Runbook ${nonce}`;
  await drawer.getByLabel("New thread title", { exact: true }).fill(title);
  await drawer.getByRole("button", { name: "Create thread", exact: true }).click();
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
  const firstPrompt = `Use shell.run once with cmd "sleep 3; printf '${firstMarker}'". After the tool returns, end with the exact marker ${firstMarker}.`;
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
  await capture("10-queued", context);

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
  const stable = await waitForStableDom(page);
  const settled = await capture("30-settled", context);
  assert.equal(settled.detail.messages.filter(message => message.author === "owner").length, 2);
  assert.equal(settled.detail.messages.filter(message => message.author === "agent").length, 2);
  assert.equal(settled.detail.turns.filter(turn => terminal(turn.state)).length, 2);
  assert.equal(settled.store.messages.length, 4);
  assert.equal(settled.store.turns.length, 2);
  const firstTool = toolPair(frames, first.turnId, /shell[._]run/);
  const secondTool = toolPair(frames, secondTurn.id, /shell[._]run/);
  for (const [turn, marker, tool] of [
    [firstTerminal, firstMarker, firstTool],
    [secondTerminal, secondMarker, secondTool],
  ]) {
    assert.equal(tool.done.event.ok, true);
    assert.match(tool.done.event.summary ?? "", new RegExp(marker));
    assert.match(agentReply(settled.detail, turn).body, new RegExp(marker));
  }
  const before = timelineProjection(stable);
  await page.reload({ waitUntil: "domcontentloaded" });
  await page.locator(`[data-message-id="${secondTerminal.agent_message_id}"]`).getByText(secondMarker, { exact: false }).waitFor();
  const reloaded = await capture("31-reloaded", context);
  const after = timelineProjection(reloaded.dom);
  assert.deepEqual(after, before);
  assert.deepEqual(reloaded.detail.activities, settled.detail.activities);
  return {
    markers: [firstMarker, secondMarker],
    turnIds: [first.turnId, secondTurn.id],
    orderedTimeline: after,
    liveTurnEvents: [first.turnId, secondTurn.id].map(turnId => ({ turnId, events: turnEvents(frames, turnId) })),
    durableActivities: reloaded.detail.activities,
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
  assert.match(successTool.done.event.summary ?? "", new RegExp(successMarker));
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
  await requireInlineTool(page, failureTool.started.event.id, "No such file or directory");

  const final = await capture("30-crosscheck", context);
  assert.equal(final.detail.messages.filter(message => message.author === "owner").length, 2);
  assert.equal(final.detail.messages.filter(message => message.author === "agent").length, 2);
  assert.equal(final.store.turns.length, 2);
  const durableCalls = final.detail.messages.flatMap(message => message.tool_calls ?? []);
  for (const pair of [successTool, failureTool]) {
    assert(durableCalls.some(call => call.id === pair.started.event.id && call.ok === pair.done.event.ok));
  }
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
  const reloaded = await capture("30-reloaded", context);
  const stored = reloaded.store.artifacts.find(artifact => artifact.id === upsert.id);
  assert.deepEqual(
    { title: stored?.title, kind: stored?.kind, mime: stored?.mime, filename: stored?.filename, content: stored?.content },
    { title, kind: "file", mime: "text/plain", filename, content },
  );
  return { turnId: turn.turnId, toolCallId: tool.started.event.id, artifactId: upsert.id, title, filename, content, downloadPath };
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
    initialModelCallBudget: scenario === "chat-chronology" || scenario === "tool-execution" ? 2 : 1,
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
    if (scenario === "artifact-creation") assert.equal(empty.store.artifacts.length, 0);

    result.detail = scenario === "chat-chronology"
      ? await runChat(context)
      : scenario === "tool-execution"
        ? await runTools(context)
        : await runArtifact(context);
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
      frames.map(row => JSON.stringify(row)).join("\n") + (frames.length ? "\n" : ""),
    );
    await writeFile(join(scenarioDir, "result.json"), `${JSON.stringify(result, null, 2)}\n`);
    await browser?.close();
    await stopProcess(host);
    logStream.end();
  }
  console.log(`${scenario}: ${result.objectiveStatus}${result.error ? ` — ${result.error}` : ""}`);
  return result;
}

const results = [];
for (const scenario of scenarios) results.push(await runScenario(scenario));
await writeFile(join(evidenceRoot, "summary.json"), `${JSON.stringify(results, null, 2)}\n`);
if (results.some(result => result.objectiveStatus !== "OBJECTIVE_PASS")) process.exitCode = 1;
