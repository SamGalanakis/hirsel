// Task headlines and deterministic parent rollups against an isolated scripted Host.
import assert from "node:assert/strict";
import { mkdir, writeFile } from "node:fs/promises";

import { isolatedUrl, launchBrowser, poll, request } from "./lib/harness.mjs";

const url = isolatedUrl(process.env.HIRSEL_TASK_STATE_URL, "HIRSEL_TASK_STATE_URL");
const token = process.env.HIRSEL_TASK_STATE_TOKEN ?? "dev-token";
const evidenceDir = process.env.HIRSEL_THREAD_SMOKE_ARTIFACTS;
if (evidenceDir) await mkdir(evidenceDir, { recursive: true });

function received(frames, predicate) {
  return frames.findLast(row => row.direction === "received" && predicate(row.frame))?.frame;
}

const browser = await launchBrowser();
try {
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  const frames = [];
  const browserErrors = [];
  page.on("pageerror", error => browserErrors.push(error.message));
  page.on("websocket", socket => socket.on("framereceived", event => {
    try { frames.push({ direction: "received", frame: JSON.parse(event.payload.toString()) }); }
    catch { /* Binary frames are outside the protocol. */ }
  }));
  await page.addInitScript(value => {
    if (window === window.top) localStorage.setItem("hirsel.token", value);
  }, token);
  await page.goto(url, { waitUntil: "domcontentloaded" });
  const hello = await poll("Task headline hello", () => received(frames, frame => frame.type === "hello_ok"), 10_000);
  await poll("Task headline Home", () => hello.threads.some(thread =>
    thread.title === "Home" && thread.kind === "space" && thread.parent_thread_id === null
  ) || Boolean(received(frames, frame =>
    (frame.type === "thread_created" || frame.type === "thread_upsert")
      && frame.thread?.title === "Home"
      && frame.thread.kind === "space"
      && frame.thread.parent_thread_id === null
  )), 10_000);

  const suffix = crypto.randomUUID().slice(0, 8);
  const spaceTitle = `Headline space ${suffix}`;
  const space = (await request({
    url,
    token,
    frame: { type: "create_thread", client_id: crypto.randomUUID(), history_id: hello.history_id, title: spaceTitle, kind: "space", parent_thread_id: null },
    expected: "thread_created",
  })).thread;
  const taskTitle = `Headline task ${suffix}`;
  const task = (await request({
    url,
    token,
    frame: { type: "create_thread", client_id: crypto.randomUUID(), history_id: hello.history_id, title: taskTitle, kind: "task", parent_thread_id: space.id },
    expected: "thread_created",
  })).thread;
  await poll("initial deterministic rollup", () => received(frames, frame =>
    frame.type === "thread_upsert"
      && frame.thread.id === space.id
      && frame.thread.headline === `1 child · #${task.id} idle`
  ), 10_000);

  const drawer = page.getByRole("button", { name: "Spaces and Tasks", exact: true });
  if (await drawer.getAttribute("aria-expanded") === "false") await drawer.click();
  await page.locator(`[data-thread-row="${space.id}"]`).click();
  await page.locator(`main[data-thread-id="${space.id}"]`).waitFor();
  const offset = frames.length;
  const body = `__hirsel_task_headline__:${task.id}`;
  const composer = page.getByRole("textbox", { name: `Message Space chat ${spaceTitle}`, exact: true });
  await composer.fill(body);
  await page.getByRole("button", { name: "Send", exact: true }).click();
  const source = await poll("headline source turn", () => received(frames.slice(offset), frame =>
    frame.type === "thread_turn" && frame.turn.thread_id === space.id
  )?.turn, 10_000);
  await poll("headline source completion", () => received(frames.slice(offset), frame =>
    frame.type === "thread_turn" && frame.turn.id === source.id && frame.turn.state === "completed"
  ), 15_000);
  const updated = await poll("Task headline upsert", () => received(frames.slice(offset), frame =>
    frame.type === "thread_upsert"
      && frame.thread.id === task.id
      && frame.thread.headline === "Release evidence ready"
  )?.thread, 10_000);
  assert.equal(updated.previous_headline, "Ready");
  assert.equal(updated.headline_revision, task.headline_revision + 1);
  assert.equal(updated.own_headline, "Release evidence ready");

  if (await drawer.getAttribute("aria-expanded") === "false") await drawer.click();
  await page.locator(`[data-thread-row="${task.id}"]`).click();
  const main = page.locator(`main[data-thread-id="${task.id}"]`);
  await main.waitFor();
  await main.locator('[data-slot="task-headline"]').getByText("Release evidence ready", { exact: true }).waitFor();
  assert.equal(await main.locator('[data-slot="task-state"]').count(), 0);

  const reloadOffset = frames.length;
  await page.reload({ waitUntil: "domcontentloaded" });
  await poll("Task headline reload", () => received(frames.slice(reloadOffset), frame =>
    frame.type === "hello_ok" && frame.history_id === hello.history_id
  ), 10_000);
  await page.locator(`main[data-thread-id="${task.id}"] [data-slot="task-headline"]`).getByText("Release evidence ready", { exact: true }).waitFor();
  assert.deepEqual(browserErrors, []);

  if (evidenceDir) {
    await page.screenshot({ path: `${evidenceDir}/task-headline.png`, fullPage: true });
    await writeFile(`${evidenceDir}/task-headline.json`, `${JSON.stringify({
      historyId: hello.history_id,
      taskId: task.id,
      headlineRevision: updated.headline_revision,
      sourceTurnId: source.id,
      browserErrors,
    }, null, 2)}\n`);
  }
  console.log("Task headline, deterministic rollup and reload passed without wide Task state.");
} finally {
  await browser.close();
}
