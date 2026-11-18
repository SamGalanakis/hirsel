import assert from 'node:assert/strict';
import {execFile} from 'node:child_process';
import {mkdtemp, writeFile, rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {pathToFileURL} from 'node:url';
import {promisify} from 'node:util';

const execute = promisify(execFile);

export async function openBrowser(origin) {
  const work = await mkdtemp(join(tmpdir(), 'checkbox-playwright-'));
  const session = `-s=checkbox-${process.pid}`;
  const run = async (...args) => {
    const {stdout} = await execute('playwright-cli', [session, ...args], {cwd: work});
    assert(!/^### Error/m.test(stdout), stdout);
    return stdout;
  };
  async function close() {
    try { await run('close'); }
    finally { await rm(work, {recursive: true, force: true}); }
  }
  try {
    await run('open', origin, '--browser=chromium');
  } catch (error) {
    await close().catch(() => {});
    throw error;
  }
  return {
    close,
    async check(callback, args = {}) {
      const code = join(work, 'check.js');
      await writeFile(code, `async page => { await (${callback})(page, ${JSON.stringify(args)}); return "CHECKBOX_PASS"; }`);
      assert.match(await run('run-code', '--filename=' + code), /### Result\s+"CHECKBOX_PASS"/);
    },
  };
}

export async function pageClick(page, {observer: mirror = false}) {
  const observer = mirror ? await page.context().newPage() : page;
  await page.setViewportSize({width: 1200, height: 1100});
  if (mirror) await observer.goto(page.url());
  for (const active of new Set([page, observer])) {
    await active.getByText('Click a pixel to toggle it.', {exact: false}).waitFor();
  }
  const isChecked = () => {
    const ctx = document.querySelector('canvas').getContext('2d');
    const color = ctx.getImageData(3, 0, 1, 1).data;
    return color[0] === 158 && color[1] === 237 && color[2] === 195;
  };
  if (await observer.evaluate(isChecked)) throw Error('pixel (3, 0) should start unchecked');
  const tick = observer.waitForFunction(isChecked, undefined, {timeout: 1000});
  await page.locator('canvas').click({position: {x: 3, y: 0}});
  await tick;
  await observer.getByText('1 / 1,000,000', {exact: true}).waitFor();
}

export async function canvasMatches(page, {bitmap}) {
  // Build the oracle from readback, with an independent byte/bit loop.
  await page.waitForFunction(expected => {
    const canvas = document.querySelector('canvas');
    const rgba = canvas.getContext('2d').getImageData(0, 0, 1000, 1000).data;
    const bits = atob(expected);
    for (let offset = 0; offset < bits.length; offset++) {
      const mask = bits.charCodeAt(offset);
      for (let j = 0; j < 8; j++) {
        const at = (offset * 8 + j) * 4;
        const checked = Boolean(mask & (1 << j));
        if (rgba[at] !== (checked ? 158 : 29) ||
            rgba[at + 1] !== (checked ? 237 : 42) ||
            rgba[at + 2] !== (checked ? 195 : 43) || rgba[at + 3] !== 255) return false;
      }
    }
    return true;
  }, bitmap, {polling: 25});
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const [name, origin] = process.argv.slice(2);
  assert(['canvas_click', 'browser_ticks'].includes(name), `unknown law: ${name}`);
  const browser = await openBrowser(origin);
  try {
    await browser.check(pageClick, {observer: name === 'browser_ticks'});
    console.log(`PASS ${name}`);
  } finally {
    await browser.close();
  }
}
