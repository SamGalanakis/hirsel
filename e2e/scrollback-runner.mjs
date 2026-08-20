#!/usr/bin/env node
import { spawn, spawnSync } from "node:child_process";
import { mkdir, writeFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { chromium } from "../app/node_modules/playwright/index.mjs";

const root = fileURLToPath(new URL("..", import.meta.url));
const app = `${root}/app`;
const reportDir = `${root}/e2e/reports`;
const mockPort = 39135;
const vitePort = 39136;
const origin = `http://127.0.0.1:${vitePort}`;
const token = "scrollback-e2e";
const exactCommand = "cd app && npm run e2e:scrollback";
const startedAt = new Date().toISOString();
const children = [];
const logs = [];
const checks = [];

function gitOutput(args) {
  const result = spawnSync("git", args, { cwd: root, encoding: "utf8" });
  return result.status === 0 ? result.stdout.trim() : "unavailable";
}

const revision = gitOutput(["rev-parse", "HEAD"]);
const dirtyOutput = gitOutput(["status", "--porcelain=v1"]);
const dirtyEntries = dirtyOutput === "unavailable" || dirtyOutput === "" ? 0 : dirtyOutput.split("\n").length;

function check(name, passed, evidence) {
  checks.push({ name, passed: Boolean(passed), evidence });
  if (!passed) throw new Error(`${name}: ${evidence}`);
}

function start(command, args, options) {
  const child = spawn(command, args, {
    ...options,
    detached: process.platform !== "win32",
    stdio: ["ignore", "pipe", "pipe"],
  });
  children.push(child);
  for (const stream of [child.stdout, child.stderr]) {
    stream.on("data", (chunk) => logs.push(chunk.toString().trim()));
  }
}

async function poll(label, probe, timeoutMs = 30_000) {
  const deadline = Date.now() + timeoutMs;
  let lastError;
  while (Date.now() < deadline) {
    try {
      const result = await probe();
      if (result) return result;
    } catch (error) {
      lastError = error;
    }
    await new Promise((resolve) => setTimeout(resolve, 75));
  }
  throw new Error(`${label} timed out${lastError ? `: ${lastError.message}` : ""}`);
}

async function writeReport(error = null) {
  await mkdir(reportDir, { recursive: true });
  const result = {
    runner: "scrollback",
    revision,
    dirty: dirtyEntries > 0,
    dirty_entries: dirtyEntries,
    started_at: startedAt,
    completed_at: new Date().toISOString(),
    command: exactCommand,
    browser: "Chromium headless, 1280x800, light, reduced motion",
    services: `mock ${mockPort}; Vite ${vitePort}`,
    status: error ? "failed" : "passed",
    checks,
    error: error?.message ?? null,
    screenshot: "e2e/reports/scrollback-latest.png",
  };
  await writeFile(`${reportDir}/scrollback-latest.json`, `${JSON.stringify(result, null, 2)}\n`);
  const rows = checks.map(({ name, passed, evidence }) =>
    `| ${passed ? "PASS" : "FAIL"} | ${name} | ${String(evidence).replaceAll("|", "\\|")} |`);
  await writeFile(`${reportDir}/scrollback-latest.md`, [
    "# Just-in-time scrollback browser report",
    "",
    `Status: **${result.status.toUpperCase()}**`,
    "",
    `- Revision: \`${revision}\``,
    `- Worktree: **${result.dirty ? `dirty (${dirtyEntries} entries)` : "clean"}**`,
    `- Started: \`${result.started_at}\``,
    `- Completed: \`${result.completed_at}\``,
    `- Command: \`${exactCommand}\``,
    `- Services: ${result.services}`,
    `- Browser: ${result.browser}`,
    `- Screenshot: \`${result.screenshot}\``,
    "",
    "| Gate | Check | Evidence |",
    "| --- | --- | --- |",
    ...rows,
    ...(error ? ["", `Failure: ${error.message}`] : []),
    "",
  ].join("\n"));
}

async function main() {
  start(process.execPath, ["tools/mock-server.mjs"], {
    cwd: app,
    env: {
      ...process.env,
      MOCK_PORT: String(mockPort),
      MOCK_HISTORY_COUNT: "760",
      MOCK_FETCH_DELAY_MS: "300",
    },
  });
  start("npm", ["exec", "vite", "--", "--host", "127.0.0.1", "--port", String(vitePort), "--strictPort"], {
    cwd: app,
    env: {
      ...process.env,
      VITE_WS_URL: "same-origin",
      HIRSEL_DEV_PROXY_TARGET: `ws://127.0.0.1:${mockPort}`,
    },
  });
  await poll("mock readiness", async () => (await fetch(`http://127.0.0.1:${mockPort}/ready`)).status === 404);
  await poll("Vite readiness", async () => (await fetch(origin)).ok);
  check("isolated services", true, `mock ${mockPort}; Vite ${vitePort}; readiness polled`);

  const browser = await chromium.launch({ headless: true });
  try {
    const context = await browser.newContext({
      viewport: { width: 1280, height: 800 },
      serviceWorkers: "block",
      colorScheme: "light",
      reducedMotion: "reduce",
    });
    await context.addInitScript((value) => localStorage.setItem("hirsel.token", value), token);
    const page = await context.newPage();
    page.setDefaultTimeout(90_000);
    const browserErrors = [];
    const requestErrors = [];
    const sentFrames = [];
    page.on("console", (message) => {
      if (message.type() === "error") browserErrors.push(message.text());
    });
    page.on("pageerror", (error) => browserErrors.push(error.message));
    page.on("requestfailed", (request) => requestErrors.push(`${request.method()} ${request.url()}: ${request.failure()?.errorText}`));
    page.on("websocket", (socket) => socket.on("framesent", (event) => {
      try { sentFrames.push(JSON.parse(String(event.payload))); } catch { /* irrelevant binary */ }
    }));

    await page.goto(origin, { waitUntil: "domcontentloaded", timeout: 90_000 });
    await page.locator('[data-slot="conversation"] article[data-message-id]').first().waitFor();
    if ((await page.locator('[data-slot="task-field"]').count()) > 0) {
      await page.keyboard.press("Escape");
      await page.locator('[data-slot="ambient-field"]').waitFor();
    }
    const scroller = page.locator('[data-slot="task-scroll"]');
    const articles = page.locator('[data-slot="conversation"] article[data-message-id]');
    let previousCount = -1;
    let stableReads = 0;
    const initialCount = await poll("initial prefetch settles", async () => {
      const count = await articles.count();
      stableReads = count === previousCount ? stableReads + 1 : 0;
      previousCount = count;
      return stableReads >= 3 ? count : 0;
    });
    check(
      "bounded initial render window",
      initialCount >= 30 && initialCount <= 90
        && (await page.locator('[data-slot="reveal-earlier"]').count()) === 0,
      `${initialCount} articles; no manual reveal control`,
    );

    const anchor = await scroller.evaluate((element) => {
      element.scrollTop = 0;
      const row = element.querySelector('[data-slot="conversation"] article[data-message-id]');
      return {
        id: row?.getAttribute("data-message-id"),
        top: row?.getBoundingClientRect().top,
      };
    });
    await poll("automatic client-window reveal", async () => {
      await scroller.evaluate((element) => {
        element.scrollTop = element.scrollTop <= 0 ? 1 : 0;
      });
      return (await articles.count()) > initialCount;
    });
    const anchoredTop = await poll("prepend anchor restoration", async () => {
      const top = await page.locator(`[data-message-id="${anchor.id}"]`).evaluate((row) => row.getBoundingClientRect().top);
      return Math.abs(top - anchor.top) <= 1.5 ? top : false;
    });
    const anchorDelta = Math.abs(anchoredTop - anchor.top);
    check("prepend anchoring", anchorDelta <= 1.5, `message ${anchor.id}; top delta ${anchorDelta.toFixed(2)}px`);

    const firstFetch = await poll("first host history request", async () => {
      await scroller.evaluate((element) => {
        element.scrollTop = element.scrollTop <= 0 ? 1 : 0;
      });
      return sentFrames.find((frame) => frame.type === "fetch_messages");
    });
    check(
      "correlated host prefetch",
      firstFetch.limit === 100 && Number.isInteger(firstFetch.before_id) && typeof firstFetch.client_id === "string",
      JSON.stringify(firstFetch),
    );
    await poll("loading row while request is outstanding", async () =>
      (await page.locator('[data-slot="loading-earlier"]').count()) === 1);
    const hostAnchor = await scroller.evaluate((element) => {
      const row = element.querySelector('[data-slot="conversation"] article[data-message-id]');
      return {
        id: row?.getAttribute("data-message-id"),
        top: row?.getBoundingClientRect().top,
      };
    });
    await poll("older host page rendered", async () =>
      (await page.getByText("Archived history 565:", { exact: false }).count()) > 0);
    const hostAnchoredTop = await poll("host prepend anchor restoration", async () => {
      const top = await page.locator(`[data-message-id="${hostAnchor.id}"]`).evaluate((row) => row.getBoundingClientRect().top);
      return Math.abs(top - hostAnchor.top) <= 1.5 ? top : false;
    });
    const hostAnchorDelta = Math.abs(hostAnchoredTop - hostAnchor.top);
    check(
      "host-page prepend anchoring",
      hostAnchorDelta <= 1.5,
      `message ${hostAnchor.id}; top delta ${hostAnchorDelta.toFixed(2)}px`,
    );

    await poll("true beginning of history", async () => {
      await scroller.evaluate((element) => {
        element.scrollTop = element.scrollTop <= 0 ? 1 : 0;
      });
      return (await page.getByText("Archived history 1:", { exact: false }).count()) > 0;
    }, 45_000);
    await poll("end-state settles", async () =>
      (await page.locator('[data-slot="loading-earlier"]').count()) === 0);
    check(
      "silent true beginning",
      (await page.locator('[data-slot="loading-earlier"]').count()) === 0
        && (await page.locator('[data-slot="reveal-earlier"]').count()) === 0,
      `${sentFrames.filter((frame) => frame.type === "fetch_messages").length} bounded pages; no spinner or button`,
    );

    check(
      "bounded range exposes truthful latest jump",
      (await page.getByRole("button", { name: "Jump to latest" }).count()) === 1
        && (await page.getByText("Archived history 760:", { exact: false }).count()) === 0,
      "newest edge evicted only after the 600-row cap",
    );
    await page.getByRole("button", { name: "Jump to latest" }).click();
    await poll("latest page reload", async () =>
      (await page.getByText("Archived history 760:", { exact: false }).count()) > 0);
    check("jump reaches true latest", true, "newest Host page reloaded before bottom pin");

    await mkdir(reportDir, { recursive: true });
    await page.screenshot({ path: `${reportDir}/scrollback-latest.png`, animations: "disabled" });
    check(
      "browser console and requests",
      browserErrors.length === 0 && requestErrors.length === 0,
      [...browserErrors, ...requestErrors].join("; ") || "no console, page, or request failures",
    );
    await context.close();
  } finally {
    await browser.close();
  }
}

let failure = null;
try {
  await main();
} catch (error) {
  failure = error;
  process.exitCode = 1;
} finally {
  for (const child of children) {
    try {
      if (process.platform === "win32") child.kill("SIGTERM");
      else process.kill(-child.pid, "SIGTERM");
    } catch { /* owned process group already exited */ }
  }
  await Promise.all(children.map((child) => new Promise((resolve) => {
    if (child.exitCode !== null || child.signalCode !== null) return resolve();
    child.once("exit", resolve);
    setTimeout(() => {
      try {
        if (process.platform === "win32") child.kill("SIGKILL");
        else process.kill(-child.pid, "SIGKILL");
      } catch { /* owned process group already exited */ }
      resolve();
    }, 1_000);
  })));
  await writeReport(failure);
}

if (failure) {
  console.error(failure);
  console.error(logs.slice(-12).join("\n"));
} else {
  console.log(`Scrollback browser gates passed (${checks.length}/${checks.length}).`);
  console.log(`Report: ${reportDir}/scrollback-latest.md`);
}
