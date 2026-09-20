// Outside material changes coalesce visibly and enter the next Space-chat turn
// through the exact persisted admission snapshot.
import assert from "node:assert/strict";
import { mkdir, writeFile } from "node:fs/promises";

import { isolatedUrl, launchBrowser, poll, request } from "./lib/harness.mjs";

const url = isolatedUrl(process.env.HIRSEL_CHANGE_DIGEST_URL, "HIRSEL_CHANGE_DIGEST_URL");
const token = process.env.HIRSEL_CHANGE_DIGEST_TOKEN ?? "dev-token";
const evidenceDir = process.env.HIRSEL_THREAD_SMOKE_ARTIFACTS;
if (evidenceDir) await mkdir(evidenceDir, { recursive: true });

function received(frames, predicate) {
  return frames.findLast(row => row.direction === "received" && predicate(row.frame))?.frame;
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
      try { frames.push({ direction: "sent", frame: JSON.parse(event.payload.toString()) }); } catch { /* protocol is JSON */ }
    });
    socket.on("framereceived", event => {
      try { frames.push({ direction: "received", frame: JSON.parse(event.payload.toString()) }); } catch { /* protocol is JSON */ }
    });
  });
  await page.addInitScript(value => { if (window === window.top) localStorage.setItem("hirsel.token", value); }, token);
  await page.goto(url, { waitUntil: "domcontentloaded" });
  const hello = await poll("change-digest hello", () => received(frames, frame => frame.type === "hello_ok"), 10_000);
  await poll("change-digest Home", () => received(frames, frame =>
    (frame.type === "thread_created" || frame.type === "thread_upsert") && frame.thread?.title === "Home"
  ), 10_000);

  const suffix = crypto.randomUUID().slice(0, 8);
  async function create(title, kind, parentThreadId) {
    return (await request({
      url,
      token,
      frame: {
        type: "create_thread",
        client_id: crypto.randomUUID(),
        history_id: hello.history_id,
        title,
        kind,
        parent_thread_id: parentThreadId,
      },
      expected: "thread_created",
    })).thread;
  }
  const source = await create(`Digest source ${suffix}`, "space", null);
  const sourceTask = await create(`Digest writer ${suffix}`, "task", source.id);
  const target = await create(`Digest target ${suffix}`, "space", null);
  const targetTask = await create(`Digest work ${suffix}`, "task", target.id);
  await request({
    url,
    token,
    frame: {
      type: "grant_thread_reach",
      client_id: crypto.randomUUID(),
      history_id: hello.history_id,
      thread_id: source.id,
      target: target.id,
      note: "deterministic outside-change fixture",
    },
    expected: "thread_grants_changed",
  });

  let revision = targetTask.state.revision;
  let firstOutsideActivity;
  for (const index of [1, 2]) {
    const offset = frames.length;
    const body = `__hirsel_change_digest_emit__:${targetTask.id}:${revision}`;
    await request({
      url,
      token,
      frame: {
        type: "send_thread_message",
        client_id: crypto.randomUUID(),
        history_id: hello.history_id,
        thread_id: source.id,
        body,
        attachments: [],
        mentions: [],
        mode: "send",
        artifact_ids: [],
      },
      expected: "msg",
    });
    const turn = await poll(`change-digest source turn ${index}`, () => received(frames.slice(offset), frame =>
      frame.type === "thread_turn" && frame.turn.thread_id === source.id
    )?.turn, 10_000);
    await poll(`change-digest source completion ${index}`, () => received(frames.slice(offset), frame =>
      frame.type === "thread_turn" && frame.turn.id === turn.id && terminal(frame.turn.state)
    ), 15_000);
    const updated = await poll(`change-digest Task state ${index}`, () => received(frames.slice(offset), frame =>
      frame.type === "thread_upsert"
        && frame.thread.id === targetTask.id
        && frame.thread.state.headline === "Outside digest ready"
    )?.thread, 10_000);
    revision = updated.state.revision;
    const activity = await poll(`outside-change activity ${index}`, () => received(frames.slice(offset), frame =>
      frame.type === "thread_activity"
        && frame.activity.thread_id === target.id
        && frame.activity.kind === "outside_change"
    )?.activity, 10_000);
    firstOutsideActivity ??= activity;
    assert.equal(activity.id, firstOutsideActivity.id, "outside changes were not coalesced by durable activity identity");
  }

  const drawer = page.getByRole("button", { name: "Spaces and Tasks", exact: true });
  if (await drawer.getAttribute("aria-expanded") === "false") await drawer.click();
  await page.locator(`[data-thread-row="${target.id}"]`).click();
  const activityLine = page.getByText(new RegExp(`^Changed by ${source.title} · \\d+ updates$`));
  await activityLine.waitFor();
  assert.equal(await activityLine.count(), 1, "the target Space chat rendered duplicate outside-change lines");

  const captureOffset = frames.length;
  const composer = page.getByRole("textbox", { name: `Message Space chat ${target.title}`, exact: true });
  await composer.fill("__hirsel_change_digest_capture__");
  await page.getByRole("button", { name: "Send", exact: true }).click();
  const owner = await poll("change-digest capture owner message", () => received(frames.slice(captureOffset), frame =>
    frame.type === "msg" && frame.message.thread_id === target.id && frame.message.body === "__hirsel_change_digest_capture__"
  )?.message, 10_000);
  const captureTurn = await poll("change-digest capture turn", () => received(frames.slice(captureOffset), frame =>
    frame.type === "thread_turn" && frame.turn.thread_id === target.id && frame.turn.owner_message_id === owner.id
  )?.turn, 10_000);
  await poll("change-digest capture completion", () => received(frames.slice(captureOffset), frame =>
    frame.type === "thread_turn" && frame.turn.id === captureTurn.id && frame.turn.state === "completed"
  ), 15_000);
  const agent = await poll("captured scripted input", () => received(frames.slice(captureOffset), frame =>
    frame.type === "msg" && frame.message.thread_id === target.id && frame.message.author === "agent"
  )?.message, 10_000);
  assert.match(agent.body, /Outside digest ready/);
  assert.match(agent.body, /"changes"/);

  const opened = await request({
    url,
    token,
    frame: { type: "open_thread", client_id: crypto.randomUUID(), thread_id: target.id, before_id: null },
    expected: "thread_opened",
  });
  const accepted = opened.detail.accepted_context;
  assert.equal(accepted.turn_id, captureTurn.id);
  assert(accepted.consumed_at, "successful terminal consumption did not advance the cursor");
  const digest = accepted.context.changes;
  assert(digest.changes.some(change => change.thread_id === targetTask.id && change.after_headline === "Outside digest ready"));
  assert(digest.changes.some(change => change.source_space_id === source.id && change.source_space_title === source.title));
  assert.equal(digest.has_more, false);
  assert.deepEqual(browserErrors, []);

  if (evidenceDir) {
    await page.screenshot({ path: `${evidenceDir}/change-digest.png`, fullPage: true });
    await writeFile(`${evidenceDir}/change-digest.json`, `${JSON.stringify({
      historyId: hello.history_id,
      sourceSpaceId: source.id,
      targetSpaceId: target.id,
      targetTaskId: targetTask.id,
      activityId: firstOutsideActivity.id,
      acceptedContext: accepted,
      browserErrors,
    }, null, 2)}\n`);
  }
  console.log("Outside-change coalescing and persisted admission digest passed.");
} finally {
  await browser.close();
}
