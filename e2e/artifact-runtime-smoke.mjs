import { chromium } from '../app/node_modules/playwright/index.mjs';
import assert from 'node:assert/strict';
const base = process.env.HIRSEL_ARTIFACT_TEST_URL;
if (!base || new URL(base).port === '3076') throw new Error('Set HIRSEL_ARTIFACT_TEST_URL to the isolated artifact harness, never the live host.');
const browser = await chromium.launch({ headless:true, executablePath: process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH ?? '/home/sam/.cache/ms-playwright/chromium-1234/chrome-linux64/chrome' });
try {
 const page=await browser.newPage(); const errors=[];page.on('pageerror',e=>errors.push(e.message));
 await page.goto(`${base}/tools/artifact-smoke.html`);
 const preview=page.frameLocator('iframe');
 await preview.getByRole('button',{name:'Count 0',exact:true}).click();
 await preview.getByRole('button',{name:'Count 1',exact:true}).waitFor();
 const sandbox=await page.locator('iframe').getAttribute('sandbox');assert.equal(sandbox,'allow-scripts');
 await page.evaluate(()=>localStorage.setItem('artifact-host-secret','private-host-value'));
 await page.evaluate(()=>window.replaceArtifact(`import { createSignal } from 'solid-js'; export default function App(){const [result,setResult]=createSignal('Check isolation');return <button onClick={async()=>{let parentBlocked=false;try{parent.localStorage.getItem('artifact-host-secret')}catch{parentBlocked=true}let networkBlocked=false;try{await fetch('/api/settings')}catch{networkBlocked=true}const socketBlocked=await new Promise(resolve=>{try{const socket=new WebSocket('wss://example.invalid');socket.onerror=()=>resolve(true);socket.onopen=()=>{socket.close();resolve(false)};setTimeout(()=>resolve(false),1000)}catch{resolve(true)}});setResult(parentBlocked&&networkBlocked&&socketBlocked?'Isolated':'FAILED')}}>{result()}</button>}`));
 await preview.getByRole('button',{name:'Check isolation',exact:true}).click();
 await preview.getByRole('button',{name:'Isolated',exact:true}).waitFor();
 assert.equal(await page.evaluate(()=>localStorage.getItem('artifact-host-secret')),'private-host-value');
 await page.evaluate(()=>window.replaceArtifact(`export default function App(){return <p>Updated current content</p>}`));
 await preview.getByText('Updated current content',{exact:true}).waitFor();
 // Once loaded, interaction remains entirely local and works offline.
 await page.evaluate(()=>window.replaceArtifact(`import {createSignal} from 'solid-js'; export default function App(){const [n,setN]=createSignal(4);return <button onClick={()=>setN(n()+1)}>Offline {n()}</button>}`));
 await preview.getByRole('button',{name:'Offline 4',exact:true}).waitFor();
 await page.context().setOffline(true);
 await preview.getByRole('button',{name:'Offline 4',exact:true}).click();
 await preview.getByRole('button',{name:'Offline 5',exact:true}).waitFor();
 await page.context().setOffline(false);
 const navigationAttempts=[];
 const externalProbe='https://artifact-network-probe.invalid/navigation';
 const sameHostProbe=new URL('/artifact-network-probe',base).href;
 // Intercept locally so a failing assertion never sends artifact content out.
 for(const target of [externalProbe,sameHostProbe]) await page.route(target,route=>{navigationAttempts.push(route.request().url());return route.fulfill({body:'unexpected navigation',contentType:'text/html'})});
 for(const [target,kind] of [[externalProbe,'script'],[sameHostProbe,'script'],[externalProbe,'link'],[sameHostProbe,'link']]) {
  await page.evaluate(({url,kind})=>window.replaceArtifact(kind==='script'
   ? `export default function App(){return <button onClick={()=>{window.location.href=${JSON.stringify(url)}}}>Navigate preview</button>}`
   : `export default function App(){return <a href=${JSON.stringify(url)} target="_self">Navigate preview</a>}`),{url:target,kind});
  await preview.getByRole(kind==='script'?'button':'link',{name:'Navigate preview',exact:true}).click({noWaitAfter:true});
  await page.waitForFunction(()=>document.querySelector('iframe')?.contentWindow !== null);
  // A blocked navigation has no network request; give any deferred request a
  // bounded opportunity to reach the interception handler before asserting.
  await page.waitForTimeout(250);
 }
 assert.deepEqual(navigationAttempts,[]);
 // Return to ordinary local interaction after the blocked navigation.
 await page.evaluate(()=>window.replaceArtifact(`import {createSignal} from 'solid-js'; export default function App(){const [n,setN]=createSignal(0);return <button onClick={()=>setN(n()+1)}>Recovered {n()}</button>}`));
 await preview.getByRole('button',{name:'Recovered 0',exact:true}).click();
 await preview.getByRole('button',{name:'Recovered 1',exact:true}).waitFor();
 assert.deepEqual(errors,[]);
 console.log(JSON.stringify({solid:'2.0.0-rc.7',reactivity:true,editRefresh:true,parentStorageBlocked:true,backendFetchBlocked:true,webSocketBlocked:true,offline:true,selfNavigationBlocked:true,sameHostNavigationBlocked:true,navigationAttempts,sandbox,errors}));
} finally { await browser.close(); }
