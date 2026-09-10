import assert from "node:assert/strict";
import { execFileSync, spawn } from "node:child_process";
import { createWriteStream } from "node:fs";
import { cp, mkdir, writeFile } from "node:fs/promises";
import { createServer } from "node:net";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { chromium } from "../app/node_modules/playwright/index.mjs";

const repo = fileURLToPath(new URL("..", import.meta.url));
const fixture = process.env.HIRSEL_RESPONSIVE_FIXTURE;
assert(fixture, "Set HIRSEL_RESPONSIVE_FIXTURE to a saved isolated runbook state directory.");
const runId = `${new Date().toISOString().replaceAll(/[:.]/g, "-")}-${process.pid}`;
const evidenceDir = process.env.HIRSEL_RESPONSIVE_EVIDENCE ?? join("/tmp", `hirsel-responsive-${runId}`);
const dataDir = join(evidenceDir, "state");
await mkdir(evidenceDir, { recursive: true });
await cp(fixture, dataDir, { recursive: true, force: false, errorOnExist: true });

function cargoTargetDirectory() {
  return JSON.parse(execFileSync("cargo", ["metadata", "--no-deps", "--format-version", "1"], { cwd: repo, encoding: "utf8" })).target_directory;
}

function sqliteJson(sql) {
  const output = execFileSync("sqlite3", ["-json", join(dataDir, "hirsel.sqlite"), sql], { encoding: "utf8" }).trim();
  return output ? JSON.parse(output) : [];
}

async function unusedPort() {
  const server = createServer();
  await new Promise((resolve, reject) => { server.once("error", reject); server.listen(0, "127.0.0.1", resolve); });
  const address = server.address();
  assert(address && typeof address !== "string");
  assert.notEqual(address.port, 3076);
  await new Promise((resolve, reject) => server.close(error => error ? reject(error) : resolve()));
  return address.port;
}

async function poll(label, predicate, timeoutMs = 60_000) {
  const deadline = Date.now() + timeoutMs;
  let lastError;
  while (Date.now() < deadline) {
    try {
      const value = await predicate();
      if (value) return value;
    } catch (error) { lastError = error; }
    await new Promise(resolve => setTimeout(resolve, 150));
  }
  throw new Error(`${label} timed out${lastError ? `: ${lastError.message}` : ""}`);
}

async function stopProcess(child) {
  if (!child || child.exitCode !== null || child.signalCode !== null) return;
  try { process.kill(-child.pid, "SIGTERM"); } catch { return; }
  await Promise.race([new Promise(resolve => child.once("exit", resolve)), new Promise(resolve => setTimeout(resolve, 5_000))]);
  if (child.exitCode === null && child.signalCode === null) {
    try { process.kill(-child.pid, "SIGKILL"); } catch { /* already stopped */ }
  }
}

const [thread] = sqliteJson("SELECT id,title FROM threads ORDER BY id LIMIT 1");
const [artifact] = sqliteJson("SELECT id,title,mime,length(content) AS bytes FROM artifacts WHERE mime='image/svg+xml' ORDER BY id LIMIT 1");
assert(thread && artifact, "Fixture must contain a Thread and a saved SVG artifact.");
const [{ base: nestingBase }] = sqliteJson("SELECT COALESCE(MAX(id), 0) + 100 AS base FROM threads");
const ancestorIds = [0, 1, 2, 3].map(offset => nestingBase + offset);
execFileSync("sqlite3", [join(dataDir, "hirsel.sqlite"), `
  PRAGMA foreign_keys=ON;
  BEGIN;
  INSERT INTO threads(id,parent_thread_id,title,description,instrument,attention,read,created_at,updated_at,revision)
  VALUES
    (${ancestorIds[0]},NULL,'Responsive parent A','','{}','quiet',1,'2026-09-10T20:00:00Z','2026-09-10T20:00:00Z',1),
    (${ancestorIds[1]},${ancestorIds[0]},'Responsive parent B','','{}','quiet',1,'2026-09-10T20:00:00Z','2026-09-10T20:00:00Z',1),
    (${ancestorIds[2]},${ancestorIds[1]},'Responsive parent C','','{}','quiet',1,'2026-09-10T20:00:00Z','2026-09-10T20:00:00Z',1),
    (${ancestorIds[3]},${ancestorIds[2]},'Responsive parent D','','{}','quiet',1,'2026-09-10T20:00:00Z','2026-09-10T20:00:00Z',1);
  UPDATE threads SET parent_thread_id=${ancestorIds[3]} WHERE id=${thread.id};
  COMMIT;
`]);
const token = `responsive-${crypto.randomUUID()}`;
const port = await unusedPort();
const url = `http://127.0.0.1:${port}`;
const log = createWriteStream(join(evidenceDir, "host.log"));
const host = spawn(join(cargoTargetDirectory(), "debug", "hirsel-host"), [], {
  cwd: repo,
  detached: true,
  stdio: ["ignore", "pipe", "pipe"],
  env: {
    ...process.env,
    HIRSEL_TOKEN: token,
    HIRSEL_AGENT: "scripted",
    HIRSEL_DRIVER: "fake",
    HIRSEL_PROVIDER: "anthropic",
    HIRSEL_DEBUG: "1",
    HIRSEL_IROH: "0",
    HIRSEL_DATA_DIR: dataDir,
    HIRSEL_CONFIG: join(dataDir, "hirsel.toml"),
    HIRSEL_TEMPLATES_DIR: join(repo, "templates"),
    HIRSEL_APP_DIR: join(repo, "app", "dist"),
    HIRSEL_LISTEN: `127.0.0.1:${port}`,
  },
});
host.stdout.pipe(log, { end: false });
host.stderr.pipe(log, { end: false });

let browser;
const browserErrors = [];
const captures = [];

async function layoutMetrics(page) {
  return page.evaluate(() => {
    const bounds = selector => {
      const node = document.querySelector(selector);
      if (!(node instanceof HTMLElement) || !node.checkVisibility()) return null;
      const box = node.getBoundingClientRect();
      return { left: box.left, right: box.right, top: box.top, bottom: box.bottom, width: box.width, height: box.height };
    };
    const status = [...document.querySelectorAll('[data-slot="thread-status-primary"]')].filter(node => node.checkVisibility()).map(node => {
      const style = getComputedStyle(node);
      const box = node.getBoundingClientRect();
      const entry = node.closest('[data-thread-entry]');
      const inventory = node.closest('nav');
      const actions = entry?.querySelector('[data-thread-actions]')?.parentElement;
      const entryBox = entry?.getBoundingClientRect();
      const inventoryBox = inventory?.getBoundingClientRect();
      const actionsBox = actions?.getBoundingClientRect();
      const actionButtons = actions ? [...actions.querySelectorAll('button')].filter(action => action.checkVisibility()) : [];
      const overlapsActions = !!actionsBox && box.left < actionsBox.right && box.right > actionsBox.left && box.top < actionsBox.bottom && box.bottom > actionsBox.top;
      return {
        text: node.textContent?.trim(), whiteSpace: style.whiteSpace, height: box.height,
        lineHeight: Number.parseFloat(style.lineHeight), clipped: node.scrollWidth > node.clientWidth,
        bounds: { left: box.left, right: box.right, width: box.width },
        entryBounds: entryBox ? { left: entryBox.left, right: entryBox.right, width: entryBox.width } : null,
        inventoryBounds: inventoryBox ? { left: inventoryBox.left, right: inventoryBox.right, width: inventoryBox.width } : null,
        clippedByEntry: !entryBox || box.left < entryBox.left - 0.5 || box.right > entryBox.right + 0.5,
        clippedByInventory: !inventoryBox || box.left < inventoryBox.left - 0.5 || box.right > inventoryBox.right + 0.5,
        overlapsActions, actionsReachable: actionButtons.every(action => {
          const actionBox = action.getBoundingClientRect();
          return action.contains(document.elementFromPoint(actionBox.x + actionBox.width / 2, actionBox.y + actionBox.height / 2));
        }),
      };
    });
    const inventory = document.querySelector('nav[aria-label="Thread inventory"]');
    return {
      viewport: { width: window.innerWidth, height: window.innerHeight, devicePixelRatio: window.devicePixelRatio },
      pageOverflow: document.documentElement.scrollWidth > window.innerWidth,
      rail: bounds('[data-slot="icon-rail"]'),
      drawer: bounds('[data-slot="thread-drawer"]'),
      conversation: bounds('[data-thread-id]'),
      overview: bounds('[data-slot="thread-empty"]'),
      artifact: bounds('[data-slot="artifact-preview"]'),
      artifactRole: document.querySelector('[data-slot="artifact-preview"]')?.getAttribute("role"),
      drawerRole: document.querySelector('[data-slot="thread-drawer"]')?.getAttribute("role"),
      inventory: inventory ? { scrollWidth: inventory.scrollWidth, clientWidth: inventory.clientWidth, scrollLeft: inventory.scrollLeft } : null,
      status,
      focusedThreadId: document.querySelector('[data-thread-id]')?.getAttribute("data-thread-id") ?? null,
      artifactTitle: document.querySelector('[data-slot="artifact-preview"] h2')?.textContent?.trim() ?? null,
    };
  });
}

function assertStatuses(metrics, width) {
  if (metrics.inventory) {
    assert(metrics.inventory.scrollWidth <= metrics.inventory.clientWidth, `${width}: Thread inventory scrolls horizontally`);
    assert.equal(metrics.inventory.scrollLeft, 0, `${width}: Thread inventory shifted horizontally`);
  }
  for (const row of metrics.status) {
    assert.equal(row.whiteSpace, "nowrap", `${width}: status ${row.text} can wrap`);
    assert.equal(row.clipped, false, `${width}: status ${row.text} is clipped`);
    assert.equal(row.clippedByEntry, false, `${width}: status ${row.text} escapes its Thread row`);
    assert.equal(row.clippedByInventory, false, `${width}: status ${row.text} escapes the Thread inventory`);
    assert.equal(row.overlapsActions, false, `${width}: status ${row.text} overlaps Thread actions`);
    assert.equal(row.actionsReachable, true, `${width}: Thread actions beside ${row.text} are unreachable`);
    assert(row.height <= row.lineHeight * 1.5, `${width}: status ${row.text} spans multiple lines`);
  }
}

function assertContained(metrics, width, selected) {
  assert.equal(metrics.viewport.width, width);
  assert.equal(metrics.pageOverflow, false, `${width}: page overflows horizontally`);
  assert(metrics.rail && metrics.artifact, `${width}: missing rail or artifact`);
  const center = selected ? metrics.conversation : metrics.overview;
  assert(center, `${width}: missing center workspace`);
  for (const [name, box] of [["rail", metrics.rail], ["center", center], ["artifact", metrics.artifact]]) {
    assert(box.left >= 0 && box.right <= width + 0.5, `${width}: ${name} escapes viewport`);
  }
  if (width >= 1280) {
    assert(metrics.drawer, `${width}: default Thread dock is absent`);
    assert.equal(metrics.drawerRole, "complementary");
    assert(metrics.drawer.right <= center.left + 0.5, `${width}: Thread dock covers center workspace`);
    assert(center.right <= metrics.artifact.left + 0.5, `${width}: artifact covers center workspace`);
    assert(center.width >= 470, `${width}: conversation/overview is too narrow (${center.width}px)`);
    assert(metrics.artifact.width >= 320 && metrics.artifact.width <= 672.5, `${width}: artifact width ${metrics.artifact.width}px is outside its useful bounds`);
  } else if (width >= 1024) {
    assert.equal(metrics.drawer, null, `${width}: Thread drawer should stay dismissed until summoned`);
    assert.equal(metrics.artifactRole, "complementary");
    assert(center.right <= metrics.artifact.left + 0.5, `${width}: artifact covers center workspace`);
  } else {
    assert.equal(metrics.drawer, null, `${width}: Thread drawer should stay dismissed until summoned`);
    assert.equal(metrics.artifactRole, "dialog");
    assert(Math.abs(metrics.artifact.width - width) < 1, `${width}: artifact is not a full-width pane`);
  }
  assertStatuses(metrics, width);
}

async function capture(page, width, label, selected) {
  await page.setViewportSize({ width, height: width <= 768 ? 844 : 900 });
  const metrics = await poll(`${label} ${width} responsive layout`, async () => {
    const current = await layoutMetrics(page);
    assertContained(current, width, selected);
    return current;
  }, 5_000);
  await page.screenshot({ path: join(evidenceDir, `${label}-${width}.png`), fullPage: true });
  captures.push({ label, ...metrics });
  return metrics;
}

async function expandNestedThread(page) {
  for (const name of ["Responsive parent A", "Responsive parent B", "Responsive parent C", "Responsive parent D"]) {
    const toggle = page.getByRole("button", { name: new RegExp(`^(Expand|Collapse) ${name}$`) });
    await toggle.waitFor({ state: "visible" });
    if (await toggle.getAttribute("aria-expanded") === "false") await toggle.click();
  }
}

try {
  await poll("isolated Host readiness", async () => {
    if (host.exitCode !== null || host.signalCode !== null) throw new Error(`Host exited (${host.exitCode ?? host.signalCode})`);
    return (await fetch(`${url}/readyz`)).ok;
  });
  browser = await chromium.launch({ headless: true, executablePath: process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH ?? "/home/sam/.cache/ms-playwright/chromium_headless_shell-1234/chrome-headless-shell-linux64/chrome-headless-shell" });
  const page = await browser.newPage({ viewport: { width: 2048, height: 900 } });
  const sockets = [];
  await page.routeWebSocket("**/ws", socket => {
    const server = socket.connectToServer();
    server.onMessage(message => {
      const data = JSON.parse(String(message));
      const injectRunningSummary = value => {
        if (!value || typeof value !== "object") return;
        if (value.id === thread.id && "running_turn" in value && "queued_turn_count" in value) {
          value.running_turn = {
            id: 9001, requester_thread_id: null, requester_turn_id: null, thread_id: thread.id,
            owner_message_id: null, agent_message_id: null, state: "running",
            started_at: new Date().toISOString(), finished_at: null,
          };
          value.queued_turn_count = 1;
        }
        for (const child of Object.values(value)) if (child && typeof child === "object") injectRunningSummary(child);
      };
      injectRunningSummary(data);
      socket.send(JSON.stringify(data));
    });
    sockets.push(socket);
  });
  page.on("pageerror", error => browserErrors.push({ type: "pageerror", message: error.message }));
  page.on("console", message => { if (message.type() === "error") browserErrors.push({ type: "console", message: message.text() }); });
  await page.addInitScript(({ ownerToken }) => {
    if (window !== window.top) return;
    localStorage.setItem("hirsel.token", ownerToken);
    localStorage.setItem("hirsel.theme", "dark");
    if (!sessionStorage.getItem("hirsel.responsive-run-initialized")) {
      localStorage.removeItem("hirsel.thread-navigation.desktop");
      sessionStorage.setItem("hirsel.responsive-run-initialized", "true");
    }
  }, { ownerToken: token });
  await page.goto(url, { waitUntil: "domcontentloaded" });
  const drawer = page.locator('[data-slot="thread-drawer"]');
  await drawer.waitFor({ state: "visible" });
  const initialFocus = await drawer.evaluate(node => ({ inside: node.contains(document.activeElement), tag: document.activeElement?.tagName, label: document.activeElement?.getAttribute("aria-label") }));
  assert.equal(initialFocus.inside, false, `default dock stole initial focus: ${JSON.stringify(initialFocus)}`);
  await expandNestedThread(page);
  await page.locator(`[data-thread-row="${thread.id}"]`).click();
  await page.locator(`[data-thread-id="${thread.id}"]`).waitFor({ state: "visible" });
  await page.getByRole("button", { name: "All artifacts", exact: true }).click();
  const artifactRef = page.locator(`[data-artifact-ref="${artifact.id}"]`).first();
  await artifactRef.waitFor({ state: "visible" });
  await artifactRef.click();
  await page.locator('[data-slot="artifact-preview"]').waitFor({ state: "visible" });
  await page.getByRole("button", { name: "Conversation", exact: true }).click();

  for (const width of [2048, 1440, 1024, 768, 390, 320]) await capture(page, width, "selected", true);

  await page.setViewportSize({ width: 1440, height: 900 });
  await page.getByRole("button", { name: "Thread overview", exact: true }).click();
  const overview = await capture(page, 1440, "overview", false);
  assert.equal(overview.artifactTitle, artifact.title, "opening the overview reset the selected artifact");
  await page.locator(`[data-thread-row="${thread.id}"]`).click();
  const selectedAgain = await capture(page, 1440, "selected-again", true);
  assert.equal(selectedAgain.focusedThreadId, String(thread.id), "Thread selection changed across layout states");
  assert.equal(selectedAgain.artifactTitle, artifact.title, "Thread selection reset the artifact");

  await page.getByRole("button", { name: "Back to conversation", exact: true }).click();
  await page.setViewportSize({ width: 768, height: 844 });
  const trigger = page.getByRole("button", { name: "Threads", exact: true });
  await trigger.click();
  const modalDrawer = page.getByRole("dialog", { name: "Threads" });
  await modalDrawer.waitFor({ state: "visible" });
  await poll("modal Thread focus", () => modalDrawer.evaluate(node => node.contains(document.activeElement)));
  await page.keyboard.press("Escape");
  await modalDrawer.waitFor({ state: "hidden" });
  assert.equal(await trigger.evaluate(node => node === document.activeElement), true, "mobile drawer did not restore focus to its trigger");
  await page.setViewportSize({ width: 1440, height: 900 });
  await drawer.waitFor({ state: "visible" });

  await page.getByRole("button", { name: "Close threads", exact: true }).click();
  await drawer.waitFor({ state: "hidden" });
  await page.setViewportSize({ width: 768, height: 844 });
  await trigger.click();
  await modalDrawer.waitFor({ state: "visible" });
  await page.keyboard.press("Escape");
  await page.setViewportSize({ width: 1440, height: 900 });
  assert.equal(await drawer.isVisible(), false, "mobile dismissal overwrote the explicit desktop-closed preference");
  await page.reload({ waitUntil: "domcontentloaded" });
  assert.equal(await drawer.isVisible(), false, "desktop-closed preference did not survive reload");

  await trigger.click();
  await drawer.waitFor({ state: "visible" });
  await expandNestedThread(page);
  await page.locator(`[data-thread-row="${thread.id}"]`).click();
  await page.getByRole("button", { name: "All artifacts", exact: true }).click();
  await page.locator(`[data-artifact-ref="${artifact.id}"]`).first().click();
  await page.locator('[data-slot="artifact-preview"]').waitFor({ state: "visible" });
  await page.getByRole("button", { name: "Conversation", exact: true }).click();
  await page.context().setOffline(true);
  for (const socket of sockets) await socket.close({ code: 1001, reason: "responsive offline check" });
  await page.waitForFunction(() => [...document.querySelectorAll('[data-slot="thread-status-primary"]')].some(node => node.textContent?.includes("Last known:")));
  const nestedOffline = await capture(page, 1440, "nested-offline", false);
  assert(nestedOffline.status.some(row => row.text?.includes("Last known: Working · 1 queued")), "longest offline nested Thread status was not exercised");
  await page.getByRole("button", { name: "Back to conversation", exact: true }).click();
  await page.setViewportSize({ width: 320, height: 844 });
  if (!await drawer.isVisible()) await trigger.click();
  await drawer.waitFor({ state: "visible" });
  const nestedStatus = page.locator(`[data-thread-entry="tree:${thread.id}"] [data-slot="thread-status-primary"]`);
  await nestedStatus.scrollIntoViewIfNeeded();
  const narrowNestedOffline = await layoutMetrics(page);
  assert.equal(narrowNestedOffline.viewport.width, 320);
  assert.equal(narrowNestedOffline.drawerRole, "dialog");
  assert.equal(narrowNestedOffline.pageOverflow, false, "320: page overflows with nested Thread drawer open");
  assertStatuses(narrowNestedOffline, 320);
  assert(narrowNestedOffline.status.some(row => row.text?.includes("Last known: Working · 1 queued")), "320: running and queued offline summary was not exercised");
  await page.screenshot({ path: join(evidenceDir, "nested-offline-320.png"), fullPage: true });
  captures.push({ label: "nested-offline", ...narrowNestedOffline });

  assert.deepEqual(browserErrors, [], `browser errors: ${JSON.stringify(browserErrors)}`);
  await writeFile(join(evidenceDir, "result.json"), `${JSON.stringify({
    source: { head: execFileSync("git", ["rev-parse", "HEAD"], { cwd: repo, encoding: "utf8" }).trim(), dirty: execFileSync("git", ["status", "--short"], { cwd: repo, encoding: "utf8" }).trim() },
    fixture, copiedState: dataDir, service: "scripted/fake", thread, artifact, captures, browserErrors, objectiveStatus: "OBJECTIVE_PASS",
  }, null, 2)}\n`);
  console.log(JSON.stringify({ objectiveStatus: "OBJECTIVE_PASS", evidenceDir, fixture, viewports: captures.map(row => row.viewport.width) }));
} finally {
  await browser?.close();
  await stopProcess(host);
  log.end();
}
