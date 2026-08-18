#!/usr/bin/env node
import { spawn, spawnSync } from "node:child_process";
import { mkdtemp, mkdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { chromium } from "../app/node_modules/playwright/index.mjs";

const root = fileURLToPath(new URL("..", import.meta.url));
const app = `${root}/app`;
const reportDir = `${root}/e2e/reports`;
const hostPort = 39129;
const vitePort = 39130;
const hostOrigin = `http://127.0.0.1:${hostPort}`;
const origin = `http://127.0.0.1:${vitePort}`;
const token = "task-host-e2e-token";
const exactCommand = "cd app && npm run e2e:task-host";
const startedAt = new Date().toISOString();
const children = [];
const logs = [];
const checks = [];
let dataDir;

function gitOutput(args) {
  const result = spawnSync("git", args, { cwd: root, encoding: "utf8" });
  return result.status === 0 ? result.stdout.trim() : "unavailable";
}

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
  return child;
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
  throw new Error(`${label} did not settle${lastError ? `: ${lastError.message}` : ""}`);
}

async function debug(path, options = {}) {
  const response = await fetch(`${hostOrigin}${path}`, {
    ...options,
    headers: {
      authorization: `Bearer ${token}`,
      ...(options.body ? { "content-type": "application/json" } : {}),
      ...options.headers,
    },
  });
  if (!response.ok) throw new Error(`${path}: ${response.status} ${await response.text()}`);
  return response.json();
}

async function eventState(eventId, predicate) {
  return poll("Host event state", async () => {
    const { events } = await debug("/debug/events");
    const event = events.find((candidate) => candidate.id === eventId);
    return event && predicate(event) ? event : null;
  });
}

async function postEventAction(eventId, action, data) {
  const response = await fetch(`${hostOrigin}/debug/event-action`, {
    method: "POST",
    headers: {
      authorization: `Bearer ${token}`,
      "content-type": "application/json",
    },
    body: JSON.stringify({ event_id: eventId, action, data }),
  });
  return { response, body: await response.text() };
}

async function writeReport(error = null) {
  await mkdir(reportDir, { recursive: true });
  const revision = gitOutput(["rev-parse", "HEAD"]);
  const dirty = gitOutput(["status", "--porcelain=v1"]);
  const result = {
    runner: "task-host",
    provenance: {
      revision,
      dirty: Boolean(dirty && dirty !== "unavailable"),
      dirty_entries: dirty && dirty !== "unavailable" ? dirty.split("\n").length : 0,
      started_at: startedAt,
      completed_at: new Date().toISOString(),
      command: exactCommand,
    },
    services: [
      { kind: "rust-host-scripted-agent", command: "prebuilt target/debug/hirsel-host", cwd: "isolated temporary directory", port: hostPort },
      { kind: "vite-development-server", command: `npm exec vite -- --host 127.0.0.1 --port ${vitePort} --strictPort`, cwd: "app", port: vitePort },
    ],
    browser: { engine: "Chromium", headless: true, viewport: "1280x900", service_workers: "blocked" },
    status: error ? "failed" : "passed",
    screenshot: "e2e/reports/task-host-latest.png",
    checks,
    error: error?.message ?? null,
  };
  await writeFile(`${reportDir}/task-host-latest.json`, `${JSON.stringify(result, null, 2)}\n`);
  const rows = checks.map(({ name, passed, evidence }) =>
    `| ${passed ? "PASS" : "FAIL"} | ${name} | ${String(evidence).replaceAll("|", "\\|")} |`);
  await writeFile(`${reportDir}/task-host-latest.md`, [
    "# Host-backed adaptive Task browser report",
    "",
    `Status: **${result.status.toUpperCase()}**`,
    "",
    `- Revision: \`${revision}\``,
    `- Worktree: **${result.provenance.dirty ? `dirty (${result.provenance.dirty_entries} entries)` : "clean"}**`,
    `- Started: \`${result.provenance.started_at}\``,
    `- Completed: \`${result.provenance.completed_at}\``,
    `- Command: \`${exactCommand}\``,
    "- Services: real Rust Host with scripted global Agent; Vite frontend",
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
  dataDir = await mkdtemp(join(tmpdir(), "hirsel-task-host-"));
  const build = spawnSync("cargo", ["build", "-p", "hirsel-host", "--bin", "hirsel-host"], {
    cwd: root,
    encoding: "utf8",
  });
  if (build.status !== 0) throw new Error(`Host build failed: ${build.stderr || build.stdout}`);
  const metadata = spawnSync("cargo", ["metadata", "--no-deps", "--format-version", "1"], {
    cwd: root,
    encoding: "utf8",
  });
  if (metadata.status !== 0) throw new Error(`Cargo metadata failed: ${metadata.stderr}`);
  const hostBinary = join(JSON.parse(metadata.stdout).target_directory, "debug", "hirsel-host");
  start(hostBinary, [], {
    cwd: dataDir,
    env: {
      ...process.env,
      HIRSEL_TOKEN: token,
      HIRSEL_AGENT: "scripted",
      HIRSEL_DRIVER: "fake",
      HIRSEL_DEBUG: "1",
      HIRSEL_IROH: "0",
      HIRSEL_DATA_DIR: dataDir,
      HIRSEL_TEMPLATES_DIR: `${root}/templates`,
      HIRSEL_LISTEN: `127.0.0.1:${hostPort}`,
    },
  });
  start("npm", ["exec", "vite", "--", "--host", "127.0.0.1", "--port", String(vitePort), "--strictPort"], {
    cwd: app,
    env: {
      ...process.env,
      VITE_WS_URL: "same-origin",
      HIRSEL_DEV_PROXY_TARGET: `ws://127.0.0.1:${hostPort}`,
    },
  });
  await poll("Rust Host readiness", async () => (await debug("/debug/health")).ok, 60_000);
  await poll("Vite readiness", async () => (await fetch(origin)).ok);
  check("isolated real services", true, `Rust Host ${hostPort}; Vite ${vitePort}; readiness polled`);

  const seeded = await debug("/debug/seed-adaptive-task", { method: "POST", body: "{}" });
  check("anchored fixture", seeded.id > 0 && seeded.anchor > 0 && seeded.status === "open", `id=${seeded.id}; anchor=${seeded.anchor}; status=${seeded.status}`);
  const chatBeforeHostile = (await debug("/debug/chat")).messages;
  const { response: hostileResponse, body: hostileBody } = await postEventAction(
    seeded.id,
    "advance",
    { confirmation: 42, injected: true },
  );
  const hostileState = await eventState(seeded.id, () => true);
  const chatAfterHostile = (await debug("/debug/chat")).messages;
  check(
    "hostile action rejected before Agent turn",
    !hostileResponse.ok
      && hostileBody.includes("unknown Task action data field")
      && JSON.stringify(hostileState.ui) === JSON.stringify(seeded.ui)
      && chatAfterHostile.length === chatBeforeHostile.length,
    `HTTP ${hostileResponse.status}; same UI=${JSON.stringify(hostileState.ui) === JSON.stringify(seeded.ui)}; chat ${chatBeforeHostile.length}->${chatAfterHostile.length}`,
  );
  const { response: undeclaredSubmitResponse, body: undeclaredSubmitBody } = await postEventAction(
    seeded.id,
    "submit",
    { confirmation: "ready" },
  );
  const stateAfterUndeclaredSubmit = await eventState(seeded.id, () => true);
  const chatAfterUndeclaredSubmit = (await debug("/debug/chat")).messages;
  check(
    "undeclared terminal submit is inert",
    !undeclaredSubmitResponse.ok
      && undeclaredSubmitBody.includes("is not declared by the current Task UI")
      && stateAfterUndeclaredSubmit.status === "open"
      && JSON.stringify(stateAfterUndeclaredSubmit.ui) === JSON.stringify(seeded.ui)
      && chatAfterUndeclaredSubmit.length === chatBeforeHostile.length,
    `HTTP ${undeclaredSubmitResponse.status}; status=${stateAfterUndeclaredSubmit.status}; same UI=${JSON.stringify(stateAfterUndeclaredSubmit.ui) === JSON.stringify(seeded.ui)}; chat ${chatBeforeHostile.length}->${chatAfterUndeclaredSubmit.length}`,
  );

  const browser = await chromium.launch({ headless: true });
  try {
    const context = await browser.newContext({
      viewport: { width: 1280, height: 900 },
      serviceWorkers: "block",
      reducedMotion: "reduce",
    });
    await context.addInitScript((value) => localStorage.setItem("hirsel.token", value), token);
    const page = await context.newPage();
    const browserErrors = [];
    const websocketUrls = [];
    page.on("console", (message) => {
      if (message.type() === "error") browserErrors.push(message.text());
    });
    page.on("pageerror", (error) => browserErrors.push(error.message));
    page.on("websocket", (socket) => websocketUrls.push(socket.url()));
    await page.goto(origin, { waitUntil: "domcontentloaded" });
    try {
      await page.locator('[aria-label="connected"]').waitFor();
    } catch (error) {
      const diagnostic = await page.evaluate(() => ({
        token_present: Boolean(localStorage.getItem("hirsel.token")),
        body: document.body.innerText.slice(0, 500),
      }));
      throw new Error(
        `browser did not connect; token present=${diagnostic.token_present}; `
        + `websockets=${websocketUrls.join(",") || "none"}; `
        + `console=${browserErrors.join("; ") || "none"}; body=${JSON.stringify(diagnostic.body)}; `
        + `cause=${error.message}`,
      );
    }
    await page.locator(`[data-task-id="${seeded.id}"]`).click();
    await page.getByRole("heading", { name: "A Task that changes with the work" }).waitFor();
    await page.getByRole("textbox", { name: "Confirmation" }).fill("ready");
    await page.getByRole("button", { name: "Continue" }).click();

    const recomposed = await eventState(seeded.id, (event) =>
      event.status === "open" && event.ui?.children?.some((node) => node.type === "status"));
    await page.getByRole("heading", { name: "adaptive-host-proof advanced" }).waitFor();
    check(
      "generic action recomposes in place",
      recomposed.id === seeded.id && recomposed.anchor === seeded.anchor && recomposed.status === "open",
      `same id=${recomposed.id}; same anchor=${recomposed.anchor}; status=${recomposed.status}`,
    );
    const { messages } = await debug("/debug/chat");
    const actionMessage = messages.find((message) => message.author === "owner" && message.ref === seeded.anchor);
    check("valid form launched global Agent turn", Boolean(actionMessage), `confirmation=ready; owner message ref=${actionMessage?.ref}; message id=${actionMessage?.id}`);

    const chatBeforeInvalidChoice = (await debug("/debug/chat")).messages;
    const { response: invalidChoiceResponse, body: invalidChoiceBody } = await postEventAction(
      seeded.id,
      "choose",
      { choice: "A", label: "Fabricated terminal choice" },
    );
    const stateAfterInvalidChoice = await eventState(seeded.id, () => true);
    const chatAfterInvalidChoice = (await debug("/debug/chat")).messages;
    check(
      "mismatched terminal choice is inert",
      !invalidChoiceResponse.ok
        && invalidChoiceBody.includes("data.label does not match choice")
        && stateAfterInvalidChoice.status === "open"
        && JSON.stringify(stateAfterInvalidChoice.ui) === JSON.stringify(recomposed.ui)
        && chatAfterInvalidChoice.length === chatBeforeInvalidChoice.length,
      `HTTP ${invalidChoiceResponse.status}; status=${stateAfterInvalidChoice.status}; same UI=${JSON.stringify(stateAfterInvalidChoice.ui) === JSON.stringify(recomposed.ui)}; chat ${chatBeforeInvalidChoice.length}->${chatAfterInvalidChoice.length}`,
    );

    await page.getByRole("button", { name: /Complete task/ }).click();
    const settled = await eventState(seeded.id, (event) => event.status === "done");
    await page.getByText("Task decided").waitFor();
    check("terminal action settles authoritatively", settled.status === "done", `Host status=${settled.status}`);

    await page.reload({ waitUntil: "domcontentloaded" });
    await page.locator('[aria-label="connected"]').waitFor();
    await page.locator(`[data-task-id="${seeded.id}"]`).click();
    await page.getByText("Task decided").waitFor();
    await page.getByRole("button", { name: "Reopen" }).click();
    const reopened = await eventState(seeded.id, (event) => event.status === "open");
    await page.getByRole("heading", { name: "adaptive-host-proof advanced" }).waitFor();
    check(
      "reload and reopen preserve meaningful stage",
      reopened.id === seeded.id && reopened.anchor === seeded.anchor
        && reopened.ui?.children?.some((node) => node.label === "Received action: advance"),
      `same id=${reopened.id}; same anchor=${reopened.anchor}; recomposed status instrument retained`,
    );
    await mkdir(reportDir, { recursive: true });
    await page.screenshot({ path: `${reportDir}/task-host-latest.png`, animations: "disabled" });
    check("browser console", browserErrors.length === 0, browserErrors.join("; ") || "no console or page errors");
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
  if (dataDir) await rm(dataDir, { recursive: true, force: true });
  await writeReport(failure);
}

if (failure) {
  console.error(failure);
  console.error(logs.slice(-16).join("\n"));
} else {
  console.log(`Host-backed adaptive Task browser gates passed (${checks.length}/${checks.length}).`);
}
