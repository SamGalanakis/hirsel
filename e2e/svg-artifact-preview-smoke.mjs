import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { chromium } from "../app/node_modules/playwright/index.mjs";

const base = process.env.HIRSEL_ARTIFACT_TEST_URL;
const database = process.env.HIRSEL_CAT_ARTIFACT_DB;
const screenshot = process.env.HIRSEL_SVG_ARTIFACT_SCREENSHOT ?? "/tmp/hirsel-svg-preview-cat.png";
if (!base || new URL(base).port === "3076") throw new Error("Set HIRSEL_ARTIFACT_TEST_URL to the isolated artifact harness, never the live host.");
if (!database) throw new Error("Set HIRSEL_CAT_ARTIFACT_DB to the SQLite evidence database containing artifact 2.");

const rows = JSON.parse(execFileSync("sqlite3", ["-readonly", "-json", database, "select id,title,mime,filename,content from artifacts where id=2;"], { encoding: "utf8" }));
assert.equal(rows.length, 1);
const stored = rows[0];
assert.equal(stored.title, "Cat picture");
assert.equal(stored.mime, "image/svg+xml");
assert.equal(stored.filename, "cat.svg");
assert.match(stored.content, /<title id="title">A cozy orange cat<\/title>/);
const artifact = { ...stored, kind: "file", thread_ids: [], created_at: "evidence", updated_at: "evidence" };

const browser = await chromium.launch({
  headless: true,
  executablePath: process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH
    ?? "/home/sam/.cache/ms-playwright/chromium-1234/chrome-linux64/chrome",
});
try {
  const page = await browser.newPage({ viewport: { width: 1200, height: 900 } });
  const errors = [];
  const externalRequests = [];
  const externalProbe = "https://artifact-network-probe.invalid/cat.svg";
  const navigationProbe = "https://artifact-network-probe.invalid/navigate";
  page.on("pageerror", error => errors.push(error.message));
  for (const target of [externalProbe, navigationProbe]) {
    await page.route(target, route => {
      externalRequests.push(route.request().url());
      return route.fulfill({ status: 200, body: "unexpected request" });
    });
  }
  await page.goto(`${base}/tools/artifact-smoke.html`);
  await page.evaluate(() => {
    window.svgPreviewMessages = [];
    addEventListener("message", event => window.svgPreviewMessages.push(event.data));
  });
  await page.evaluate(value => window.showArtifact(value), artifact);
  const preview = page.frameLocator("iframe");
  const image = preview.locator('img[alt="Cat picture"]');
  await image.waitFor();
  assert.deepEqual(await image.evaluate(node => ({ width: node.naturalWidth, height: node.naturalHeight })), { width: 800, height: 800 });
  assert.equal(await page.locator("iframe").getAttribute("sandbox"), "allow-scripts");
  assert.match(await preview.locator('meta[http-equiv="Content-Security-Policy"]').getAttribute("content"), /img-src data: blob:/);
  const source = await image.getAttribute("src");
  assert.ok(source?.startsWith("data:image/svg+xml;charset=utf-8,"));
  assert.equal(decodeURIComponent(source.slice(source.indexOf(",") + 1)), stored.content);
  await image.screenshot({ path: screenshot });

  const hostile = stored.content.replace("</svg>", `<script>parent.postMessage('svg-script-executed','*');document.title='executed'</script><image href="${externalProbe}" width="800" height="800"/><a href="${navigationProbe}"><rect width="800" height="800" fill="transparent"/></a></svg>`);
  await page.evaluate(value => window.showArtifact(value), { ...artifact, content: hostile });
  await image.waitFor();
  assert.equal(decodeURIComponent((await image.getAttribute("src")).split(",", 2)[1]), hostile);
  const beforeClick = page.url();
  const frameBox = await page.locator("iframe").boundingBox();
  assert.ok(frameBox);
  await page.mouse.click(frameBox.x + frameBox.width / 2, frameBox.y + frameBox.height / 2);
  await page.waitForTimeout(500);
  assert.equal(page.url(), beforeClick);
  assert.deepEqual(await page.evaluate(() => window.svgPreviewMessages), []);
  assert.deepEqual(externalRequests, []);
  assert.deepEqual(errors, []);
  console.log(JSON.stringify({
    artifactId: stored.id,
    naturalSize: [800, 800],
    sourceBytesPreserved: true,
    svgScriptExecuted: false,
    externalRequests,
    navigationBlocked: true,
    sandbox: "allow-scripts",
    screenshot,
    errors,
  }));
} finally {
  await browser.close();
}
