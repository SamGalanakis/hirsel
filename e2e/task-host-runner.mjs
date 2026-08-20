#!/usr/bin/env node
import { mkdir, readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { chromium } from "../app/node_modules/playwright/index.mjs";
import {
  appReady,
  PORTS,
  pollReady,
  recordCheck,
  startHost,
  startProcess,
  teardown,
  writeReport,
} from "./lib/harness.mjs";

const root = fileURLToPath(new URL("..", import.meta.url));
const app = `${root}/app`;
const reportDir = `${root}/e2e/reports`;
const { host: hostPort, vite: vitePort } = PORTS.taskHost;
const hostOrigin = `http://127.0.0.1:${hostPort}`;
const origin = `http://127.0.0.1:${vitePort}`;
const token = "task-host-e2e-token";
const exactCommand = "cd app && npm run e2e:task-host";
/** A recognisable fake key the host is booted with. Nothing but `hirsel.toml`
 * may ever hold it whole: not a WebSocket frame, not the rendered DOM. */
const SENTINEL_KEY = "sk-or-e2e-SENTINEL-0123456789abcdef";
const startedAt = new Date().toISOString();
const children = [];
const logs = [];
const checks = [];
let dataDir;

function check(name, passed, evidence) {
  return recordCheck(checks, name, passed, evidence);
}

async function poll(label, probe, timeoutMs = 30_000) {
  return pollReady(label, probe, timeoutMs);
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

async function report(error = null) {
  return writeReport({
    root,
    reportDir,
    basename: "task-host-latest",
    title: "Host-backed adaptive Task browser report",
    report: {
    runner: "task-host",
    started_at: startedAt,
    command: exactCommand,
    services: [
      { kind: "rust-host-scripted-agent", command: "prebuilt target/debug/hirsel-host", cwd: "isolated temporary directory", port: hostPort },
      { kind: "vite-development-server", command: `npm exec vite -- --host 127.0.0.1 --port ${vitePort} --strictPort`, cwd: "app", port: vitePort },
    ],
    browser: { engine: "Chromium", headless: true, viewport: "1280x900", service_workers: "blocked" },
    status: "passed",
    screenshot: "e2e/reports/task-host-latest.png",
    checks,
    },
    error,
  });
}

async function main() {
  ({ dataDir } = await startHost({
    root,
    children,
    logs,
    port: hostPort,
    token,
    agent: "scripted",
    driver: "fake",
    dataDirPrefix: "hirsel-task-host-",
    // A key the host really stores, so the secrets gate below is measuring a
    // configured provider rather than an empty roster.
    env: { OPENROUTER_API_KEY: SENTINEL_KEY },
  }));
  startProcess(children, "npm", ["exec", "vite", "--", "--host", "127.0.0.1", "--port", String(vitePort), "--strictPort"], {
    cwd: app,
    env: {
      ...process.env,
      VITE_WS_URL: "same-origin",
      HIRSEL_DEV_PROXY_TARGET: `ws://127.0.0.1:${hostPort}`,
    },
  }, logs);
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
    // Everything the host says to this page, verbatim — the wire half of the
    // secrets gate below.
    const wsFrames = [];
    page.on("websocket", (socket) => {
      socket.on("framereceived", (frame) => wsFrames.push(frame.payload));
    });
    await page.goto(origin, { waitUntil: "domcontentloaded" });
    try {
      await appReady(page);
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
    // Exercise the real Settings → WS op → ConfigStore → broadcast path. The
    // reset half proves that the UI removes the override instead of persisting
    // a copied bundled prompt.
    const promptMarker = "Host e2e prompt override";
    await page.locator('[data-slot="phone-overflow-trigger"]').click();
    await page.getByRole("menuitem", { name: "Settings" }).click();
    const settingsPanel = page.locator('[data-slot="settings-panel"]');
    await settingsPanel.waitFor();
    // The prompt editors live under the Agents tab now; Settings lands on
    // Appearance, so the tab has to be taken before the editor exists.
    await settingsPanel.getByRole("tab", { name: "Agents", exact: true }).click();
    const promptEditor = page.getByRole("textbox", { name: "Main agent system prompt" });
    await promptEditor.fill(promptMarker);
    await page.getByRole("button", { name: "Save Main agent system prompt" }).click();
    await poll("Prompt override persistence", async () =>
      (await readFile(`${dataDir}/hirsel.toml`, "utf8")).includes(promptMarker));
    await page.getByRole("button", { name: "Reset Main agent system prompt to default" }).click();
    await poll("Prompt override removal", async () =>
      !(await readFile(`${dataDir}/hirsel.toml`, "utf8")).includes(promptMarker));
    check(
      "prompt edit round-trip",
      (await readFile(`${dataDir}/hirsel.toml`, "utf8")).includes("[agent]")
        && !(await readFile(`${dataDir}/hirsel.toml`, "utf8")).includes(promptMarker),
      "saved through WebSocket, broadcast back to Settings, then reset removed [agent].prompt",
    );

    // Secrets discipline, end to end: the host stores the key, the wire carries
    // only presence and a 4-character tail, and the page can render no more of
    // it than that tail. All three halves are asserted together, because a
    // gate that passes on an unconfigured provider proves nothing.
    await settingsPanel.getByRole("tab", { name: "Providers", exact: true }).click();
    const maskedTail = SENTINEL_KEY.slice(-4);
    const openRouterRow = settingsPanel.getByText(`Key set · ends ${maskedTail}`, { exact: false });
    const providersPanel = settingsPanel.locator('[role="tabpanel"]');
    // `exact` so the instance's own name matches and its base URL — which also
    // contains "openrouter" — does not: two matches is a strict-mode violation,
    // and a bare locator timeout would report nothing a reader could act on.
    await providersPanel.getByText("OpenRouter", { exact: true }).first().waitFor();
    // A missing tail is a FAILED row with its evidence, not a bare locator
    // timeout: the three parts below are the point of the gate, and a reader of
    // the report needs to see which one gave way.
    const tailShown = await openRouterRow
      .waitFor({ timeout: 10_000 })
      .then(() => true)
      .catch(() => false);
    const rosterText = (await providersPanel.innerText()).replace(/\s+/g, " ").slice(0, 240);
    const storedWhole = (await readFile(`${dataDir}/hirsel.toml`, "utf8")).includes(SENTINEL_KEY);
    const textFrames = wsFrames.filter((payload) => typeof payload === "string");
    const wireClean = !textFrames.join("\n").includes(SENTINEL_KEY);
    const markup = await page.evaluate(() => document.documentElement.innerHTML);
    // Nothing longer than the masked tail: the last 8 characters would already
    // be twice what the client is allowed to know.
    const domClean = !markup.includes(SENTINEL_KEY) && !markup.includes(SENTINEL_KEY.slice(-8));
    check(
      "provider key never leaves the host",
      storedWhole && wireClean && domClean && tailShown,
      `hirsel.toml holds the whole key=${storedWhole}; `
      + `no full key in ${textFrames.length} inbound WS frames=${wireClean}; `
      + `no key and no 8-character suffix in the rendered DOM=${domClean}; `
      + `Providers shows the masked tail "${maskedTail}"=${tailShown}; roster reads "${rosterText}"`,
    );
    await page.getByRole("button", { name: "Close Settings" }).click();
    // The seeded fixture is a blocking judgment, so the load-time rule already
    // opens it focused; clicking the chip would TOGGLE that focus back off.
    const ensureFocused = async (id) => {
      const open = await page.locator(`[data-slot="task-field"][data-task="${id}"]`).count();
      if (open === 1) return;
      await page.locator(`[data-task-id="${id}"]`).click();
      await page.locator(`[data-slot="task-card"]`).waitFor();
    };
    await ensureFocused(seeded.id);
    const seededHeading = page.getByRole("heading", { name: "A Task that changes with the work" });
    await seededHeading.waitFor();
    // The generated question renders inside the pinned card, above the Task's
    // own conversation and below its quiet identity.
    const cardFraming = await page.evaluate(() => {
      const card = document.querySelector('[data-slot="task-card"]');
      const identity = card?.querySelector("h2");
      const generated = card?.querySelector("h3");
      return {
        identity: identity?.textContent?.trim() ?? null,
        identityPx: identity ? Number.parseFloat(getComputedStyle(identity).fontSize) : 0,
        generated: generated?.textContent?.trim() ?? null,
        generatedPx: generated ? Number.parseFloat(getComputedStyle(generated).fontSize) : 0,
        position: card ? getComputedStyle(card).position : "missing",
        conversationBelow: Boolean(card && document.querySelector('[data-slot="task-conversation"]')
          && card.getBoundingClientRect().bottom
            <= document.querySelector('[data-slot="task-conversation"]').getBoundingClientRect().top + 1),
      };
    });
    check(
      "host Task opens as a pinned card",
      cardFraming.identity === "adaptive host proof"
        && cardFraming.generated === "A Task that changes with the work"
        && cardFraming.identityPx < cardFraming.generatedPx
        && cardFraming.position === "sticky"
        && cardFraming.conversationBelow,
      JSON.stringify(cardFraming),
    );
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

    // A cold load at the ROOT. Not a reload: the address bar tracks focus, so a
    // reload would carry the `/t/<id>` of the Task just settled, and a deep link
    // is a focus the Owner already chose — it outranks the landing heuristic by
    // design (the Task-margins suite proves that path).
    await page.goto(origin, { waitUntil: "domcontentloaded" });
    await appReady(page);
    // Settled work never wins load-time focus, so on a root load the field
    // rests ambient and the chip is the way back into the decided Task.
    const restedAmbient = await page.evaluate(() => ({
      taskFields: document.querySelectorAll('[data-slot="task-field"]').length,
      ambient: document.querySelectorAll('[data-slot="ambient-field"]').length,
      focused: document.querySelector('[data-slot="composer-shell"]')?.getAttribute("data-focused"),
    }));
    check(
      "settled Task does not steal load-time focus",
      restedAmbient.taskFields === 0 && restedAmbient.ambient === 1 && restedAmbient.focused === "false",
      JSON.stringify(restedAmbient),
    );
    await ensureFocused(seeded.id);
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
  await teardown(children, { dataDirs: [dataDir] });
  await report(failure);
}

if (failure) {
  console.error(failure);
  console.error(logs.slice(-16).join("\n"));
} else {
  console.log(`Host-backed adaptive Task browser gates passed (${checks.length}/${checks.length}).`);
}
