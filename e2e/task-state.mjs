// Durable Task state, artifact revisions and deterministic parent rollups.
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
  page.on("websocket", socket => {
    socket.on("framesent", event => {
      try { frames.push({ direction: "sent", frame: JSON.parse(event.payload.toString()) }); } catch { /* protocol is JSON */ }
    });
    socket.on("framereceived", event => {
      try { frames.push({ direction: "received", frame: JSON.parse(event.payload.toString()) }); } catch { /* protocol is JSON */ }
    });
  });
  await page.addInitScript(value => { if (window === window.top) localStorage.setItem("hirsel.token", value); }, token);
  await page.goto(url, { waitUntil: "domcontentloaded" });
  const hello = await poll("Task-state hello", () => received(frames, frame => frame.type === "hello_ok"), 10_000);
  const home = await poll("Task-state Home", () => received(frames, frame =>
    (frame.type === "thread_created" || frame.type === "thread_upsert")
      && frame.thread?.title === "Home"
  )?.thread, 10_000);
  await page.locator(`main[data-thread-id="${home.id}"]`).waitFor();

  const suffix = crypto.randomUUID().slice(0, 8);
  const spaceTitle = `Material state space ${suffix}`;
  const space = (await request({
    url,
    token,
    frame: {
      type: "create_thread",
      client_id: crypto.randomUUID(),
      history_id: hello.history_id,
      title: spaceTitle,
      kind: "space",
      parent_thread_id: home.id,
    },
    expected: "thread_created",
  })).thread;
  const taskTitle = `Material state ${suffix}`;
  const created = await request({
    url,
    token,
    frame: {
      type: "create_thread",
      client_id: crypto.randomUUID(),
      history_id: hello.history_id,
      title: taskTitle,
      kind: "task",
      parent_thread_id: space.id,
    },
    expected: "thread_created",
  });
  const task = created.thread;
  assert.equal(task.state.revision, 1);
  await poll("initial deterministic rollup", () => received(frames, frame =>
    frame.type === "thread_upsert"
      && frame.thread.id === space.id
      && frame.thread.state.headline === `1 child · #${task.id} idle`
  )?.thread, 10_000);

  const drawer = page.getByRole("button", { name: "Spaces and Tasks", exact: true });
  if (await drawer.getAttribute("aria-expanded") === "false") await drawer.click();
  await page.locator(`[data-thread-row="${space.id}"]`).click();
  const offset = frames.length;
  const composer = page.getByRole("textbox", { name: `Message Space chat ${spaceTitle}`, exact: true });
  await composer.fill(`__hirsel_task_state__:${task.id}:${task.state.revision}`);
  await page.getByRole("button", { name: "Send", exact: true }).click();
  const source = await poll("Task-state source turn", () => received(frames.slice(offset), frame =>
    frame.type === "thread_turn" && frame.turn.thread_id === space.id
  )?.turn, 10_000);
  await poll("Task-state source completion", () => received(frames.slice(offset), frame =>
    frame.type === "thread_turn" && frame.turn.id === source.id && frame.turn.state === "completed"
  ), 15_000);
  const updated = await poll("material Task upsert", () => received(frames.slice(offset), frame =>
    frame.type === "thread_upsert"
      && frame.thread.id === task.id
      && frame.thread.state.headline === "Release evidence ready"
      && frame.thread.state.findings?.length === 2
      && frame.thread.state.artifact_ids?.length === 1
      && frame.thread.state.revision >= 3
  )?.thread, 10_000);
  const artifactId = updated.state.artifact_ids[0];
  const artifact = await poll("artifact revision upsert", () => received(frames.slice(offset), frame =>
    frame.type === "artifact_upsert" && frame.artifact.id === artifactId && frame.artifact.revision === 2
  )?.artifact, 10_000);
  const effects = received(frames.slice(offset), frame =>
    frame.type === "thread_effects_changed" && frame.turn_id === source.id
  )?.effects ?? [];
  assert.equal(effects.filter(effect => effect.receipt.tool === "threads_state" && effect.receipt.effect === "edited").length, 1);

  if (await drawer.getAttribute("aria-expanded") === "false") await drawer.click();
  await page.locator(`[data-thread-row="${task.id}"]`).click();
  const state = page.locator(`main[data-thread-id="${task.id}"] [data-slot="task-state"]`);
  await state.getByText("Release evidence ready", { exact: true }).waitFor();
  await state.getByText("Linux checks passed", { exact: true }).waitFor();
  await state.getByText("Android remains", { exact: true }).waitFor();
  await state.getByText("Task state evidence", { exact: true }).waitFor();

  const reloadOffset = frames.length;
  await page.reload({ waitUntil: "domcontentloaded" });
  await poll("Task-state reload", () => received(frames.slice(reloadOffset), frame =>
    frame.type === "hello_ok" && frame.history_id === hello.history_id
  ), 10_000);
  await page.locator(`main[data-thread-id="${task.id}"]`).waitFor();
  await page.locator(`main[data-thread-id="${task.id}"] [data-slot="task-state"]`).getByText("Release evidence ready", { exact: true }).waitFor();
  assert.deepEqual(browserErrors, []);

  if (evidenceDir) {
    await page.screenshot({ path: `${evidenceDir}/task-state.png`, fullPage: true });
    await writeFile(`${evidenceDir}/task-state.json`, `${JSON.stringify({
      historyId: hello.history_id,
      taskId: task.id,
      stateRevision: updated.state.revision,
      artifactId,
      artifactRevision: artifact.revision,
      sourceTurnId: source.id,
      browserErrors,
    }, null, 2)}\n`);
  }
  console.log("Task state, artifact revision, parent rollup and reload passed.");
} finally {
  await browser.close();
}
