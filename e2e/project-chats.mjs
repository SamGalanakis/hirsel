// Deterministic project-chat contract against an isolated scripted Host.
import assert from "node:assert/strict";
import { mkdir, writeFile } from "node:fs/promises";

import { isolatedUrl, launchBrowser, poll, request } from "./lib/harness.mjs";

const url = isolatedUrl(process.env.HIRSEL_PROJECT_CHATS_URL, "HIRSEL_PROJECT_CHATS_URL");
const token = process.env.HIRSEL_PROJECT_CHATS_TOKEN ?? "dev-token";
const evidenceDir = process.env.HIRSEL_THREAD_SMOKE_ARTIFACTS;
if (evidenceDir) await mkdir(evidenceDir, { recursive: true });

function received(frames, predicate) {
  return frames.findLast(row => row.direction === "received" && predicate(row.frame))?.frame;
}

function sent(frames, predicate) {
  return frames.findLast(row => row.direction === "sent" && predicate(row.frame))?.frame;
}

async function contextText(page) {
  return page.locator('[data-slot="composer-context"]').innerText();
}

const browser = await launchBrowser();
const evidence = {};
try {
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  const frames = [];
  const errors = [];
  page.on("pageerror", error => errors.push(error.message));
  page.on("websocket", socket => {
    socket.on("framesent", event => {
      try { frames.push({ direction: "sent", frame: JSON.parse(event.payload.toString()) }); }
      catch { /* Binary frames are outside this protocol. */ }
    });
    socket.on("framereceived", event => {
      try { frames.push({ direction: "received", frame: JSON.parse(event.payload.toString()) }); }
      catch { /* Binary frames are outside this protocol. */ }
    });
  });
  await page.addInitScript(value => {
    if (window === window.top) localStorage.setItem("hirsel.token", value);
  }, token);
  await page.goto(url, { waitUntil: "domcontentloaded" });
  const hello = await poll("project-chat hello", () => received(frames, frame => frame.type === "hello_ok"), 10_000);
  const home = await poll("Home bootstrap", () => {
    const frame = received(frames, candidate =>
      (candidate.type === "thread_created" || candidate.type === "thread_upsert")
      && candidate.thread?.title === "Home"
      && candidate.thread.kind === "space"
      && candidate.thread.parent_thread_id === null
    );
    return frame?.thread;
  }, 10_000).catch(error => {
    console.error(JSON.stringify({ errors, frames: frames.slice(-20) }, null, 2));
    throw error;
  });
  await page.locator(`main[data-thread-id="${home.id}"]`).waitFor({ state: "visible" });
  assert.equal(new URL(page.url()).pathname, `/t/${home.id}`);
  assert.match(await contextText(page), /Project\s+Home/);
  assert.match(await contextText(page), /Focus\s+None/);
  assert.match(await contextText(page), /Worker\s+None/);
  const homeDetail = await request({
    url,
    token,
    frame: { type: "open_thread", client_id: crypto.randomUUID(), thread_id: home.id },
    expected: "thread_opened",
  });
  assert.deepEqual(homeDetail.detail.grants, [], "Home received an automatic reach grant");

  await page.getByRole("button", { name: "New Space or Task", exact: true }).first().click();
  const create = page.getByRole("dialog", { name: "New Space or Task", exact: true });
  await create.waitFor({ state: "visible" });
  const taskTitle = `Project contract ${crypto.randomUUID().slice(0, 8)}`;
  await create.getByLabel("New space or task title", { exact: true }).fill(taskTitle);
  await create.getByRole("button", { name: "Kind: Space", exact: true }).click();
  await page.getByRole("menuitemradio", { name: "task", exact: true }).click();
  await create.getByRole("button", { name: "Inside: Top level", exact: true }).click();
  await page.getByRole("menuitem", { name: "Home", exact: true }).click();
  await create.getByRole("button", { name: "Create Task", exact: true }).click();
  const task = await poll("project Task creation", () => received(frames, frame =>
    frame.type === "thread_created"
      && frame.thread?.title === taskTitle
      && frame.thread.kind === "task"
      && frame.thread.parent_thread_id === home.id
  )?.thread, 10_000);
  await page.locator(`main[data-thread-id="${task.id}"]`).waitFor({ state: "visible" });
  await page.getByRole("textbox", { name: `Step in with worker ${taskTitle}`, exact: true }).waitFor();
  assert.match(await contextText(page), /Project\s+Home/);
  assert.match(await contextText(page), new RegExp(`Worker\\s+${taskTitle}`));

  await page.getByRole("button", { name: "Talk about this", exact: true }).click();
  await page.locator(`main[data-thread-id="${home.id}"]`).waitFor({ state: "visible" });
  assert.match(await contextText(page), new RegExp(`Focus\\s+${taskTitle}`));
  assert.match(await contextText(page), /Worker\s+None/);
  const focusBody = `Discuss ${taskTitle}`;
  const projectComposer = page.getByRole("textbox", { name: "Message project chat Home", exact: true });
  await projectComposer.fill(focusBody);
  const focusOffset = frames.length;
  await page.getByRole("button", { name: "Send", exact: true }).click();
  const focusedSend = await poll("focused project send", () => sent(frames.slice(focusOffset), frame =>
    frame.type === "send_thread_message" && frame.thread_id === home.id && frame.body === focusBody
  ), 10_000);
  assert.equal(focusedSend.focus.task_thread_id, task.id);
  assert.deepEqual(Object.keys(focusedSend.focus.snapshot).sort(), ["brief", "instrument_summary", "title"]);
  assert.equal(focusedSend.focus.snapshot.title, taskTitle);
  const focusedEcho = await poll("focused message echo", () => received(frames.slice(focusOffset), frame =>
    frame.type === "msg" && frame.message.author === "owner" && frame.message.body === focusBody
  )?.message, 10_000);
  assert.deepEqual(focusedEcho.focus, focusedSend.focus);
  await poll("focus consumed after acceptance", async () => /Focus\s+None/.test(await contextText(page)), 10_000);
  const focusedTurn = await poll("focused project turn", () => received(frames.slice(focusOffset), frame =>
    frame.type === "thread_turn" && frame.turn.thread_id === home.id && frame.turn.owner_message_id === focusedEcho.id
  )?.turn, 10_000);
  await poll("focused project reply", () => received(frames.slice(focusOffset), frame =>
    frame.type === "thread_turn" && frame.turn.id === focusedTurn.id
      && ["completed", "failed", "cancelled", "interrupted"].includes(frame.turn.state)
  ), 15_000);

  const drawerTrigger = page.getByRole("button", { name: "Spaces and Tasks", exact: true });
  if (await drawerTrigger.getAttribute("aria-expanded") === "false") await drawerTrigger.click();
  await page.locator(`[data-thread-row="${task.id}"]`).click();
  await page.locator(`main[data-thread-id="${task.id}"]`).waitFor({ state: "visible" });
  const workerComposer = page.getByRole("textbox", { name: `Step in with worker ${taskTitle}`, exact: true });
  await workerComposer.fill("slow:1.5");
  const workerOffset = frames.length;
  await page.getByRole("button", { name: "Send", exact: true }).click();
  const workerTurn = await poll("running worker turn", () => received(frames.slice(workerOffset), frame =>
    frame.type === "thread_turn" && frame.turn.thread_id === task.id && frame.turn.state === "running"
  )?.turn, 10_000);
  await page.getByRole("button", { name: "Stop the agent", exact: true }).waitFor({ state: "visible" });
  await page.getByRole("button", { name: "Send after current turn", exact: true }).waitFor({ state: "visible" });
  await page.getByRole("button", { name: "Stop the agent", exact: true }).click();
  await poll("stopped worker turn", () => received(frames.slice(workerOffset), frame =>
    frame.type === "thread_turn" && frame.turn.id === workerTurn.id
      && ["cancelled", "interrupted", "completed"].includes(frame.turn.state)
  ), 10_000);

  await page.getByRole("button", { name: "Project chat", exact: true }).click();
  await page.locator(`main[data-thread-id="${home.id}"]`).waitFor({ state: "visible" });
  const delegationBody = `Please delegate this scripted check ${crypto.randomUUID()}`;
  await projectComposer.fill(delegationBody);
  const delegationOffset = frames.length;
  await page.getByRole("button", { name: "Send", exact: true }).click();
  const delegated = await poll("scripted atomic delegation", () => received(frames.slice(delegationOffset), frame =>
    frame.type === "thread_upsert"
      && frame.thread?.title === "Repository fix"
      && frame.thread.kind === "task"
      && frame.thread.parent_thread_id === home.id
  )?.thread, 15_000);

  const reloadOffset = frames.length;
  await page.goto(url, { waitUntil: "domcontentloaded" });
  const reloadedHello = await poll("route-free reload", () => received(frames.slice(reloadOffset), frame =>
    frame.type === "hello_ok" && frame.history_id === hello.history_id
  ), 10_000);
  await page.locator(`main[data-thread-id="${home.id}"]`).waitFor({ state: "visible" });
  assert.equal(new URL(page.url()).pathname, `/t/${home.id}`);
  assert.equal(
    sent(frames.slice(reloadOffset), frame => frame.type === "ensure_home_project"),
    undefined,
    "route-free reload bootstrapped Home instead of restoring the last project",
  );
  assert.equal(
    reloadedHello.threads.filter(thread => thread.title === "Home" && thread.kind === "space" && thread.parent_thread_id === null).length,
    1,
    "Home bootstrap was not idempotent",
  );
  assert.deepEqual(errors, []);
  Object.assign(evidence, {
    historyId: hello.history_id,
    homeId: home.id,
    taskId: task.id,
    delegatedTaskId: delegated.id,
    focusedSend,
    browserErrors: errors,
  });
  if (evidenceDir) {
    await page.screenshot({ path: `${evidenceDir}/project-chats.png`, fullPage: true });
    await writeFile(`${evidenceDir}/project-chats.json`, `${JSON.stringify(evidence, null, 2)}\n`);
  }
  await page.close();
  console.log("Project landing, explicit focus, worker pairing, queued send and delegation passed.");
} finally {
  await browser.close();
}
