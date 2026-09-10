import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { chromium } from "../app/node_modules/playwright/index.mjs";
import { WebSocket } from "../app/node_modules/ws/wrapper.mjs";

const host = process.env.HIRSEL_ARTIFACT_HOST_URL;
const token = process.env.HIRSEL_ARTIFACT_HOST_TOKEN;
const database = process.env.HIRSEL_ARTIFACT_DB;
const evidenceDir = process.env.HIRSEL_ARTIFACT_EVIDENCE;
const threadId = Number(process.env.HIRSEL_ARTIFACT_THREAD_ID ?? "1");
const artifactId = Number(process.env.HIRSEL_ARTIFACT_ID ?? "2");
if (!host || new URL(host).port === "3076") throw new Error("Set HIRSEL_ARTIFACT_HOST_URL to an isolated Host, never the live Host.");
if (!token || !database || !evidenceDir) throw new Error("Set HIRSEL_ARTIFACT_HOST_TOKEN, HIRSEL_ARTIFACT_DB, and HIRSEL_ARTIFACT_EVIDENCE.");
await mkdir(evidenceDir, { recursive: true });

function sqliteJson(sql) {
  const output = execFileSync("sqlite3", ["-readonly", "-json", database, sql], { encoding: "utf8" }).trim();
  return output ? JSON.parse(output) : [];
}

const [stored] = sqliteJson(`select id,title,json_extract(kind,'$') as kind,mime,filename,content from artifacts where id=${artifactId}`);
assert(stored, `artifact ${artifactId} is absent from the evidence database`);
assert.equal(stored.kind, "file");
assert.equal(stored.mime, "image/svg+xml");
assert.match(stored.content, /<svg\b/i);

async function hello() {
  return new Promise((resolve, reject) => {
    const socket = new WebSocket(`${host.replace(/^http/, "ws")}/ws`);
    const timer = setTimeout(() => { socket.close(); reject(new Error("hello_ok timed out")); }, 10_000);
    socket.on("error", reject);
    socket.on("open", () => socket.send(JSON.stringify({ type: "hello", auth: { static_token: token } })));
    socket.on("message", raw => {
      const frame = JSON.parse(raw.toString());
      if (frame.type === "hello_ok") {
        clearTimeout(timer);
        socket.close();
        resolve(frame);
      }
    });
  });
}

const greeting = await hello();

async function assertContainedImage(panel) {
  const image = panel.frameLocator("iframe").locator(`img[alt="${stored.title}"]`);
  await image.waitFor({ state: "visible" });
  assert.deepEqual(await image.evaluate(node => ({ width: node.naturalWidth, height: node.naturalHeight })), { width: 800, height: 800 });
  const box = await image.boundingBox();
  const frameBox = await panel.locator("iframe").boundingBox();
  assert(box && frameBox, "rendered SVG has no visible geometry");
  assert(box.width <= frameBox.width && box.height <= frameBox.height, "rendered SVG is clipped by its preview frame");
}

async function assertControlsFit(page, panel) {
  const metrics = await panel.evaluate(node => {
    const controls = [...node.querySelectorAll("header button")].map(button => {
      const box = button.getBoundingClientRect();
      return { label: button.getAttribute("aria-label") ?? button.textContent?.trim(), left: box.left, right: box.right, top: box.top, bottom: box.bottom };
    });
    const box = node.getBoundingClientRect();
    return {
      pageOverflow: document.documentElement.scrollWidth > window.innerWidth,
      panelOverflow: node.scrollWidth > node.clientWidth,
      panel: { left: box.left, right: box.right, top: box.top, bottom: box.bottom },
      viewport: { width: window.innerWidth, height: window.innerHeight },
      controls,
    };
  });
  assert.equal(metrics.pageOverflow, false, "page has horizontal overflow");
  assert.equal(metrics.panelOverflow, false, "artifact panel has horizontal overflow");
  for (const control of metrics.controls) {
    assert(control.left >= 0 && control.right <= metrics.viewport.width, `${control.label} is outside the viewport horizontally`);
    assert(control.top >= 0 && control.bottom <= metrics.viewport.height, `${control.label} is outside the viewport vertically`);
  }
  return metrics;
}

async function exerciseModes(page, panel, label, downloadName) {
  const rendered = panel.getByRole("button", { name: "Rendered", exact: true });
  const source = panel.getByRole("button", { name: "Source", exact: true });
  await rendered.waitFor({ state: "visible" });
  assert.equal(await rendered.getAttribute("aria-pressed"), "true");
  assert.equal(await source.getAttribute("aria-pressed"), "false");
  await assertContainedImage(panel);
  await page.screenshot({ path: join(evidenceDir, `${label}-rendered.png`), fullPage: true });

  await source.focus();
  await source.press("Enter");
  assert.equal(await source.getAttribute("aria-pressed"), "true");
  assert.equal(await source.evaluate(node => node === document.activeElement), true, "Source lost keyboard focus after activation");
  const sourceView = panel.locator('[data-slot="artifact-source"]');
  await sourceView.waitFor({ state: "visible" });
  assert.equal(await sourceView.textContent(), stored.content, "Source differs from exact stored SVG bytes");
  assert.equal(await panel.locator("iframe").count(), 0, "Source mode mounted a renderer iframe");
  assert.equal(await sourceView.locator("svg,script,img,canvas").count(), 0, "Source mode interpreted SVG markup");
  await page.screenshot({ path: join(evidenceDir, `${label}-source.png`), fullPage: true });

  await rendered.focus();
  await rendered.press("Enter");
  assert.equal(await rendered.getAttribute("aria-pressed"), "true");
  assert.equal(await rendered.evaluate(node => node === document.activeElement), true, "Rendered lost keyboard focus after activation");
  await assertContainedImage(panel);

  const downloadPromise = page.waitForEvent("download");
  await panel.getByRole("button", { name: downloadName, exact: true }).click();
  const download = await downloadPromise;
  assert.equal(download.suggestedFilename(), stored.filename);
  const downloadPath = join(evidenceDir, `${label}-${stored.filename}`);
  await download.saveAs(downloadPath);
  assert.deepEqual(await readFile(downloadPath), Buffer.from(stored.content), "download differs from stored artifact bytes");
  return { controls: await assertControlsFit(page, panel), downloadPath };
}

const browser = await chromium.launch({
  headless: true,
  executablePath: process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH
    ?? "/home/sam/.cache/ms-playwright/chromium_headless_shell-1234/chrome-headless-shell-linux64/chrome-headless-shell",
});
const results = [];
try {
  for (const viewport of [{ width: 1440, height: 900 }, { width: 390, height: 844 }]) {
    const phone = viewport.width < 1024;
    const page = await browser.newPage({ viewport, hasTouch: phone });
    const errors = [];
    const frames = [];
    page.on("pageerror", error => errors.push({ type: "pageerror", message: error.message }));
    page.on("console", message => { if (message.type() === "error") errors.push({ type: "console", message: message.text() }); });
    page.on("websocket", socket => {
      socket.on("framesent", event => {
        try {
          const frame = JSON.parse(event.payload.toString());
          frames.push(frame.type === "hello" ? { direction: "sent", frame: { type: "hello", auth: "<redacted>" } } : { direction: "sent", frame });
        } catch { /* non-JSON frame */ }
      });
      socket.on("framereceived", event => {
        try { frames.push({ direction: "received", frame: JSON.parse(event.payload.toString()) }); }
        catch { /* non-JSON frame */ }
      });
    });
    await page.addInitScript(value => { if (window === window.top) localStorage.setItem("hirsel.token", value); }, token);
    await page.goto(`${host}/t/${threadId}?history=${greeting.history_id}`, { waitUntil: "domcontentloaded" });
    const card = page.locator(`[data-artifact-ref="${artifactId}"]`).first();
    await card.waitFor({ state: "visible" });
    await card.click();
    let panel = page.locator('[data-slot="artifact-preview"]');
    await panel.waitFor({ state: "visible" });
    const preview = await exerciseModes(page, panel, `${phone ? "phone" : "desktop"}-preview`, "Download");
    await panel.getByRole("button", { name: "Back to conversation", exact: true }).click();

    await page.reload({ waitUntil: "domcontentloaded" });
    await card.waitFor({ state: "visible" });
    await card.click();
    panel = page.locator('[data-slot="artifact-preview"]');
    await panel.waitFor({ state: "visible" });
    assert.equal(await panel.getByRole("button", { name: "Rendered", exact: true }).getAttribute("aria-pressed"), "true", "reopened viewer did not reset to Rendered");
    await assertContainedImage(panel);
    await page.screenshot({ path: join(evidenceDir, `${phone ? "phone" : "desktop"}-preview-reloaded.png`), fullPage: true });
    await panel.getByRole("button", { name: "Back to conversation", exact: true }).click();

    const actions = card.locator("..").getByRole("button", { name: "Artifact actions", exact: true });
    if (!phone) {
      await actions.click();
      await page.getByRole("menuitem", { name: "Showcase in this thread", exact: true }).click();
    } else {
      await page.getByRole("button", { name: "Show showcase", exact: true }).click();
    }
    panel = page.locator('[data-slot="thread-showcase"]');
    await panel.waitFor({ state: "visible" });
    const showcase = await exerciseModes(page, panel, `${phone ? "phone" : "desktop"}-showcase`, "Download showcase");
    if (phone) await panel.getByRole("button", { name: "Back to conversation", exact: true }).click();

    await page.reload({ waitUntil: "domcontentloaded" });
    await card.waitFor({ state: "visible" });
    if (phone) await page.getByRole("button", { name: "Show showcase", exact: true }).click();
    panel = page.locator('[data-slot="thread-showcase"]');
    await panel.waitFor({ state: "visible" });
    assert.equal(await panel.getByRole("button", { name: "Rendered", exact: true }).getAttribute("aria-pressed"), "true", "reloaded showcase did not reset to Rendered");
    await assertContainedImage(panel);
    await page.screenshot({ path: join(evidenceDir, `${phone ? "phone" : "desktop"}-showcase-reloaded.png`), fullPage: true });
    assert.deepEqual(errors, [], `browser errors: ${JSON.stringify(errors)}`);
    await writeFile(join(evidenceDir, `${phone ? "phone" : "desktop"}-frames.ndjson`), `${frames.map(row => JSON.stringify(row)).join("\n")}\n`);
    results.push({ viewport, preview, showcase, errors });
    await page.close();
  }
  await writeFile(join(evidenceDir, "result.json"), `${JSON.stringify({ artifact: { ...stored, content: `<${stored.content.length} exact bytes>` }, results }, null, 2)}\n`);
  console.log(JSON.stringify({ artifactId, title: stored.title, mime: stored.mime, sourceBytes: stored.content.length, viewports: results.map(result => result.viewport), objectiveStatus: "OBJECTIVE_PASS" }));
} finally {
  await browser.close();
}
