// Deterministic Space-chat contract against an isolated scripted Host.
import assert from "node:assert/strict";
import { mkdir, writeFile } from "node:fs/promises";

import { isolatedUrl, launchBrowser, poll, request } from "./lib/harness.mjs";

const url = isolatedUrl(process.env.HIRSEL_SPACE_CHATS_URL, "HIRSEL_SPACE_CHATS_URL");
const token = process.env.HIRSEL_SPACE_CHATS_TOKEN ?? "dev-token";
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
  const hello = await poll("Space-chat hello", () => received(frames, frame => frame.type === "hello_ok"), 10_000);
  const home = await poll("Home bootstrap", () => {
    const snapshot = hello.threads.find(thread =>
      thread.title === "Home" && thread.kind === "space" && thread.parent_thread_id === null
    );
    if (snapshot) return snapshot;
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
  assert.match(await contextText(page), /Recipient\s+Home/);
  const homeDetail = await request({
    url,
    token,
    frame: { type: "open_thread", client_id: crypto.randomUUID(), thread_id: home.id },
    expected: "thread_opened",
  });
  assert.deepEqual(homeDetail.detail.grants, [], "Home received an automatic reach grant");

  await page.getByRole("button", { name: "New Space or Task", exact: true }).first().click();
  const nestedCreate = page.getByRole("dialog", { name: "New Space or Task", exact: true });
  await nestedCreate.waitFor({ state: "visible" });
  const nestedTitle = `Nested Space ${crypto.randomUUID().slice(0, 8)}`;
  await nestedCreate.getByLabel("New space or task title", { exact: true }).fill(nestedTitle);
  await nestedCreate.getByRole("button", { name: "Inside: Top level", exact: true }).click();
  await page.getByRole("menuitem", { name: "Home", exact: true }).click();
  await nestedCreate.getByRole("button", { name: "Create Space", exact: true }).click();
  const nested = await poll("nested Space creation", () => received(frames, frame =>
    frame.type === "thread_created"
      && frame.thread?.title === nestedTitle
      && frame.thread.kind === "space"
      && frame.thread.parent_thread_id === home.id
  )?.thread, 10_000);
  await page.locator(`main[data-thread-id="${nested.id}"]`).waitFor({ state: "visible" });
  await page.getByRole("textbox", { name: `Message Space chat ${nestedTitle}`, exact: true }).waitFor();
  assert.match(await contextText(page), new RegExp(`Recipient\\s+${nestedTitle}`));
  assert.equal(
    await page.getByRole("textbox", { name: `Step in with worker ${nestedTitle}`, exact: true }).count(),
    0,
    "nested Space was presented as a worker",
  );
  const nestedDrawerTrigger = page.getByRole("button", { name: "Spaces and Tasks", exact: true });
  if (await nestedDrawerTrigger.getAttribute("aria-expanded") === "false") await nestedDrawerTrigger.click();
  await page.locator(`[data-thread-row="${home.id}"]`).click();
  await page.locator(`main[data-thread-id="${home.id}"]`).waitFor({ state: "visible" });

  await page.getByRole("button", { name: "New Space or Task", exact: true }).first().click();
  const create = page.getByRole("dialog", { name: "New Space or Task", exact: true });
  await create.waitFor({ state: "visible" });
  const taskTitle = `Space contract ${crypto.randomUUID().slice(0, 8)}`;
  await create.getByLabel("New space or task title", { exact: true }).fill(taskTitle);
  await create.getByRole("button", { name: "Kind: Space", exact: true }).click();
  await page.getByRole("menuitemradio", { name: "task", exact: true }).click();
  await create.getByRole("button", { name: "Inside: Top level", exact: true }).click();
  await page.getByRole("menuitem", { name: "Home", exact: true }).click();
  await create.getByRole("button", { name: "Create Task", exact: true }).click();
  const task = await poll("Space Task creation", () => received(frames, frame =>
    frame.type === "thread_created"
      && frame.thread?.title === taskTitle
      && frame.thread.kind === "task"
      && frame.thread.parent_thread_id === home.id
  )?.thread, 10_000);
  await page.locator(`main[data-thread-id="${task.id}"]`).waitFor({ state: "visible" });
  await page.getByRole("textbox", { name: `Step in with worker ${taskTitle}`, exact: true }).waitFor();
  assert.match(await contextText(page), new RegExp(`Recipient\\s+${taskTitle}\\s+·\\s+Task worker`));

  await page.getByRole("button", { name: "Talk about this", exact: true }).click();
  await page.locator(`main[data-thread-id="${home.id}"]`).waitFor({ state: "visible" });
  assert.match(await contextText(page), /Recipient\s+Home/);
  const focusBody = `#${task.id} Discuss ${taskTitle}`;
  const spaceComposer = page.getByRole("textbox", { name: "Message Space chat Home", exact: true });
  assert.match(await spaceComposer.inputValue(), new RegExp(`#${task.id}`));
  await spaceComposer.fill(focusBody);
  const focusOffset = frames.length;
  await page.getByRole("button", { name: "Send", exact: true }).click();
  const focusedSend = await poll("focused Space send", () => sent(frames.slice(focusOffset), frame =>
    frame.type === "send_thread_message" && frame.thread_id === home.id && frame.body === focusBody
  ), 10_000);
  assert.deepEqual(focusedSend.mentions, [task.id]);
  const focusedEcho = await poll("focused message echo", () => received(frames.slice(focusOffset), frame =>
    frame.type === "msg" && frame.message.author === "owner" && frame.message.body === focusBody
  )?.message, 10_000);
  const focusedTurn = await poll("focused Space turn", () => received(frames.slice(focusOffset), frame =>
    frame.type === "thread_turn" && frame.turn.thread_id === home.id && frame.turn.owner_message_id === focusedEcho.id
  )?.turn, 10_000);
  const focusedCompleted = await poll("focused Space completion", () => received(frames.slice(focusOffset), frame =>
    frame.type === "thread_turn" && frame.turn.id === focusedTurn.id
      && frame.turn.state === "completed"
  )?.turn, 15_000);
  assert.ok(focusedCompleted.agent_message_id, "focused Space turn completed without an Agent reply");
  await poll("addressed Space Agent reply", () => received(frames.slice(focusOffset), frame =>
    frame.type === "msg"
      && frame.message.id === focusedCompleted.agent_message_id
      && frame.message.thread_id === home.id
      && frame.message.author === "agent"
  )?.message, 10_000);

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
  const queuedBody = `Queued worker follow-up ${crypto.randomUUID()}`;
  await workerComposer.fill(queuedBody);
  const queuedOffset = frames.length;
  await page.getByRole("button", { name: "Send after current turn", exact: true }).click();
  const queuedSend = await poll("queued worker send", () => sent(frames.slice(queuedOffset), frame =>
    frame.type === "send_thread_message"
      && frame.thread_id === task.id
      && frame.body === queuedBody
      && frame.mode === "next_turn"
  ), 10_000);
  const queuedEcho = await poll("queued worker identity", () => received(frames.slice(queuedOffset), frame =>
    frame.type === "msg"
      && frame.message.client_id === queuedSend.client_id
      && frame.message.thread_id === task.id
      && frame.message.author === "owner"
  )?.message, 10_000);
  const queuedTurn = await poll("queued worker turn", () => received(frames.slice(queuedOffset), frame =>
    frame.type === "thread_turn"
      && frame.turn.thread_id === task.id
      && frame.turn.owner_message_id === queuedEcho.id
      && frame.turn.state === "queued"
  )?.turn, 10_000);
  await poll("first worker completion", () => received(frames.slice(workerOffset), frame =>
    frame.type === "thread_turn" && frame.turn.id === workerTurn.id && frame.turn.state === "completed"
  ), 10_000);
  const queuedCompleted = await poll("queued worker completion", () => received(frames.slice(queuedOffset), frame =>
    frame.type === "thread_turn" && frame.turn.id === queuedTurn.id && frame.turn.state === "completed"
  )?.turn, 10_000);
  assert.ok(queuedCompleted.agent_message_id, "queued worker turn completed without an Agent reply");

  const cancelBody = `slow:5 cancellation ${crypto.randomUUID()}`;
  await workerComposer.fill(cancelBody);
  const cancelOffset = frames.length;
  await page.getByRole("button", { name: "Send", exact: true }).click();
  const cancelSend = await poll("cancellable worker send", () => sent(frames.slice(cancelOffset), frame =>
    frame.type === "send_thread_message"
      && frame.thread_id === task.id
      && frame.body === cancelBody
  ), 10_000);
  const cancelEcho = await poll("cancellable worker identity", () => received(frames.slice(cancelOffset), frame =>
    frame.type === "msg"
      && frame.message.client_id === cancelSend.client_id
      && frame.message.thread_id === task.id
      && frame.message.author === "owner"
  )?.message, 10_000);
  const cancelTurn = await poll("cancellable worker turn", () => received(frames.slice(cancelOffset), frame =>
    frame.type === "thread_turn"
      && frame.turn.thread_id === task.id
      && frame.turn.owner_message_id === cancelEcho.id
      && frame.turn.state === "running"
  )?.turn, 10_000);
  const stop = page.getByRole("button", { name: "Stop the agent", exact: true });
  await stop.waitFor({ state: "visible" });
  await stop.click();
  await poll("cancelled worker terminal state", () => received(frames.slice(cancelOffset), frame =>
    frame.type === "thread_turn"
      && frame.turn.id === cancelTurn.id
      && ["cancelled", "interrupted"].includes(frame.turn.state)
  )?.turn, 10_000);

  await page.getByRole("button", { name: "Space chat", exact: true }).click();
  await page.locator(`main[data-thread-id="${home.id}"]`).waitFor({ state: "visible" });
  const delegationBody = `Please delegate this scripted check ${crypto.randomUUID()}`;
  await spaceComposer.fill(delegationBody);
  const delegationOffset = frames.length;
  await page.getByRole("button", { name: "Send", exact: true }).click();
  const delegated = await poll("scripted atomic delegation", () => received(frames.slice(delegationOffset), frame =>
    frame.type === "thread_upsert"
      && frame.thread?.title === "Repository fix"
      && frame.thread.kind === "task"
      && frame.thread.parent_thread_id === home.id
  )?.thread, 15_000);
  const delegatedTurn = await poll("delegated child ran", () => received(frames.slice(delegationOffset), frame =>
    frame.type === "thread_turn"
      && frame.turn.thread_id === delegated.id
      && frame.turn.state === "completed"
  )?.turn, 15_000);
  assert.ok(delegatedTurn.agent_message_id, "delegated child completed without an Agent reply");
  await poll("delegated child Agent reply", () => received(frames.slice(delegationOffset), frame =>
    frame.type === "msg"
      && frame.message.id === delegatedTurn.agent_message_id
      && frame.message.thread_id === delegated.id
      && frame.message.author === "agent"
  )?.message, 10_000);

  const reloadOffset = frames.length;
  await page.goto(url, { waitUntil: "domcontentloaded" });
  const reloadedHello = await poll("route-free reload", () => received(frames.slice(reloadOffset), frame =>
    frame.type === "hello_ok" && frame.history_id === hello.history_id
  ), 10_000);
  await page.locator(`main[data-thread-id="${home.id}"]`).waitFor({ state: "visible" });
  assert.equal(new URL(page.url()).pathname, `/t/${home.id}`);
  assert.equal(
    sent(frames.slice(reloadOffset), frame => frame.type === "create_thread" && frame.title === "Home"),
    undefined,
    "route-free reload bootstrapped Home instead of restoring the last Space",
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
    nestedSpaceId: nested.id,
    taskId: task.id,
    delegatedTaskId: delegated.id,
    queuedTurnId: queuedTurn.id,
    cancelledTurnId: cancelTurn.id,
    delegatedTurnId: delegatedTurn.id,
    focusedSend,
    browserErrors: errors,
  });
  if (evidenceDir) {
    await page.screenshot({ path: `${evidenceDir}/space-chats.png`, fullPage: true });
    await writeFile(`${evidenceDir}/space-chats.json`, `${JSON.stringify(evidence, null, 2)}\n`);
  }
  await page.close();
  console.log("Space landing, Task reference, worker pairing, queued send, cancellation and delegation passed.");
} finally {
  await browser.close();
}
