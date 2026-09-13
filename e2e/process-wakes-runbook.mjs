import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { join } from "node:path";

export async function runProcessWakes(context, { sendMessage, poll, openThread, capture, sqliteJson }) {
  const { page, frames, threadId, nonce, dataDir, scenarioDir, url, token } = context;
  const marker = `awake-${nonce}`;
  const { turnId } = await sendMessage(page, frames, threadId,
    `Wake yourself in 3 seconds. Define exactly one process named wakeAfterThreeSeconds whose only action is returning the exact string ${JSON.stringify(marker)}. Register one timer.Schedule with label "runbook wake" and in_secs:3 targeting it. Do not call any tools inside its body and do not emit an explicit wake. After registration say "registered". When the process result returns, do not register anything else; you may stay quiet.`);
  const detail = await poll("one process delivery and settled normal turn", async () => {
    const detail = await openThread(url, token, threadId);
    const deliveries = detail.messages.filter(message => message.origin?.kind === "process");
    assert(deliveries.length <= 1, "duplicate process deliveries");
    if (deliveries.length !== 1 || detail.turns.length < 2 || detail.turns.some(turn => ["queued", "running"].includes(turn.state))) return false;
    assert.equal(detail.turns.length, 2);
    assert(detail.turns.every(turn => turn.state === "completed"));
    return detail;
  });
  const delivery = detail.messages.find(message => message.origin?.kind === "process");
  assert.equal(delivery.body, marker);
  assert.equal(delivery.origin.name, "wakeAfterThreeSeconds");
  assert.equal(delivery.origin.result, marker);
  assert.equal(delivery.origin.outcome, "completed");
  assert.equal(delivery.origin.trigger.kind, "timer");
  assert.equal(delivery.origin.trigger.in_secs, 3);
  assert.equal(detail.turns.filter(turn => turn.id !== turnId && turn.owner_message_id === null).length, 1);
  const note = page.locator(`[data-slot="process-message"][data-message-id="${delivery.id}"]`);
  await note.waitFor({ state: "visible" });
  assert.equal(await note.locator('[data-slot="conversation-note"]').count(), 1);
  assert.equal(await note.locator('[data-slot="agent-message"], [data-slot="owner-message"]').count(), 0);
  assert((await note.innerText()).includes("timer · in 3s"));
  assert((await note.innerText()).includes(marker));
  assert(!(await note.innerText()).includes("blake3"));
  const first = await capture("01-process-delivered", context);
  const receipts = sqliteJson(join(dataDir, "hirsel.sqlite"), `SELECT message_id,result,triage_dispatched FROM process_deliveries WHERE thread_id=${threadId}`);
  assert.equal(receipts.length, 1);
  assert.equal(receipts[0].message_id, delivery.id);
  assert.deepEqual(JSON.parse(receipts[0].result), delivery.origin);
  assert.equal(receipts[0].triage_dispatched, 1);
  assert.equal(first.store.messages.filter(message => message.id === delivery.id && message.body === marker).length, 1);
  const live = frames.filter(row => row.direction === "received" && row.frame.type === "msg" && row.frame.message.id === delivery.id);
  assert.equal(live.length, 1);
  assert.deepEqual(live[0].frame.message.origin, delivery.origin);
  await page.reload({ waitUntil: "domcontentloaded" });
  await note.waitFor({ state: "visible" });
  const reloaded = await capture("02-process-reloaded", context);
  assert.deepEqual(reloaded.detail.messages.find(message => message.id === delivery.id), delivery);
  assert.equal(reloaded.detail.turns.length, 2);
  const log = await readFile(join(scenarioDir, "host.log"), "utf8");
  assert(!log.includes("triage fork"), "solicited delivery entered triage");
  return { marker, deliveryId: delivery.id, ownerTurnId: turnId, turnIds: detail.turns.map(turn => turn.id), receiptCount: receipts.length };
}
