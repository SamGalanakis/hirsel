#!/usr/bin/env node
/**
 * Focus↔ambient continuity + responsive geometry sweep.
 *
 * Boots the seeded mock host and Vite exactly like the task-margins runner,
 * then at each audited width measures the shell in BOTH states (ambient and a
 * focused task) and reports:
 *   - horizontal page overflow (document and any offending element),
 *   - the geometry that must NOT move across the focus swap (composer capsule,
 *     send button, task strip, floating ⋯, field left edge),
 *   - wide-content containers that must scroll inside themselves.
 *
 * Output is one JSON blob on stdout; screenshots land in e2e/reports/sweep-*.
 */
import { spawn } from "node:child_process";
import { mkdir } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { chromium } from "../app/node_modules/playwright/index.mjs";

const root = fileURLToPath(new URL("..", import.meta.url));
const app = `${root}/app`;
const reportDir = `${root}/e2e/reports`;
const mockPort = 39131;
const vitePort = 39132;
const origin = `http://127.0.0.1:${vitePort}`;
const token = "responsive-sweep";
const children = [];
const shot = process.env.SWEEP_SHOTS === "1";
const tag = process.env.SWEEP_TAG ?? "after";

const WIDTHS = [
  { w: 360, h: 780 },
  { w: 400, h: 850 },
  { w: 717, h: 900 },
  { w: 900, h: 900 },
  { w: 1100, h: 900 },
  { w: 1440, h: 1000 },
  { w: 1440, h: 600 },
  { w: 400, h: 520 }, // phone with an on-screen keyboard up
];

function start(command, args, options) {
  const child = spawn(command, args, { ...options, detached: true, stdio: ["ignore", "pipe", "pipe"] });
  children.push(child);
  return child;
}

async function poll(label, probe, timeoutMs = 30_000) {
  const deadline = Date.now() + timeoutMs;
  let lastError;
  while (Date.now() < deadline) {
    try {
      if (await probe()) return true;
    } catch (error) {
      lastError = error;
    }
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  throw new Error(`${label} not ready${lastError ? `: ${lastError.message}` : ""}`);
}

const probeGeometry = () => {
  const rect = (selector) => {
    const node = document.querySelector(selector);
    if (!node) return null;
    const r = node.getBoundingClientRect();
    return { x: Math.round(r.x * 10) / 10, y: Math.round(r.y * 10) / 10, w: Math.round(r.width * 10) / 10, h: Math.round(r.height * 10) / 10 };
  };
  const doc = document.documentElement;
  // An element parked outside the viewport INSIDE a horizontal scroller (the
  // task strip, a table box) is reachable and fine; only content with no
  // scrollable ancestor is genuinely cut off, since the app shell is
  // `overflow: hidden` and the page itself never scrolls.
  const inScroller = (node) => {
    for (let p = node.parentElement; p; p = p.parentElement) {
      const x = getComputedStyle(p).overflowX;
      if (x === "auto" || x === "scroll") return true;
    }
    return false;
  };
  const overflowing = [];
  const scrollers = [];
  for (const node of document.querySelectorAll("body *")) {
    const r = node.getBoundingClientRect();
    if (r.width > 0 && r.height > 0 && (r.right > window.innerWidth + 1 || r.left < -1) && !inScroller(node)) {
      overflowing.push({
        tag: node.tagName.toLowerCase(),
        slot: node.getAttribute("data-slot") ?? node.className?.toString?.().slice(0, 60) ?? "",
        left: Math.round(r.left),
        right: Math.round(r.right),
      });
    }
    if (node.scrollWidth > node.clientWidth + 1 && node.clientWidth > 4) {
      const style = getComputedStyle(node);
      if (style.overflowX !== "auto" && style.overflowX !== "scroll") {
        scrollers.push({
          slot: node.getAttribute("data-slot") ?? node.tagName.toLowerCase(),
          cls: node.className?.toString?.().slice(0, 40) ?? "",
          overflowX: style.overflowX,
          scrollWidth: node.scrollWidth,
          clientWidth: node.clientWidth,
        });
      }
    }
  }
  const composerNode = document.querySelector('[data-slot="composer-shell"]');
  const composerRect = composerNode?.getBoundingClientRect();
  const sendRect = document.querySelector('[aria-label="Send"]')?.getBoundingClientRect();
  return {
    viewport: { w: window.innerWidth, h: window.innerHeight },
    // The composer must be wholly on screen at every width and height — it is
    // the one control that must never need scrolling to reach.
    composerOnScreen: composerRect
      ? composerRect.bottom <= window.innerHeight + 1 && composerRect.top >= -1
        && (!sendRect || sendRect.right <= window.innerWidth + 1)
      : false,
    // One column in both states now. The probe watches the conversation half
    // of the focused column: a second track here would mean the two-column
    // margin had crept back.
    gridCols: (() => {
      const f = document.querySelector('[data-slot="task-conversation"]');
      return f ? getComputedStyle(f).gridTemplateColumns : null;
    })(),
    pageOverflow: doc.scrollWidth - doc.clientWidth,
    bodyOverflow: document.body.scrollWidth - window.innerWidth,
    overflowing: overflowing.slice(0, 8),
    scrollers: scrollers.slice(0, 8),
    composer: rect('[data-slot="composer-shell"]'),
    composerFrame: rect('[data-slot="composer-shell"]')
      && rect('[data-slot="composer-shell"]'),
    send: rect('[aria-label="Send"]'),
    attach: rect('[aria-label="Attach files"]'),
    textarea: rect('[data-composer="main"]'),
    index: rect('[data-slot="task-index"]'),
    more: rect('[data-slot="phone-overflow-trigger"]'),
    affordances: rect('[data-slot="home-affordances"]'),
    scroll: rect('[data-slot="task-scroll"]'),
    field: rect('[data-slot="task-field"]') ?? rect('[data-slot="ambient-field"]'),
    card: rect('[data-slot="task-card"]'),
    column: rect('[data-slot="task-column"]'),
    conversation: rect('[data-slot="conversation"]'),
    firstChip: rect("[data-task-id]"),
  };
};

async function main() {
  start(process.execPath, ["tools/mock-server.mjs"], {
    cwd: app,
    env: { ...process.env, MOCK_PORT: String(mockPort), MOCK_REPLY_MS: "20" },
  });
  start("npm", ["exec", "vite", "--", "--host", "127.0.0.1", "--port", String(vitePort), "--strictPort"], {
    cwd: app,
    env: { ...process.env, VITE_WS_URL: "same-origin", HIRSEL_DEV_PROXY_TARGET: `ws://127.0.0.1:${mockPort}` },
  });
  await poll("mock", async () => (await fetch(`http://127.0.0.1:${mockPort}/ready`)).status === 404);
  await poll("vite", async () => (await fetch(origin)).ok);

  const browser = await chromium.launch({ headless: true });
  const results = [];
  const errors = [];
  try {
    for (const { w, h } of WIDTHS) {
      const context = await browser.newContext({
        viewport: { width: w, height: h },
        hasTouch: w < 900,
        serviceWorkers: "block",
        colorScheme: "dark",
      });
      await context.addInitScript((value) => localStorage.setItem("hirsel.token", value), token);
      const page = await context.newPage();
      page.on("pageerror", (error) => errors.push(`${w}x${h}: ${error.message}`));
      await page.goto(origin, { waitUntil: "domcontentloaded" });
      await page.locator('[data-task-id]').first().waitFor();
      await page.waitForTimeout(600);

      // The load lands focused on the most-needing task; Esc clears to ambient.
      await page.keyboard.press("Escape");
      await page.waitForTimeout(400);
      const ambient = await page.evaluate(probeGeometry);
      if (shot) {
        await mkdir(reportDir, { recursive: true });
        await page.screenshot({ path: `${reportDir}/sweep-${tag}-${w}x${h}-ambient.png` });
      }

      await page.locator("[data-task-id]").first().click();
      await page.waitForTimeout(500);
      const focused = await page.evaluate(probeGeometry);
      if (shot) await page.screenshot({ path: `${reportDir}/sweep-${tag}-${w}x${h}-focused.png` });

      // A long markdown table + code block in the focused task's margin is the
      // wide-content case; send one and re-measure overflow.
      await page.locator('[data-composer="main"]').fill(
        "| alpha column | beta column | gamma column | delta column |\n| --- | --- | --- | --- |\n| 1111111111 | 2222222222 | 3333333333 | 4444444444 |\n\n```\nnpm run some::extremely::long::command --with-a-flag=/very/long/path/that/never/wraps/at/all\n```",
      );
      await page.locator('[aria-label="Send"]').click();
      await page.waitForTimeout(900);
      const wide = await page.evaluate(probeGeometry);
      if (shot) await page.screenshot({ path: `${reportDir}/sweep-${tag}-${w}x${h}-wide.png` });

      // Utility panes over the same field: Processes then Settings, both
      // reached from the one floating ⋯.
      const panes = {};
      for (const item of ["Processes", "Settings"]) {
        await page.locator('[data-slot="phone-overflow-trigger"]').click();
        await page.waitForTimeout(200);
        await page.getByRole("menuitem", { name: new RegExp(item, "i") }).first().click();
        await page.waitForTimeout(500);
        panes[item] = await page.evaluate(probeGeometry);
        if (shot) await page.screenshot({ path: `${reportDir}/sweep-${tag}-${w}x${h}-${item}.png` });
        await page.keyboard.press("Escape");
        await page.waitForTimeout(300);
      }

      const chips = await page.evaluate(() =>
        [...document.querySelectorAll("[data-task-id]")].map((n) => Math.round(n.getBoundingClientRect().width)));

      results.push({ w, h, ambient, focused, wide, panes, chips });
      await context.close();
    }
  } finally {
    await browser.close();
    for (const child of children) {
      try { process.kill(-child.pid, "SIGKILL"); } catch { /* already gone */ }
    }
  }
  process.stdout.write(`${JSON.stringify({ results, errors }, null, 1)}\n`);
}

main().then(() => process.exit(0), (error) => {
  console.error(error);
  for (const child of children) {
    try { process.kill(-child.pid, "SIGKILL"); } catch { /* already gone */ }
  }
  process.exit(1);
});
