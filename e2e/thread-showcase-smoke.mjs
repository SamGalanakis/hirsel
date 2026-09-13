import assert from 'node:assert/strict';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { isolatedUrl, launchBrowser, request as harnessRequest } from './lib/harness.mjs';
const host = isolatedUrl(process.env.HIRSEL_ARTIFACT_HOST_URL, 'HIRSEL_ARTIFACT_HOST_URL');
const base = process.env.HIRSEL_APP_URL ? isolatedUrl(process.env.HIRSEL_APP_URL, 'HIRSEL_APP_URL') : host;
const token = process.env.HIRSEL_ARTIFACT_HOST_TOKEN ?? 'showcase-test';
let history;
function request(frame, expected) { return harnessRequest({url:host,token,frame,expected,includeHistoryId:frame.type==='create_thread',onHello:value=>{history=value.history_id;}}); }
async function publish(threadId, draft) {
 const response = await fetch(`${host}/debug/publish-artifact`, { method: 'POST', headers: { Authorization: `Bearer ${token}`, 'Content-Type': 'application/json' }, body: JSON.stringify({ operation_id: crypto.randomUUID(), thread_id: threadId, draft }) });
 if (!response.ok) throw new Error(await response.text()); return response.json();
}
const browser = await launchBrowser();
const results = [];
try { for (const viewport of [{width:1440,height:900},{width:390,height:844}]) {
 const phone = viewport.width < 1024;
 const thread = (await request({type:'create_thread',parent_thread_id:null,client_id:crypto.randomUUID(),title:`Showcase ${viewport.width} ${Date.now()}`,kind:'space'}, 'thread_created')).thread;
 const artifact = await publish(thread.id, {title:'Working counter',kind:'solid',content:"import {createSignal} from 'solid-js'; export default function App(){const[n,setN]=createSignal(0);return <main style={{padding:'24px'}}><h1>Working result</h1><button onClick={()=>setN(n()+1)}>Count {n()}</button></main>}"});
 const alternateTitle = `Project notes ${viewport.width}`;
 const alternate = await publish(thread.id, {title:alternateTitle,kind:'markdown',content:`# ${alternateTitle}\n\nA persistent reference for this thread.`});
 const page = await browser.newPage({viewport,hasTouch:phone}); const errors=[];
 page.on('pageerror', error => errors.push(error.message));
 await page.addInitScript(token => { if (window === window.top) localStorage.setItem('hirsel.token', token); }, token);
 await page.goto(`${base}/t/${thread.id}?history=${history}`);
 const card = page.locator(`[data-artifact-ref="${artifact.id}"]`).first(); await card.waitFor();
 const composer = page.locator('[data-composer="main"]'); await composer.fill('Keep this draft');
 await card.locator('..').getByRole('button',{name:'Open with'}).click();
 await page.getByRole('menuitem',{name:'Showcase in this thread'}).click();
 const showButton = page.getByRole('button',{name:'Show showcase',exact:true});
 if (phone) await showButton.click();
 const panel = page.locator('[data-slot="thread-showcase"]'); await panel.waitFor({state:'visible'});
 if (phone) assert.equal(await panel.evaluate((node) => node.contains(document.activeElement)),true);
 let preview = page.frameLocator('[data-slot="thread-showcase"] iframe');
 await preview.getByRole('button',{name:'Count 0',exact:true}).click();
 await preview.getByRole('button',{name:'Count 1',exact:true}).waitFor();
 assert.equal(await page.locator('[data-slot="artifact-preview"]').count(),0);
 await page.screenshot({path:join(tmpdir(),`hirsel-showcase-${phone?'phone':'desktop'}.png`)});
 const downloadPromise = page.waitForEvent('download'); await panel.getByRole('button',{name:'Download showcase'}).click();
 const download = await downloadPromise; assert.equal(download.suggestedFilename(),'Working counter.jsx');
 if (phone) { await panel.getByRole('button',{name:'Back to conversation'}).click(); assert.equal(await showButton.evaluate((node) => node === document.activeElement),true); assert.equal(await composer.inputValue(),'Keep this draft'); }
 else { const top = await panel.boundingBox(); await page.locator('[data-slot="thread-scroll"]').evaluate(el=>{el.scrollTop=0}); assert.deepEqual(await panel.boundingBox(),top); }
 await page.locator(`[data-artifact-ref="${alternate.id}"]`).first().click();
 await page.locator('[data-slot="artifact-preview"]').waitFor({state:'visible'});
 assert.equal(await panel.isVisible(),false);
 await page.locator('[data-slot="artifact-preview"]').getByRole('button',{name:'Back to conversation'}).click();
 if (phone) await page.getByRole('button',{name:'Show showcase',exact:true}).click();
 await preview.getByRole('button',{name:'Count 1',exact:true}).waitFor();
 if (phone) await panel.getByRole('button',{name:'Back to conversation'}).click();
 await page.reload(); await page.locator(`[data-artifact-ref="${artifact.id}"]`).first().waitFor();
 assert.equal(await composer.inputValue(),'Keep this draft');
 if (phone) await page.getByRole('button',{name:'Show showcase',exact:true}).click();
 await preview.getByRole('button',{name:'Count 0',exact:true}).waitFor();
 await panel.getByRole('button',{name:'Showcase actions'}).click(); await page.getByRole('menuitem',{name:'Replace showcase'}).click();
 const picker = page.getByRole('dialog',{name:'Choose showcase'}); await picker.locator(`[data-artifact-id="${alternate.id}"]`).click();
 await panel.getByRole('heading',{name:alternateTitle}).waitFor();
 await page.frameLocator('[data-slot="thread-showcase"] iframe').getByRole('heading',{name:alternateTitle}).waitFor();
 await panel.getByRole('button',{name:'Showcase actions'}).click(); await page.getByRole('menuitem',{name:'Remove showcase'}).click();
 await panel.waitFor({state:'hidden'}); assert.equal(await composer.inputValue(),'Keep this draft');
 const detail = await request({type:'open_thread',thread_id:thread.id,client_id:crypto.randomUUID()}, 'thread_opened');
 assert.equal(detail.detail.thread.showcased_artifact_id,null);
 assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth),false); assert.deepEqual(errors,[]);
 results.push({viewport,promote:true,replace:true,remove:true,reload:true,draft:true,previewIndependent:true,download:true,errors}); await page.close();
} console.log(JSON.stringify(results,null,2)); } finally { await browser.close(); }
