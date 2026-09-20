// Effect pills are a pure projection of durable tool timeline events.
import assert from "node:assert/strict";
import { mkdir, writeFile } from "node:fs/promises";

import { isolatedUrl, launchBrowser, poll } from "./lib/harness.mjs";

const url = isolatedUrl(process.env.HIRSEL_EFFECT_PILLS_URL, "HIRSEL_EFFECT_PILLS_URL");
const token = process.env.HIRSEL_EFFECT_PILLS_TOKEN ?? "dev-token";
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
  const hello = await poll("effect-pills hello", () => received(frames, frame => frame.type === "hello_ok"), 10_000);
  const home = await poll("effect-pills Home", () => hello.threads.find(thread =>
    thread.title === "Home" && thread.kind === "space" && thread.parent_thread_id === null
  ) ?? received(frames, frame =>
    (frame.type === "thread_created" || frame.type === "thread_upsert")
      && frame.thread?.title === "Home"
      && frame.thread.kind === "space"
      && frame.thread.parent_thread_id === null
  )?.thread, 10_000);
  await page.locator(`main[data-thread-id="${home.id}"]`).waitFor();

  const offset = frames.length;
  const body = "__hirsel_effect_pills_projection__";
  const composer = page.getByRole("textbox", { name: "Message Space chat Home", exact: true });
  await composer.fill(body);
  await page.getByRole("button", { name: "Send", exact: true }).click();
  const owner = await poll("effect-pills owner echo", () => received(frames.slice(offset), frame =>
    frame.type === "msg" && frame.message.author === "owner" && frame.message.body === body
  )?.message, 10_000);
  const source = await poll("effect-pills source turn", () => received(frames.slice(offset), frame =>
    frame.type === "thread_turn" && frame.turn.owner_message_id === owner.id
  )?.turn, 10_000);
  await poll("effect-pills source completion", () => received(frames.slice(offset), frame =>
    frame.type === "thread_turn" && frame.turn.id === source.id && frame.turn.state === "completed"
  ), 15_000);
  const created = await poll("projected effect target", () => received(frames.slice(offset), frame =>
    frame.type === "thread_upsert" && frame.thread?.title === "Projected effect"
  )?.thread, 10_000);
  const pill = page.locator(`[data-turn-id="${source.id}"] [data-effect="created"]`);
  await pill.getByText("Created · Projected effect", { exact: true }).waitFor();
  assert.equal(await pill.getByRole("button", { name: "Open", exact: true }).count(), 1);
  assert.equal(received(frames.slice(offset), frame => frame.type === "thread_effects_changed"), undefined);

  const reloadOffset = frames.length;
  await page.reload({ waitUntil: "domcontentloaded" });
  await poll("effect-pills reload", () => received(frames.slice(reloadOffset), frame =>
    frame.type === "hello_ok" && frame.history_id === hello.history_id
  ), 10_000);
  await page.locator(`main[data-thread-id="${home.id}"]`).waitFor();
  const reloadedPill = page.locator(`[data-turn-id="${source.id}"] [data-effect="created"]`);
  await reloadedPill.getByText("Created · Projected effect", { exact: true }).waitFor();
  await reloadedPill.getByRole("button", { name: "Open", exact: true }).click();
  await page.locator(`main[data-thread-id="${created.id}"]`).waitFor();
  assert.deepEqual(browserErrors, []);

  if (evidenceDir) {
    await page.screenshot({ path: `${evidenceDir}/effect-pills.png`, fullPage: true });
    await writeFile(`${evidenceDir}/effect-pills.json`, `${JSON.stringify({
      historyId: hello.history_id,
      sourceTurnId: source.id,
      targetThreadId: created.id,
      browserErrors,
    }, null, 2)}\n`);
  }
  console.log("Durable tool timeline projected one reload-stable Open-only effect pill.");
} finally {
  await browser.close();
}
