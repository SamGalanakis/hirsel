// Durable effect pills against an isolated scripted Host. Every case waits for
// a real terminal source turn; the paused target is explicitly cancelled.
import assert from "node:assert/strict";
import { mkdir, writeFile } from "node:fs/promises";

import { isolatedUrl, launchBrowser, poll, request } from "./lib/harness.mjs";

const url = isolatedUrl(process.env.HIRSEL_EFFECT_PILLS_URL, "HIRSEL_EFFECT_PILLS_URL");
const token = process.env.HIRSEL_EFFECT_PILLS_TOKEN ?? "dev-token";
const evidenceDir = process.env.HIRSEL_THREAD_SMOKE_ARTIFACTS;
if (evidenceDir) await mkdir(evidenceDir, { recursive: true });

function received(frames, predicate) {
  return frames.findLast(row => row.direction === "received" && predicate(row.frame))?.frame;
}
function sent(frames, predicate) {
  return frames.findLast(row => row.direction === "sent" && predicate(row.frame))?.frame;
}
const terminal = state => ["completed", "failed", "cancelled", "interrupted"].includes(state);

const browser = await launchBrowser();
try {
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  const frames = [];
  const browserErrors = [];
  page.on("pageerror", error => browserErrors.push(error.message));
  page.on("websocket", socket => {
    socket.on("framesent", event => {
      try { frames.push({ direction: "sent", frame: JSON.parse(event.payload.toString()) }); } catch { /* binary is outside the protocol */ }
    });
    socket.on("framereceived", event => {
      try { frames.push({ direction: "received", frame: JSON.parse(event.payload.toString()) }); } catch { /* binary is outside the protocol */ }
    });
  });
  await page.addInitScript(value => { if (window === window.top) localStorage.setItem("hirsel.token", value); }, token);
  await page.goto(url, { waitUntil: "domcontentloaded" });
  const hello = await poll("effect-pills hello", () => received(frames, frame => frame.type === "hello_ok"), 10_000);
  const home = await poll("effect-pills Home", () => received(frames, frame =>
    (frame.type === "thread_created" || frame.type === "thread_upsert")
      && frame.thread?.title === "Home"
      && frame.thread.kind === "space"
  )?.thread, 10_000);
  await page.locator(`main[data-thread-id="${home.id}"]`).waitFor();

  const peer = await request({
    url,
    token,
    frame: { type: "create_thread", client_id: crypto.randomUUID(), history_id: hello.history_id, title: "Foreign refusal target", kind: "task", parent_thread_id: null },
    expected: "thread_created",
  });

  const composer = page.getByRole("textbox", { name: "Message project chat Home", exact: true });
  async function sendFixture(body, label) {
    const offset = frames.length;
    await composer.fill(body);
    await page.getByRole("button", { name: "Send", exact: true }).click();
    const owner = await poll(`${label} owner echo`, () => received(frames.slice(offset), frame => frame.type === "msg" && frame.message.author === "owner" && frame.message.body === body)?.message, 10_000);
    const turn = await poll(`${label} accepted turn`, () => received(frames.slice(offset), frame => frame.type === "thread_turn" && frame.turn.thread_id === home.id && frame.turn.owner_message_id === owner.id)?.turn, 10_000);
    const effects = await poll(`${label} effect receipt`, () => received(frames.slice(offset), frame => frame.type === "thread_effects_changed" && frame.turn_id === turn.id && frame.effects.length > 0), 10_000).catch(error => {
      const recent = frames.slice(offset).filter(row => row.direction === "received").slice(-12).map(row => ({ type: row.frame.type, turn: row.frame.turn?.id ?? row.frame.turn_id, state: row.frame.turn?.state, effects: row.frame.effects?.map(effect => effect.receipt), detail: row.frame.detail }));
      throw new Error(`${error.message}; recent received frames: ${JSON.stringify(recent)}`);
    });
    return { offset, turn, effects };
  }

  const paused = await sendFixture("__hirsel_effect_pills_paused__", "paused");
  const delegated = paused.effects.effects.find(effect => effect.receipt.effect === "delegated");
  assert(delegated?.receipt.target_turn_id, "paused delegation omitted its exact target turn");
  const pausedTurnId = delegated.receipt.target_turn_id;
  await poll("paused target running", () => received(frames.slice(paused.offset), frame => frame.type === "thread_turn" && frame.turn.id === pausedTurnId && frame.turn.state === "running"), 10_000);
  const stopProjection = await poll("paused Stop projection", () => received(frames.slice(paused.offset), frame => frame.type === "thread_effects_changed" && frame.turn_id === paused.turn.id && frame.effects.some(effect => effect.actions.some(action => action.kind === "stop" && action.turn_id === pausedTurnId))), 10_000);
  assert(stopProjection.effects.some(effect => effect.receipt.effect === "created"), "new delegation omitted the created receipt");
  // The source run card may exchange its streaming and durable render at this
  // instant. Dispatch through the mounted control without Playwright's
  // stability wait so that transition cannot outlive the target turn.
  await poll("mounted Stop control", () => page.evaluate(turnId => {
    const pill = document.querySelector(`[data-turn-id="${turnId}"] [data-effect="delegated"]`);
    const button = [...(pill?.querySelectorAll("button") ?? [])].find(candidate => candidate.textContent?.trim() === "Stop");
    button?.click();
    return Boolean(button);
  }, paused.turn.id), 10_000);
  const cancellation = await poll("exact pill cancellation", () => sent(frames.slice(paused.offset), frame => frame.type === "cancel_thread_turn" && frame.turn_id === pausedTurnId), 10_000);
  assert.equal(cancellation.expected_state, "running");
  await poll("paused target terminal", () => received(frames.slice(paused.offset), frame => frame.type === "thread_turn" && frame.turn.id === pausedTurnId && terminal(frame.turn.state)), 10_000);
  await poll("paused source completed", () => received(frames.slice(paused.offset), frame => frame.type === "thread_turn" && frame.turn.id === paused.turn.id && frame.turn.state === "completed"), 10_000);

  const failed = await sendFixture("__hirsel_effect_pills_failed__", "failed");
  const failedTerminal = await poll("failed source terminal", () => received(frames.slice(failed.offset), frame => frame.type === "thread_turn" && frame.turn.id === failed.turn.id && terminal(frame.turn.state))?.turn, 10_000);
  assert.equal(failedTerminal.state, "failed");
  assert.equal(failedTerminal.agent_message_id, null, "failed fixture unexpectedly created a final reply");
  await page.locator(`[data-turn-id="${failed.turn.id}"] [data-effect="delegated"]`).waitFor();

  const refusal = await sendFixture(`__hirsel_effect_pills_refused__:${peer.thread.id}`, "refused");
  const refusedReceipt = refusal.effects.effects.find(effect => effect.receipt.effect === "refused");
  assert.deepEqual(refusedReceipt?.receipt.target, { kind: "thread", thread_id: peer.thread.id });
  assert.equal(refusedReceipt?.receipt.refusal?.reason, "outside_grant");
  await poll("refused source terminal", () => received(frames.slice(refusal.offset), frame => frame.type === "thread_turn" && frame.turn.id === refusal.turn.id && terminal(frame.turn.state)), 10_000);
  const refusedPill = page.locator(`[data-turn-id="${refusal.turn.id}"] [data-effect="refused"]`);
  await refusedPill.getByRole("button", { name: "Review reach", exact: true }).waitFor();
  assert.match(await refusedPill.innerText(), /everything below it/i);

  const reloadOffset = frames.length;
  await page.reload({ waitUntil: "domcontentloaded" });
  await poll("effect-pills reload", () => received(frames.slice(reloadOffset), frame => frame.type === "hello_ok" && frame.history_id === hello.history_id), 10_000);
  await page.locator(`main[data-thread-id="${home.id}"]`).waitFor();
  for (const [label, turnId, kind] of [["paused", paused.turn.id, "delegated"], ["failed", failed.turn.id, "delegated"], ["refused", refusal.turn.id, "refused"]]) {
    await page.locator(`[data-turn-id="${turnId}"] [data-effect="${kind}"]`).waitFor({ state: "visible" });
    assert(received(frames.slice(reloadOffset), frame => frame.type === "thread_opened" && frame.detail.effects.some(effect => effect.receipt.turn_id === turnId)), `${label} receipt missing from reload snapshot`);
  }
  assert.deepEqual(browserErrors, []);

  if (evidenceDir) {
    await page.screenshot({ path: `${evidenceDir}/effect-pills.png`, fullPage: true });
    await writeFile(`${evidenceDir}/effect-pills.json`, `${JSON.stringify({ historyId: hello.history_id, pausedTurnId: paused.turn.id, failedTurnId: failed.turn.id, refusedTurnId: refusal.turn.id, targetTurnId: pausedTurnId, cancellation, browserErrors }, null, 2)}\n`);
  }
  console.log("Paused, failed, refused and reloaded effect pills passed with real terminal turns.");
} finally {
  await browser.close();
}
