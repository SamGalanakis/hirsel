// Run only against an isolated host/data copy; creates and settles test threads.
// HIRSEL_THREAD_SMOKE_URL=http://127.0.0.1:PORT HIRSEL_THREAD_SMOKE_TOKEN=... node e2e/thread-smoke.mjs
import { mkdir, writeFile } from "node:fs/promises";
import { WebSocket } from "../app/node_modules/ws/wrapper.mjs";
import { chromium } from "../app/node_modules/playwright/index.mjs";
const url = process.env.HIRSEL_THREAD_SMOKE_URL;
if (!url || new URL(url).port === "3076") throw new Error("Set HIRSEL_THREAD_SMOKE_URL to an isolated test host, never live port 3076");
const artifacts = process.env.HIRSEL_THREAD_SMOKE_ARTIFACTS;
if (artifacts) await mkdir(artifacts, { recursive: true });
const evidence = [];
async function inventory(page) {
  await page.getByRole("button", { name: "Spaces and Tasks", exact: true }).click();
  await page.getByLabel("New space or task title", { exact: true }).waitFor();
}
async function chooseLifecycle(page, label) {
  await page.getByRole("button", { name: "Thread actions", exact: true }).click();
  await page.getByRole("menuitem", { name: label, exact: true }).click();
}
async function expectLifecycle(page, label) {
  await page.getByRole("button", { name: "Thread actions", exact: true }).click();
  await page.getByRole("menuitem", { name: label, exact: true }).waitFor();
  await page.keyboard.press("Escape");
}
async function overview(page) {
  await page.getByRole("button", { name: "Thread overview", exact: true }).click();
  await page.locator('[data-slot="thread-empty"]').waitFor();
  if (new URL(page.url()).pathname !== "/") throw new Error("Overview did not clear the explicit selection");
  if (await page.locator("textarea").count()) throw new Error("Overview has an implicit recipient");
}
const browser = await chromium.launch({ headless: true, ...(process.env.CHROMIUM_EXECUTABLE ? { executablePath: process.env.CHROMIUM_EXECUTABLE } : {}) });
try {
  for (const viewport of [{ width: 1440, height: 900 }, { width: 390, height: 844 }]) {
    const page = await browser.newPage({ viewport, hasTouch: viewport.width < 1024 });
    const frames = [];
    const errors = [];
    page.on("pageerror", error => errors.push(error.message));
    page.on("websocket", ws => {
      ws.on("framereceived", event => { try { frames.push(JSON.parse(event.payload.toString())); } catch { /* binary frames are not part of this protocol */ } });
    });
    await page.addInitScript(token => { if (window === window.top) localStorage.setItem("hirsel.token", token); }, process.env.HIRSEL_THREAD_SMOKE_TOKEN ?? "dev-token");
    await page.goto(url);
    if (process.env.HIRSEL_THREAD_SMOKE_EXPECT_ID) {
      const id = process.env.HIRSEL_THREAD_SMOKE_EXPECT_ID;
      await inventory(page);
      await page.locator(`[data-thread-row="${id}"]`).click();
      await page.locator(`[data-thread-id="${id}"]`).waitFor();
      await page.locator('[data-slot="thread-context"] h1').filter({ hasText: "buy-groceries" }).waitFor();
      if (artifacts) await page.screenshot({ path: `${artifacts}/imported-groceries-${viewport.width}.png`, fullPage: true });
    }
    const title = `Task smoke ${viewport.width} ${Date.now()}`;
    await inventory(page);
    await page.getByLabel("New space or task title").fill(title);
    await page.getByRole("button", { name: "New Task", exact: true }).click();
    await page.locator('[data-slot="thread-context"] h1').filter({ hasText: title }).waitFor();
    const path = new URL(page.url()).pathname + new URL(page.url()).search;
    const threadId = Number(path.split("/").at(-1));
    if (!/^\/t\/\d+$/.test(path)) throw new Error(`Thread create did not navigate: ${path}`);
    await page.locator("textarea").fill("This draft belongs to this thread");
    await overview(page);
    if (await page.locator("textarea").count()) throw new Error("Thread draft leaked into overview");
    await page.goto(`${url}${path}`);
    await page.locator('[data-slot="thread-context"] h1').filter({ hasText: title }).waitFor();
    if (await page.locator("textarea").inputValue() !== "This draft belongs to this thread") throw new Error("Thread draft was lost");
    const body = `Owned message ${viewport.width} ${Date.now()}`;
    await page.locator("textarea").fill(body);
    const sendTarget = await page.getByRole("button", { name: "Send", exact: true }).boundingBox();
    if (!sendTarget || sendTarget.width < 44 || sendTarget.height < 44) throw new Error("Send target is smaller than 44px");
    if (viewport.width < 1024) await page.getByRole("button", { name: "Send", exact: true }).click();
    else await page.locator("textarea").press("Enter");
    await page.getByRole("article", { name: "Hirsel", exact: true }).filter({ hasText: "scripted Agent mode" }).waitFor();
    const ownedMessages = frames.filter(frame => frame.type === "msg" && frame.message.thread_id === threadId).map(frame => frame.message);
    if (!ownedMessages.some(message => message.author === "owner" && message.body === body) || !ownedMessages.some(message => message.author === "agent")) throw new Error("Host did not emit both messages with correct Thread ownership");
    if (artifacts) await page.screenshot({ path: `${artifacts}/thread-conversation-${viewport.width}.png`, fullPage: true });
    await overview(page);
    if (await page.getByText(body, { exact: true }).count()) throw new Error("Owned message leaked into overview");
    await page.goto(`${url}${path}`);
    await page.getByText(body, { exact: true }).waitFor();
    await chooseLifecycle(page, "Mark task done");
    await expectLifecycle(page, "Reopen task");
    await page.reload();
    await expectLifecycle(page, "Reopen task");
    await chooseLifecycle(page, "Reopen task");
    await expectLifecycle(page, "Mark task done");
    const dimensions = await page.evaluate(() => ({ width: document.documentElement.clientWidth, scroll: document.documentElement.scrollWidth }));
    if (dimensions.scroll > dimensions.width) throw new Error("Horizontal overflow");
    if (errors.length) throw new Error(`Browser errors: ${errors.join("; ")}`);
    evidence.push({ viewport, threadId, ownedMessages, browserErrors: errors });
    await page.close();
  }
  if (process.env.HIRSEL_THREAD_SMOKE_ADAPTIVE === "1") {
    const token = process.env.HIRSEL_THREAD_SMOKE_TOKEN ?? "dev-token";
    const response = await fetch(`${url}/debug/seed-adaptive-thread`, { method: "POST", headers: { Authorization: `Bearer ${token}` } });
    if (!response.ok) throw new Error(`Adaptive fixture failed: ${response.status} ${await response.text()}`);
    const thread = await response.json();
    const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
    const sent = [];
    page.on("websocket", ws => ws.on("framesent", event => sent.push(JSON.parse(event.payload.toString()))));
    await page.addInitScript(value => { if (window === window.top) localStorage.setItem("hirsel.token", value); }, token);
    await page.goto(url);
    await page.getByRole("button", {name:"Spaces and Tasks",exact:true}).click();
    await page.locator(`[data-thread-row="${thread.id}"]`).click();
    await page.getByLabel("Confirmation").fill("ready");
    await page.getByRole("button", { name: "Continue", exact: true }).click();
    await page.getByRole("heading", { name: "Adaptive host proof advanced", exact: true }).waitFor();
    await expectLifecycle(page, "Mark task done");
    const action = sent.find(frame => frame.type === "thread_action" && frame.action === "advance");
    if (!action?.expected_revision) throw new Error("Generated action did not carry the displayed revision");
    const request = (frame, expected) => new Promise((resolve, reject) => {
      const ws = new WebSocket(`${url.replace(/^http/, "ws")}/ws`);
      const timer = setTimeout(() => { ws.close(); reject(new Error(`Missing ${expected}`)); }, 5000);
      ws.on("error", reject);
      ws.on("open", () => ws.send(JSON.stringify({ type: "hello", auth: { static_token: token } })));
      ws.on("message", raw => {
        const message = JSON.parse(raw.toString());
        if (message.type === "hello_ok") ws.send(JSON.stringify(frame));
        else if (message.type === expected) { clearTimeout(timer); ws.close(); resolve(message); }
      });
    });
    const stale = await request(action, "error");
    if (!/revision|changed|stale/i.test(stale.detail)) throw new Error(`Unexpected stale action failure: ${stale.detail}`);
    const reopened = await request({ type: "open_thread", client_id: "smoke-adaptive-open", thread_id: thread.id }, "thread_opened");
    if (reopened.detail.thread.id !== thread.id || reopened.detail.thread.settled_at !== null) throw new Error("Continuation changed identity or settled work");
    if (reopened.detail.messages.filter(message => message.author === "owner").length !== 1) throw new Error("Stale action appended a duplicate owner message");
    if (artifacts) await page.screenshot({ path: `${artifacts}/adaptive-thread-1440.png`, fullPage: true });
    evidence.push({ adaptiveThreadId: thread.id, submittedRevision: action.expected_revision, currentRevision: reopened.detail.thread.revision, staleError: stale.detail });
    await page.close();
  }
  if (artifacts) await writeFile(`${artifacts}/thread-smoke-evidence.json`, JSON.stringify(evidence, null, 2));
  console.log("Task create, focus, draft isolation, done/reopen and reconnect smoke passed on desktop and phone.");
} finally { await browser.close(); }
