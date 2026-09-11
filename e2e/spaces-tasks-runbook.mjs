import assert from "node:assert/strict";
import { execFileSync, spawn } from "node:child_process";
import { createWriteStream } from "node:fs";
import { mkdir, writeFile } from "node:fs/promises";
import { createServer } from "node:net";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { chromium } from "../app/node_modules/playwright/index.mjs";
import { WebSocket } from "../app/node_modules/ws/wrapper.mjs";

const repo = fileURLToPath(new URL("..", import.meta.url));
const runId = `${new Date().toISOString().replaceAll(/[:.]/g, "-")}-${process.pid}`;
const evidenceDir = process.env.HIRSEL_SPACES_EVIDENCE ?? join("/tmp", `hirsel-spaces-tasks-${runId}`);
const dataDir = join(evidenceDir, "state");
await mkdir(dataDir, { recursive: true });
console.log(`Spaces and Tasks evidence: ${evidenceDir}`);

function git(...args) {
  return execFileSync("git", args, { cwd: repo, encoding: "utf8" }).trim();
}

function cargoTargetDirectory() {
  return JSON.parse(execFileSync(
    "cargo",
    ["metadata", "--no-deps", "--format-version", "1"],
    { cwd: repo, encoding: "utf8" },
  )).target_directory;
}

function sqliteJson(sql) {
  const output = execFileSync("sqlite3", ["-json", join(dataDir, "hirsel.sqlite"), sql], { encoding: "utf8" }).trim();
  return output ? JSON.parse(output) : [];
}

function sqliteRun(sql) {
  execFileSync("sqlite3", [join(dataDir, "hirsel.sqlite"), sql], { encoding: "utf8" });
}

function sqlText(value) {
  return `'${value.replaceAll("'", "''")}'`;
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

async function poll(label, predicate, timeoutMs = 15_000) {
  const deadline = Date.now() + timeoutMs;
  let lastError;
  while (Date.now() < deadline) {
    try {
      const value = await predicate();
      if (value) return value;
    } catch (error) {
      lastError = error;
    }
    await new Promise(resolve => setTimeout(resolve, 150));
  }
  throw new Error(`${label} timed out${lastError ? `: ${lastError.message}` : ""}`);
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

function parseFrame(payload) {
  try { return JSON.parse(payload.toString()); } catch { return null; }
}

function waitForFrame(frames, offset, label, predicate) {
  return poll(label, () => frames.slice(offset).find(frame => predicate(frame)));
}

async function socketRequest(url, token, frame, expected) {
  return new Promise((resolve, reject) => {
    const socket = new WebSocket(`${url.replace(/^http/, "ws")}/ws`);
    const timer = setTimeout(() => {
      socket.close();
      reject(new Error(`${expected} timed out`));
    }, 10_000);
    socket.on("error", reject);
    socket.on("open", () => socket.send(JSON.stringify({ type: "hello", auth: { static_token: token } })));
    socket.on("message", raw => {
      const message = parseFrame(raw);
      if (message?.type === "hello_ok") socket.send(JSON.stringify(frame));
      else if (message?.type === expected) {
        clearTimeout(timer);
        socket.close();
        resolve(message);
      } else if (message?.type === "error") {
        clearTimeout(timer);
        socket.close();
        reject(new Error(message.detail));
      }
    });
  });
}

async function helloSnapshot(url, token) {
  return new Promise((resolve, reject) => {
    const socket = new WebSocket(`${url.replace(/^http/, "ws")}/ws`);
    const timer = setTimeout(() => {
      socket.close();
      reject(new Error("hello_ok timed out"));
    }, 10_000);
    socket.on("error", reject);
    socket.on("open", () => socket.send(JSON.stringify({ type: "hello", auth: { static_token: token } })));
    socket.on("message", raw => {
      const message = parseFrame(raw);
      if (message?.type === "hello_ok") {
        clearTimeout(timer);
        socket.close();
        resolve(message);
      } else if (message?.type === "error") {
        clearTimeout(timer);
        socket.close();
        reject(new Error(message.detail));
      }
    });
  });
}

async function openThread(url, token, threadId) {
  return socketRequest(url, token, { type: "open_thread", client_id: `runbook-open-${crypto.randomUUID()}`, thread_id: threadId }, "thread_opened");
}

function storeSnapshot() {
  return {
    schemaVersion: sqliteJson("SELECT user_version AS version FROM pragma_user_version")[0]?.version,
    threads: sqliteJson("SELECT id,kind,parent_thread_id,pinned_at,title,instrument,attention,settled_at,read,revision FROM threads ORDER BY id"),
    turns: sqliteJson("SELECT id,thread_id,requester_thread_id,owner_message_id,agent_message_id,state,started_at,finished_at FROM thread_turns ORDER BY id"),
    messages: sqliteJson("SELECT id,thread_id,author,body,ts FROM chat_messages ORDER BY id"),
  };
}

function threadRecord(snapshot, id) {
  const thread = snapshot.threads.find(candidate => candidate.id === id);
  assert(thread, `SQLite has no Thread ${id}`);
  return thread;
}

async function domSnapshot(page) {
  return page.evaluate(() => {
    const visible = element => Boolean(element && (element.offsetWidth || element.offsetHeight || element.getClientRects().length));
    const current = document.querySelector("main[data-thread-id]");
    return {
      url: location.href,
      current: current ? {
        id: Number(current.getAttribute("data-thread-id")),
        title: current.querySelector('[data-slot="thread-context"] h1')?.textContent?.trim() ?? null,
        kind: current.querySelector('[data-slot="thread-context"] [data-thread-kind]')?.getAttribute("data-thread-kind") ?? null,
        text: current.textContent?.trim() ?? "",
      } : null,
      rows: [...document.querySelectorAll("[data-thread-row]")].filter(visible).map(row => ({
        id: Number(row.getAttribute("data-thread-row")),
        text: row.textContent?.trim() ?? "",
        current: row.getAttribute("aria-current"),
      })),
      alerts: [...document.querySelectorAll('[role="alert"]')].filter(visible).map(row => row.textContent?.trim() ?? ""),
      buttons: [...document.querySelectorAll("button")].filter(visible).map(button => button.textContent?.trim() || button.getAttribute("aria-label") || ""),
      pageOverflow: document.documentElement.scrollWidth > document.documentElement.clientWidth,
      viewport: { width: innerWidth, height: innerHeight, devicePixelRatio },
      avatars: [...document.querySelectorAll("[data-thread-kind]")].filter(visible).map(avatar => ({
        kind: avatar.getAttribute("data-thread-kind"),
        radius: getComputedStyle(avatar).borderRadius,
      })),
    };
  });
}

async function capture(page, url, token, label, focusId) {
  const [dom, inventory, detail] = await Promise.all([
    domSnapshot(page),
    helloSnapshot(url, token),
    openThread(url, token, focusId),
  ]);
  const store = storeSnapshot();
  await Promise.all([
    page.screenshot({ path: join(evidenceDir, `${label}.png`), fullPage: true }),
    writeFile(join(evidenceDir, `${label}-dom.json`), `${JSON.stringify(dom, null, 2)}\n`),
    writeFile(join(evidenceDir, `${label}-inventory.json`), `${JSON.stringify(inventory, null, 2)}\n`),
    writeFile(join(evidenceDir, `${label}-thread.json`), `${JSON.stringify(detail, null, 2)}\n`),
    writeFile(join(evidenceDir, `${label}-store.json`), `${JSON.stringify(store, null, 2)}\n`),
  ]);
  assert.equal(dom.current?.id, focusId, `${label}: wrong selected Thread`);
  assert.equal(dom.pageOverflow, false, `${label}: page overflows horizontally`);
  assert.deepEqual(detail.detail.thread, inventory.threads.find(thread => thread.id === focusId), `${label}: open_thread and inventory disagree`);
  const stored = threadRecord(store, focusId);
  for (const field of ["id", "kind", "parent_thread_id", "title", "attention", "settled_at", "revision"]) {
    assert.deepEqual(detail.detail.thread[field], stored[field], `${label}: ${field} differs between wire and SQLite`);
  }
  return { dom, inventory, detail: detail.detail, store };
}

async function ensureDrawer(page) {
  const drawer = page.locator('[data-slot="thread-drawer"]');
  if (!await drawer.isVisible()) await page.getByRole("button", { name: "Spaces and Tasks", exact: true }).click();
  await drawer.waitFor({ state: "visible" });
  return drawer;
}

async function createItem(page, frames, title, kind, parent) {
  if (parent === null) {
    await page.getByRole("button", { name: "New Space or Task", exact: true }).click();
  } else {
    await chooseAction(page, parent.kind === "task" ? "New child task" : "New child");
  }
  const drawer = await ensureDrawer(page);
  await drawer.getByLabel("New space or task title", { exact: true }).fill(title);
  if (parent?.kind === "task") {
    assert.equal(await drawer.getByRole("button", { name: "New Space", exact: true }).count(), 0, "Task child form offered New Space");
  }
  const offset = frames.length;
  const sentOffset = sentFrames.length;
  await drawer.getByRole("button", { name: kind === "space" ? "New Space" : "New Task", exact: true }).click();
  const created = await waitForFrame(frames, offset, `${title} creation`, frame => frame.type === "thread_created" && frame.thread?.title === title);
  const request = sentFrames.slice(sentOffset).find(frame => frame.type === "create_thread" && frame.title === title);
  assert(request, `${title} did not use the create_thread wire action`);
  assert.equal(request.kind, kind);
  assert.equal(request.parent_thread_id, parent?.id ?? null);
  assert.equal(created.thread.kind, kind);
  assert.equal(created.thread.parent_thread_id, parent?.id ?? null);
  await page.locator(`main[data-thread-id="${created.thread.id}"]`).waitFor();
  return created.thread;
}

async function selectThread(page, id) {
  const drawer = await ensureDrawer(page);
  const row = drawer.locator(`[data-thread-row="${id}"]`);
  await row.waitFor({ state: "visible" });
  await row.click();
  await page.locator(`main[data-thread-id="${id}"]`).waitFor();
}

async function chooseAction(page, label) {
  await page.getByRole("button", { name: "Thread actions", exact: true }).click();
  await page.getByRole("menuitem", { name: label, exact: true }).click();
}

async function mutate(page, frames, id, label, predicate) {
  const offset = frames.length;
  const sentOffset = sentFrames.length;
  await chooseAction(page, label);
  const response = await waitForFrame(frames, offset, `${label} response`, frame => frame.type === "thread_upsert" && frame.thread?.id === id && predicate(frame.thread));
  const request = sentFrames.slice(sentOffset).find(frame => frame.type === "thread_action" && frame.thread_id === id);
  assert(request, `${label} did not use the thread_action wire action`);
  if (request.action === "set_kind") {
    assert(["space", "task"].includes(request.data?.kind), `${label} omitted the target kind`);
    assert(Number.isInteger(request.expected_revision), `${label} omitted expected_revision`);
  }
  return response;
}

async function invalidMutation(page, frames, url, token, id, label, detailPattern, store) {
  const beforeDetail = (await openThread(url, token, id)).detail.thread;
  const beforeStore = threadRecord(storeSnapshot(), id);
  const beforeCount = store.threads.length;
  const offset = frames.length;
  await chooseAction(page, label);
  const failure = await waitForFrame(frames, offset, `${label} rejection`, frame => frame.type === "error" && detailPattern.test(frame.detail ?? ""));
  const alert = page.locator(`main[data-thread-id="${id}"]`).getByRole("alert");
  await alert.waitFor({ state: "visible" });
  await alert.locator("summary").click();
  await alert.getByText(detailPattern).waitFor();
  const afterDetail = (await openThread(url, token, id)).detail.thread;
  const afterStore = threadRecord(storeSnapshot(), id);
  assert.deepEqual(afterDetail, beforeDetail, `${label} changed the authoritative Thread`);
  assert.deepEqual(afterStore, beforeStore, `${label} changed the SQLite Thread`);
  assert.equal(storeSnapshot().threads.length, beforeCount, `${label} partially created a Thread`);
  return failure;
}

function assertKindPresentation(dom, kind) {
  assert.equal(dom.current?.kind, kind);
  const avatar = dom.avatars.find(candidate => candidate.kind === kind);
  assert(avatar, `No visible ${kind} avatar`);
  const radius = Number.parseFloat(avatar.radius);
  if (kind === "space") assert(radius < 20, `Space avatar is not visibly squared (${avatar.radius})`);
  else assert(radius > 100, `Task avatar is not visibly round (${avatar.radius})`);
}

const token = `spaces-runbook-${crypto.randomUUID()}`;
const port = await unusedPort();
const url = `http://127.0.0.1:${port}`;
const hostBinary = process.env.HIRSEL_SPACES_HOST_BIN ?? join(cargoTargetDirectory(), "debug", "hirsel-host");
const log = createWriteStream(join(evidenceDir, "host.log"));
const host = spawn(hostBinary, [], {
  cwd: repo,
  detached: true,
  stdio: ["ignore", "pipe", "pipe"],
  env: {
    ...process.env,
    HIRSEL_TOKEN: token,
    HIRSEL_AGENT: "scripted",
    HIRSEL_DRIVER: "fake",
    HIRSEL_PROVIDER: "anthropic",
    HIRSEL_DEBUG: "1",
    HIRSEL_IROH: "0",
    HIRSEL_DATA_DIR: dataDir,
    HIRSEL_CONFIG: join(dataDir, "hirsel.toml"),
    HIRSEL_TEMPLATES_DIR: join(repo, "templates"),
    HIRSEL_APP_DIR: join(repo, "app", "dist"),
    HIRSEL_LISTEN: `127.0.0.1:${port}`,
  },
});
host.stdout.pipe(log, { end: false });
host.stderr.pipe(log, { end: false });

let browser;
const browserErrors = [];
const expectedProtocolErrors = [];
const frames = [];
const sentFrames = [];
const checkpoints = {};
const failures = [];

try {
  await poll("isolated Host readiness", async () => {
    if (host.exitCode !== null || host.signalCode !== null) throw new Error(`Host exited (${host.exitCode ?? host.signalCode})`);
    return (await fetch(`${url}/readyz`)).ok;
  }, 30_000);
  assert.notEqual(new URL(url).port, "3076");
  browser = await chromium.launch({
    headless: true,
    executablePath: process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH
      ?? "/home/sam/.cache/ms-playwright/chromium_headless_shell-1234/chrome-headless-shell-linux64/chrome-headless-shell",
  });
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  page.on("pageerror", error => browserErrors.push({ type: "pageerror", message: error.message }));
  page.on("console", message => {
    if (message.type() !== "error") return;
    if (message.text().startsWith("hirsel protocol error:")) expectedProtocolErrors.push(message.text());
    else browserErrors.push({ type: "console", message: message.text() });
  });
  page.on("websocket", socket => {
    socket.on("framereceived", event => {
      const frame = parseFrame(event.payload);
      if (frame) frames.push(frame);
    });
    socket.on("framesent", event => {
      const frame = parseFrame(event.payload);
      if (frame && frame.type !== "hello") sentFrames.push(frame);
    });
  });
  await page.addInitScript(value => {
    if (window !== window.top) return;
    localStorage.setItem("hirsel.token", value);
    localStorage.setItem("hirsel.theme", "dark");
    localStorage.removeItem("hirsel.thread-navigation.desktop");
  }, token);
  await page.goto(url, { waitUntil: "domcontentloaded" });
  await page.getByText("Start with a Space or Task", { exact: true }).waitFor();

  const rootSpace = await createItem(page, frames, "Product planning", "space", null);
  const childSpace = await createItem(page, frames, "User research", "space", rootSpace);
  await selectThread(page, rootSpace.id);
  const childTask = await createItem(page, frames, "Draft brief", "task", rootSpace);
  const rootTask = await createItem(page, frames, "Ship release", "task", null);
  const nestedTask = await createItem(page, frames, "Review release", "task", rootTask);
  assert.deepEqual(
    storeSnapshot().threads.map(thread => [thread.title, thread.kind, thread.parent_thread_id]),
    [
      [rootSpace.title, "space", null],
      [childSpace.title, "space", rootSpace.id],
      [childTask.title, "task", rootSpace.id],
      [rootTask.title, "task", null],
      [nestedTask.title, "task", rootTask.id],
    ],
  );

  await selectThread(page, rootTask.id);
  await mutate(page, frames, rootTask.id, "Mark task done", thread => thread.kind === "task" && thread.settled_at !== null);
  await page.reload({ waitUntil: "domcontentloaded" });
  await page.locator(`main[data-thread-id="${rootTask.id}"]`).waitFor();
  await page.getByRole("button", { name: "Thread actions", exact: true }).click();
  await page.getByRole("menuitem", { name: "Reopen task", exact: true }).waitFor();
  await page.keyboard.press("Escape");
  await mutate(page, frames, rootTask.id, "Reopen task", thread => thread.kind === "task" && thread.settled_at === null);

  const conversionId = rootTask.id;
  await mutate(page, frames, conversionId, "Change to Space", thread => thread.kind === "space");
  let converted = await capture(page, url, token, "20-valid-task-to-space", conversionId);
  assert.equal(converted.detail.messages.length, 0, "conversion changed the conversation");
  assert.equal(converted.detail.thread.id, conversionId, "conversion changed identity");
  await mutate(page, frames, conversionId, "Change to Task", thread => thread.kind === "task");
  converted = await capture(page, url, token, "21-valid-space-to-task", conversionId);
  assert.equal(converted.detail.messages.length, 0, "round-trip conversion changed the conversation");
  assert.equal(converted.detail.thread.id, conversionId, "round-trip conversion changed identity");

  await selectThread(page, rootSpace.id);
  failures.push(await invalidMutation(page, frames, url, token, rootSpace.id, "Change to Task", /cannot contain Spaces|Space child|contains.*Space/i, storeSnapshot()));
  checkpoints.invalidSpace = await capture(page, url, token, "30-invalid-space-to-task", rootSpace.id);
  await page.locator(`main[data-thread-id="${rootSpace.id}"]`).getByRole("alert").getByRole("button", { name: "Dismiss", exact: true }).click();

  const invalidTaskDrawer = await ensureDrawer(page);
  await invalidTaskDrawer.getByRole("button", { name: `Expand ${rootTask.title}`, exact: true }).click();
  await selectThread(page, nestedTask.id);
  failures.push(await invalidMutation(page, frames, url, token, nestedTask.id, "Change to Space", /Task parent|under a Task|parent.*Task/i, storeSnapshot()));
  checkpoints.invalidTask = await capture(page, url, token, "31-invalid-task-to-space", nestedTask.id);
  await page.locator(`main[data-thread-id="${nestedTask.id}"]`).getByRole("alert").getByRole("button", { name: "Dismiss", exact: true }).click();

  await selectThread(page, rootTask.id);
  await mutate(page, frames, rootTask.id, "Pin thread", thread => thread.pinned_at !== null);
  await page.reload({ waitUntil: "domcontentloaded" });
  await page.locator(`main[data-thread-id="${rootTask.id}"]`).waitFor();
  const rootTaskDrawer = await ensureDrawer(page);
  assert.equal(await rootTaskDrawer.locator(`[data-thread-row="${rootTask.id}"]`).count(), 1, "Pinned root is duplicated");
  await rootTaskDrawer.locator(`[data-thread-row="${rootTask.id}"]`).getByText("Pinned", { exact: true }).waitFor();
  await page.getByText(/Child threads · 1/).waitFor();
  checkpoints.taskDesktop = await capture(page, url, token, "40-task-desktop", rootTask.id);
  assertKindPresentation(checkpoints.taskDesktop.dom, "task");

  await page.setViewportSize({ width: 390, height: 844 });
  const taskNarrowDrawer = await ensureDrawer(page);
  await taskNarrowDrawer.getByRole("button", { name: `Expand ${rootTask.title}`, exact: true }).click();
  checkpoints.taskNarrow = await capture(page, url, token, "41-task-narrow", rootTask.id);
  assertKindPresentation(checkpoints.taskNarrow.dom, "task");

  // Fixture-only layer: add visible operational state and two generated actions
  // directly to this disposable store. Core create/action/conversion claims above
  // were already completed exclusively through shipping controls.
  const instrument = JSON.stringify({
    type: "card",
    children: [
      { type: "heading", text: "Space coordination fixture", level: 2 },
      { type: "text", text: "Queued and needs-input state remains visible on an ongoing Space." },
      { type: "submit", action: "advance", label: "Continue fixture", settles: false },
      { type: "submit", action: "complete", label: "Complete fixture", settles: true },
    ],
  });
  sqliteRun(`
    PRAGMA foreign_keys=ON;
    BEGIN;
    UPDATE threads
      SET attention='needs_owner', instrument=${sqlText(instrument)}, revision=revision+1
      WHERE id=${rootSpace.id};
    INSERT INTO thread_turns(thread_id,state,started_at)
      VALUES(${rootSpace.id},'completed','2026-09-10T11:00:00Z');
    UPDATE thread_turns SET finished_at='2026-09-10T11:01:00Z'
      WHERE id=last_insert_rowid();
    COMMIT;
  `);
  await page.goto(`${url}/t/${rootSpace.id}${new URL(page.url()).search}`, { waitUntil: "domcontentloaded" });
  await page.locator(`main[data-thread-id="${rootSpace.id}"]`).waitFor();
  await page.getByRole("button", { name: "Continue fixture", exact: true }).waitFor();
  assert.equal(await page.getByRole("button", { name: "Complete fixture", exact: true }).count(), 0, "Space exposed a generated completion action");
  await page.getByRole("button", { name: "Thread actions", exact: true }).click();
  assert.equal(await page.getByRole("menuitem", { name: /Mark task done|Reopen task/ }).count(), 0, "Space exposed an Owner completion action");
  await page.keyboard.press("Escape");
  const fixtureDrawer = await ensureDrawer(page);
  const spaceRow = fixtureDrawer.locator(`[data-thread-row="${rootSpace.id}"]`);
  await spaceRow.getByText("Needs you", { exact: true }).waitFor();
  assert.equal(await spaceRow.getByText("Turn finished", { exact: false }).count(), 0, "Space exposed an idle success badge");
  checkpoints.spaceCompletedSuppressed = await capture(page, url, token, "49-space-completed-suppressed", rootSpace.id);

  sqliteRun(`
    PRAGMA foreign_keys=ON;
    INSERT INTO thread_turns(thread_id,state,started_at)
      VALUES(${rootSpace.id},'queued','2026-09-10T12:00:00Z');
  `);
  await page.reload({ waitUntil: "domcontentloaded" });
  await page.locator(`main[data-thread-id="${rootSpace.id}"]`).waitFor();
  await ensureDrawer(page);
  await page.locator(`[data-thread-row="${rootSpace.id}"]`).getByText("Queued", { exact: false }).waitFor();

  await page.setViewportSize({ width: 1440, height: 900 });
  await ensureDrawer(page);
  checkpoints.spaceDesktop = await capture(page, url, token, "50-space-fixture-desktop", rootSpace.id);
  assertKindPresentation(checkpoints.spaceDesktop.dom, "space");
  await page.setViewportSize({ width: 390, height: 844 });
  const spaceNarrowDrawer = await ensureDrawer(page);
  await spaceNarrowDrawer.getByRole("button", { name: `Expand ${rootSpace.title}`, exact: true }).click();
  checkpoints.spaceNarrow = await capture(page, url, token, "51-space-fixture-narrow", rootSpace.id);
  assertKindPresentation(checkpoints.spaceNarrow.dom, "space");

  assert.equal(storeSnapshot().schemaVersion, 6, "runbook did not use durable schema 6");
  assert.deepEqual(
    expectedProtocolErrors,
    failures.map(frame => `hirsel protocol error: ${frame.detail}`),
    "browser protocol errors differed from the two asserted conversion rejections",
  );
  assert.deepEqual(browserErrors, [], `browser errors: ${JSON.stringify(browserErrors)}`);
  await writeFile(join(evidenceDir, "frames.json"), `${JSON.stringify({ received: frames, sent: sentFrames }, null, 2)}\n`);
  const result = {
    source: { head: git("rev-parse", "HEAD"), dirty: git("status", "--short") },
    service: "scripted/fake",
    providerCalls: 0,
    fixture: {
      used: true,
      scope: "After core UI actions, SQLite seeded only Space attention, one completed turn, one inert queued turn, and generated Continue/Complete controls for suppression presentation.",
    },
    port,
    ids: { rootSpace: rootSpace.id, childSpace: childSpace.id, childTask: childTask.id, rootTask: rootTask.id, nestedTask: nestedTask.id },
    rejected: failures.map(frame => frame.detail),
    expectedProtocolErrors,
    screenshots: ["20-valid-task-to-space.png", "21-valid-space-to-task.png", "30-invalid-space-to-task.png", "31-invalid-task-to-space.png", "40-task-desktop.png", "41-task-narrow.png", "49-space-completed-suppressed.png", "50-space-fixture-desktop.png", "51-space-fixture-narrow.png"],
    browserErrors,
    scorecardStatus: "NOT_JUDGED",
    objectiveStatus: "OBJECTIVE_PASS",
  };
  await writeFile(join(evidenceDir, "result.json"), `${JSON.stringify(result, null, 2)}\n`);
  console.log(JSON.stringify({ objectiveStatus: result.objectiveStatus, scorecardStatus: result.scorecardStatus, evidenceDir, ids: result.ids }));
} catch (error) {
  await writeFile(join(evidenceDir, "failure.json"), `${JSON.stringify({ message: error.message, stack: error.stack, browserErrors }, null, 2)}\n`);
  throw error;
} finally {
  if (browser) await browser.close();
  await stopProcess(host);
  log.end();
}
