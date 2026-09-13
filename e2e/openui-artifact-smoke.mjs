import assert from 'node:assert/strict';
import { mkdir } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { isolatedUrl, launchBrowser, poll, request as harnessRequest } from './lib/harness.mjs';
// An `openui` artifact published through the same path the agent tool uses is
// drawn natively — no frame — in the preview and the showcase, its controls
// send one ordinary Owner message to the Thread, and both themes are
// photographed at desktop width.
const host = isolatedUrl(process.env.HIRSEL_ARTIFACT_HOST_URL, 'HIRSEL_ARTIFACT_HOST_URL');
const base = process.env.HIRSEL_APP_URL ? isolatedUrl(process.env.HIRSEL_APP_URL, 'HIRSEL_APP_URL') : host;
const token = process.env.HIRSEL_ARTIFACT_HOST_TOKEN ?? 'openui-test';
const shots = process.env.HIRSEL_OPENUI_SHOTS ?? join(tmpdir(), 'hirsel-openui-shots');
await mkdir(shots, { recursive: true });
let history;
function request(frame, expected) { return harnessRequest({url:host,token,frame,expected,includeHistoryId:frame.type==='create_thread',onHello:value=>{history=value.history_id;}}); }
async function publish(threadId, draft) {
 const response = await fetch(`${host}/debug/publish-artifact`, { method: 'POST', headers: { Authorization: `Bearer ${token}`, 'Content-Type': 'application/json' }, body: JSON.stringify({ operation_id: crypto.randomUUID(), thread_id: threadId, draft }) });
 if (!response.ok) throw new Error(await response.text()); return response.json();
}
export const DASHBOARD = `root = Stack([band, detail, ask], "col", "lg")
band = Stack([opened, replied, bounced, dropped], "row", "md", "stretch")
opened = Metric("Open rate", "38%", "+4pt", "up")
replied = Metric("Replies", "112", "-9", "down")
bounced = Metric("Bounces", "1.2%", "flat", "flat")
dropped = Metric("Unsubscribed", "6", "+2", "down")
detail = Stack([bySegment, weekly], "row", "lg", "start")
bySegment = Section("By segment", [segments, ghost], "Last seven days")
segments = Table(["Segment", "Sent", "Opened", "Replied"], [["Trial", "820", "310", "41"], ["Paid", "1,240", "470", "58"], ["Churned", "96", "12", "1"]])
ghost = Phantom("not in the library")
weekly = Section("Opens by day", [chart, note])
chart = Chart("bar", ["Mon", "Tue", "Wed", "Thu", "Fri"], [120, 210, 175, 260, 190], "Opens")
note = Callout("Thursday's spike is the product update.", "info")
ask = Form("narrow", [segment, floor], [apply, reset])
segment = Select("segment", ["Trial", "Paid", "Churned"], "Segment", "All segments", ["required"])
floor = Slider("floor", "Minimum opens", 0, 500, 10, 100)
apply = Button("Apply filter", "narrow", "primary")
reset = Button("Reset", "reset", "secondary")`;
const browser = await launchBrowser();
const results = [];
try {
 const thread = (await request({type:'create_thread',parent_thread_id:null,client_id:crypto.randomUUID(),title:`OpenUI ${Date.now()}`,kind:'space'}, 'thread_created')).thread;
 const artifact = await publish(thread.id, {title:'Campaign board',kind:'openui',content:DASHBOARD});
 assert.equal(artifact.kind, 'openui');
 for (const theme of ['dark', 'light']) {
  const page = await browser.newPage({viewport:{width:1440,height:900}}); const errors=[];
  page.on('pageerror', error => errors.push(error.message));
  await page.addInitScript(([token, theme]) => { if (window === window.top) { localStorage.setItem('hirsel.token', token); localStorage.setItem('hirsel.theme', theme); } }, [token, theme]);
  await page.goto(`${base}/t/${thread.id}?history=${history}`);
  assert.equal(await page.evaluate(() => document.documentElement.classList.contains('dark')), theme === 'dark');
  const card = page.locator(`[data-artifact-ref="${artifact.id}"]`).first(); await card.waitFor();
  assert.equal(await card.getAttribute('title'), 'Open interactive artifact');
  // Preview: native drawing, no frame, the parts of the dashboard present.
  await card.click();
  const preview = page.locator('[data-slot="artifact-preview"]'); await preview.waitFor({state:'visible'});
  const drawing = preview.locator('[data-slot="openui-root"]'); await drawing.waitFor();
  assert.equal(await preview.locator('iframe').count(), 0);
  await drawing.getByText('38%', {exact:true}).waitFor();
  assert.equal(await drawing.getByRole('table').count(), 1);
  await drawing.getByRole('img', {name:/Opens chart/}).waitFor();
  await drawing.getByRole('form', {name:'narrow'}).waitFor();
  // The line the library cannot use is dropped and counted, never fatal.
  await drawing.getByRole('button', {name:/1 line was dropped/}).waitFor();
  assert.equal(await drawing.getByText('not in the library').count(), 0);
  await page.screenshot({path: join(shots, `openui-dashboard-${theme}.png`)});
  await preview.getByRole('button', {name:'Back to conversation', exact:true}).click();
  await preview.waitFor({state:'detached'});
  // Showcase: the same native drawing in the Thread's own panel.
  await card.locator('..').getByRole('button', {name:'Open with'}).click();
  await page.getByRole('menuitem', {name:'Showcase in this thread'}).click();
  const panel = page.locator('[data-slot="thread-showcase"]'); await panel.waitFor({state:'visible'});
  const showcase = panel.locator('[data-slot="openui-root"]'); await showcase.waitFor();
  assert.equal(await panel.locator('iframe').count(), 0);
  await showcase.getByRole('form', {name:'narrow'}).waitFor();
  // A required field holds the primary action back; a filled form sends one
  // Owner message carrying the label and the fenced payload.
  const messagesBefore = (await request({type:'open_thread',thread_id:thread.id,client_id:crypto.randomUUID()}, 'thread_opened')).detail.messages.filter(m => m.author === 'owner').length;
  await showcase.getByRole('button', {name:'Apply filter', exact:true}).click();
  await showcase.getByRole('alert').waitFor();
  await showcase.getByLabel('Segment').selectOption('Paid');
  await showcase.getByRole('button', {name:'Apply filter', exact:true}).click();
  const sent = await poll('owner action message', async () => {
   const detail = (await request({type:'open_thread',thread_id:thread.id,client_id:crypto.randomUUID()}, 'thread_opened')).detail;
   const owned = detail.messages.filter(m => m.author === 'owner');
   return owned.length > messagesBefore ? owned.at(-1) : null;
  }, 20_000);
  assert.ok(sent.body.startsWith('Apply filter\n\n```json\n'), sent.body);
  const payload = JSON.parse(sent.body.slice(sent.body.indexOf('{'), sent.body.lastIndexOf('}') + 1));
  assert.deepEqual(payload, {artifact_id: artifact.id, action: 'narrow', params: {}, form_state: {narrow: {segment: {value: 'Paid', componentType: 'Select'}, floor: {value: 100, componentType: 'Slider'}}}});
  assert.deepEqual(sent.artifact_ids, [artifact.id]);
  await page.locator(`[data-slot="thread-scroll"] article[data-author="owner"]`).filter({hasText:'Apply filter'}).first().waitFor();
  // Source stays inert text; the download keeps the kind's own name.
  await panel.getByRole('button', {name:'Showcase actions'}).click();
  await page.getByRole('menuitem', {name:'Remove showcase'}).click();
  await panel.waitFor({state:'hidden'});
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
  assert.deepEqual(errors, []);
  results.push({theme, artifactId: artifact.id, threadId: thread.id, native: true, dropped: 1, action: payload.action, screenshot: join(shots, `openui-dashboard-${theme}.png`), errors});
  await page.close();
 }
 console.log(JSON.stringify(results, null, 2));
} finally { await browser.close(); }
