#!/usr/bin/env node
import { mkdir } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { chromium } from "../app/node_modules/playwright/index.mjs";
import { appReady, PORTS, pollReady, recordCheck, startProcess, teardown, writeReport as writeHarnessReport } from "./lib/harness.mjs";

const root = fileURLToPath(new URL("..", import.meta.url));
const app = `${root}/app`;
const reportDir = `${root}/e2e/reports`;
const { mock: mockPort, vite: vitePort } = PORTS.taskMargins;
const origin = `http://127.0.0.1:${vitePort}`;
const token = "task-margins-e2e";
const exactCommand = "cd app && npm run e2e:task-margins";
const startedAt = new Date().toISOString();
const children = [];
const logs = [];
const checks = [];
const screenshots = [];

async function captureScreenshot(page, name) {
  await mkdir(reportDir, { recursive: true });
  const relativePath = `e2e/reports/task-margins-${name}.png`;
  await page.screenshot({ path: `${root}/${relativePath}`, animations: "disabled" });
  screenshots.push(relativePath);
}

function check(name, passed, evidence) {
  return recordCheck(checks, name, passed, evidence);
}

async function poll(label, probe, timeoutMs = 15_000) {
  return pollReady(label, probe, timeoutMs);
}

async function writeReport(error = null) {
  return writeHarnessReport({
    root,
    reportDir,
    basename: "task-margins-latest",
    title: "Task Margins browser report",
    report: {
      runner: "task-margins",
      started_at: startedAt,
      command: exactCommand,
      services: [
        { kind: "seeded-websocket-mock", command: "node tools/mock-server.mjs", cwd: "app", port: mockPort },
        { kind: "vite-development-server", command: `npm exec vite -- --host 127.0.0.1 --port ${vitePort} --strictPort`, cwd: "app", port: vitePort },
      ],
      browser: {
        engine: "Chromium",
        headless: true,
        viewport_sequence: ["1440x1000", "390x844", "320x700"],
        color_scheme: "light",
        reduced_motion: "reduce",
        touch_emulation: true,
      },
      ports: { mock: mockPort, vite: vitePort },
      screenshots,
      status: "passed",
      checks,
    },
    error,
  });
}

async function main() {
  startProcess(children, process.execPath, ["tools/mock-server.mjs"], {
    cwd: app,
    env: { ...process.env, MOCK_PORT: String(mockPort), MOCK_REPLY_MS: "20" },
  }, logs);
  startProcess(children, "npm", ["exec", "vite", "--", "--host", "127.0.0.1", "--port", String(vitePort), "--strictPort"], {
    cwd: app,
    env: {
      ...process.env,
      VITE_WS_URL: "same-origin",
      HIRSEL_DEV_PROXY_TARGET: `ws://127.0.0.1:${mockPort}`,
    },
  }, logs);
  await poll("mock", async () => (await fetch(`http://127.0.0.1:${mockPort}/ready`)).status === 404);
  await poll("Vite", async () => (await fetch(origin)).ok);
  check("isolated services", true, `mock ${mockPort}; Vite ${vitePort}; readiness polled`);

  const browser = await chromium.launch({ headless: true });
  try {
    const context = await browser.newContext({
      viewport: { width: 1440, height: 1000 },
      hasTouch: true,
      serviceWorkers: "block",
      colorScheme: "light",
      reducedMotion: "reduce",
    });
    await context.addInitScript((value) => localStorage.setItem("hirsel.token", value), token);
    const page = await context.newPage();
    const browserErrors = [];
    const requestErrors = [];
    const sentFrames = [];
    page.on("console", (message) => {
      if (message.type() === "error") browserErrors.push(message.text());
    });
    page.on("pageerror", (error) => browserErrors.push(error.message));
    page.on("requestfailed", (request) => requestErrors.push(`${request.method()} ${request.url()}: ${request.failure()?.errorText}`));
    page.on("response", (response) => {
      if (response.status() >= 400) requestErrors.push(`${response.status()} ${response.url()}`);
    });
    page.on("websocket", (socket) => {
      socket.on("framesent", (event) => {
        try { sentFrames.push(JSON.parse(String(event.payload))); } catch { /* binary/non-JSON is irrelevant */ }
      });
    });
    const frameAfter = (start, predicate) => poll("outgoing frame", () => sentFrames.slice(start).find(predicate));

    await page.goto(origin, { waitUntil: "domcontentloaded" });
    await appReady(page);
    const textarea = page.getByRole("textbox", { name: "Message Hirsel" });
    const send = page.getByRole("button", { name: "Send", exact: true });
    const more = page.locator('[data-slot="phone-overflow-trigger"]');
    // There is no standing Processes button any more: home keeps one quiet ⋯
    // and every utility, Processes included, is summoned from behind it.
    const openProcesses = async () => {
      await more.click();
      await page.getByRole("menuitem", { name: /^Processes/ }).click();
    };
    const taskField = page.locator('[data-slot="task-field"]');
    const taskCard = page.locator('[data-slot="task-card"]');
    /** Focus a task without toggling one that is already open. */
    const ensureFocused = async (id) => {
      const open = await page.locator(`[data-slot="task-field"][data-task="${id}"]`).count();
      if (open === 1) return;
      await page.locator(`[data-task-id="${id}"]`).click();
      await taskCard.waitFor();
    };
    const ambientField = page.locator('[data-slot="ambient-field"]');
    const composerShell = page.locator('[data-slot="composer-shell"]');
    const question = page.getByRole("heading", { level: 3, name: "Ship build 4821 to production?" });

    // The seeded field carries one blocked task (deploy 4821), so the load-time
    // rule (DESIGN §"On load the most-needing task opens focused") has to land
    // on it: card pinned, composer tinted, chip pressed, no ambient field.
    let deploy = page.getByRole("button", { name: /deploy 4821, blocked on you/i });
    let auth = page.getByRole("button", { name: /auth pr, needs you/i });
    await question.waitFor();
    check(
      "most-needing task opens focused",
      await taskField.getAttribute("data-task") === "1"
        && (await taskCard.count()) === 1
        && (await ambientField.count()) === 0
        && await composerShell.getAttribute("data-focused") === "true"
        && await deploy.getAttribute("aria-pressed") === "true"
        && await auth.getAttribute("aria-pressed") === "false"
        && (await page.locator('[data-slot="task-index"]').count()) === 1,
      "blocked deploy 4821 wins load-time focus over the needs-you and done tasks",
    );

    // Esc is the deliberate zoom-out, and ambient carries no title, no task
    // field, no composer label and no placeholder.
    await page.keyboard.press("Escape");
    await ambientField.waitFor();
    check(
      "escape rests the field ambient",
      (await ambientField.count()) === 1
        && (await page.locator('[data-slot="task-index"]').count()) === 1
        && (await taskField.count()) === 0
        && (await taskCard.count()) === 0
        && await composerShell.getAttribute("data-focused") === "false"
        && await deploy.getAttribute("aria-pressed") === "false"
        && await textarea.getAttribute("placeholder") === null
        && (await page.getByText("Global Hirsel", { exact: true }).count()) === 0,
      "one Tasks inventory; no title, task field, composer label, or placeholder",
    );
    const inventoryCounts = await Promise.all(["deploy 4821", "auth pr", "nightly backup"].map(async (name) =>
      page.getByRole("button", { name: new RegExp(`^${name},`, "i") }).count()));
    check("one task identity inventory", inventoryCounts.every((count) => count === 1), `button counts ${inventoryCounts.join(",")}`);

    let startFrame = sentFrames.length;
    await textarea.fill("Summarize what matters across everything");
    await send.click();
    const globalFrame = await frameAfter(startFrame, (frame) => frame.type === "send_message" && frame.body === "Summarize what matters across everything");
    check("exact global frame", globalFrame.ref === null && (!globalFrame.mentions || globalFrame.mentions.length === 0), JSON.stringify(globalFrame));

    await deploy.click();
    await question.waitFor();
    // The identity is quiet CONTEXT inside the pinned card now (it used to be
    // sr-only): it renders once, is visible, and the machine-authored question
    // still visibly leads it in both size and weight.
    const dive = await page.evaluate(() => {
      const card = document.querySelector('[data-slot="task-card"]');
      const field = document.querySelector('[data-slot="task-field"]');
      const identity = card?.querySelector("h2");
      const generated = card?.querySelector("h3");
      const identityStyle = identity ? getComputedStyle(identity) : null;
      return {
        fieldText: field?.textContent ?? "",
        identityText: identity?.textContent?.trim() ?? "",
        identityPx: identityStyle ? Number.parseFloat(identityStyle.fontSize) : 0,
        identityColor: identityStyle?.color ?? "",
        identityVisible: Boolean(identity && identity.getBoundingClientRect().height > 0),
        identityCount: field?.querySelectorAll("h2").length ?? 0,
        generatedPx: generated ? Number.parseFloat(getComputedStyle(generated).fontSize) : 0,
        generatedColor: generated ? getComputedStyle(generated).color : "",
        cardPosition: card ? getComputedStyle(card).position : "missing",
      };
    });
    check(
      "generated question leads its quiet identity",
      dive.identityCount === 1 && dive.identityVisible && dive.identityText === "deploy 4821"
        && dive.generatedPx >= 28 && dive.identityPx < dive.generatedPx
        && dive.identityColor !== dive.generatedColor
        && !dive.fieldText.includes("blocked on you"),
      `h2 "${dive.identityText}" ${dive.identityPx}px ${dive.identityColor}; h3 ${dive.generatedPx}px ${dive.generatedColor}; status lives in the chip`,
    );
    check(
      "task focus visible",
      await deploy.getAttribute("aria-pressed") === "true"
        && await composerShell.getAttribute("data-focused") === "true"
        && dive.cardPosition === "sticky"
        && (await page.locator('[data-slot="task-scope"]').count()) === 0,
      `selected task row, pinned ${dive.cardPosition} card, and composer tone carry focus without a mode label`,
    );
    // One column at the reading measure in BOTH states: the card, the
    // conversation under it and the standing composer share edges, and the
    // composer does not move when focus clears.
    const columnGeometry = await page.evaluate(() => {
      const box = (selector) => {
        const element = document.querySelector(selector);
        if (!element) return null;
        const rect = element.getBoundingClientRect();
        return { left: Math.round(rect.left), width: Math.round(rect.width) };
      };
      return {
        card: box('[data-slot="task-card"]'),
        conversation: box('[data-slot="task-conversation"]'),
        composer: box('[data-slot="composer-shell"]'),
      };
    });
    check(
      "one measure-wide column",
      columnGeometry.card.left === columnGeometry.conversation.left
        && columnGeometry.card.width === columnGeometry.conversation.width
        && columnGeometry.card.width <= 820
        && columnGeometry.composer.width <= 820,
      JSON.stringify(columnGeometry),
    );
    startFrame = sentFrames.length;
    await textarea.fill("What would make this unsafe?");
    await send.click();
    const taskFrame = await frameAfter(startFrame, (frame) => frame.type === "send_message" && frame.body === "What would make this unsafe?");
    check("exact task frame", taskFrame.ref === 2 && taskFrame.mentions?.length === 1 && taskFrame.mentions[0] === 1, JSON.stringify(taskFrame));

    const taskScroll = page.locator('[data-slot="task-scroll"]');
    await taskScroll.evaluate((element) => { element.scrollTop = 14; });
    await deploy.click();
    const ambientComposer = await composerShell.evaluate((element) => {
      const rect = element.getBoundingClientRect();
      return { left: Math.round(rect.left), width: Math.round(rect.width) };
    });
    check(
      "focus clears to ambient",
      (await taskField.count()) === 0
        && (await taskCard.count()) === 0
        && (await ambientField.count()) === 1
        && await composerShell.getAttribute("data-focused") === "false"
        && await deploy.getAttribute("aria-pressed") === "false"
        && ambientComposer.left === columnGeometry.composer.left
        && ambientComposer.width === columnGeometry.composer.width,
      `activating the focused task removes focus without naming a mode or moving the composer (${JSON.stringify(ambientComposer)})`,
    );
    startFrame = sentFrames.length;
    await textarea.fill("Compare this with the auth task");
    await send.click();
    const asideFrame = await frameAfter(startFrame, (frame) => frame.type === "send_message" && frame.body === "Compare this with the auth task");
    check("exact ambient frame", asideFrame.ref === null && (!asideFrame.mentions || asideFrame.mentions.length === 0), JSON.stringify(asideFrame));
    await deploy.click();
    check(
      "focus return",
      await question.isVisible()
        && await deploy.getAttribute("aria-pressed") === "true"
        && await composerShell.getAttribute("data-focused") === "true",
      "same task is one action away from the ambient whole",
    );

    const deployMargin = page.locator('[data-slot="conversation"]');
    const deployAttribution = {
      anchor: await deployMargin.getByText(/Deploy of build 4821/).count(),
      probe: await deployMargin.getByText(/What would make this unsafe/).count(),
      auth: await deployMargin.getByText(/auth refactor branch/).count(),
      text: (await deployMargin.textContent())?.slice(0, 500),
    };
    check(
      "interleaved deploy attribution",
      deployAttribution.anchor === 1 && deployAttribution.probe >= 1 && deployAttribution.auth === 0,
      JSON.stringify(deployAttribution),
    );
    await auth.click();
    const authMargin = page.locator('[data-slot="conversation"]');
    check(
      "interleaved auth attribution",
      (await authMargin.getByText(/auth refactor branch/).count()) === 1
        && (await authMargin.getByText(/Deploy of build 4821/).count()) === 0
        && (await authMargin.getByText(/What would make this unsafe/).count()) === 0,
      "auth remains isolated after interleaved global and deploy sends",
    );

    startFrame = sentFrames.length;
    await page.getByRole("textbox", { name: "Reviewer" }).fill("Sam");
    await page.getByRole("button", { name: "Open PR" }).click();
    const submitFrame = await frameAfter(startFrame, (frame) => frame.type === "event_action" && frame.event_id === 2);
    await page.getByText("Task decided").waitFor();
    check("exact form payload", submitFrame.action === "submit" && submitFrame.data?.reviewer === "Sam", JSON.stringify(submitFrame));

    deploy = page.getByRole("button", { name: /deploy 4821, blocked on you/i });
    await deploy.click();
    const deployIdentity = await taskField.getAttribute("data-task");
    const firstChoice = page.getByRole("button", { name: /Ship now/ });
    const choiceMetrics = await firstChoice.evaluate((element) => ({
      height: element.getBoundingClientRect().height,
      recommendedRadius: getComputedStyle(element.querySelector("span:last-child") ?? element).borderRadius,
    }));
    check("flat choice target", choiceMetrics.height >= 44 && choiceMetrics.recommendedRadius === "0px", JSON.stringify(choiceMetrics));
    startFrame = sentFrames.length;
    await firstChoice.click();
    const advanceFrame = await frameAfter(startFrame, (frame) => frame.type === "event_action" && frame.event_id === 1);
    const canaryHeading = page.getByRole("heading", { level: 3, name: "Canary is healthy. Promote production?" });
    await canaryHeading.waitFor();
    check(
      "adaptive same-task stage",
      advanceFrame.action === "advance" && advanceFrame.data?.choice === "A"
        && await taskField.getAttribute("data-task") === deployIdentity
        && (await page.getByText("Task decided").count()) === 0,
      `${JSON.stringify(advanceFrame)}; task ${deployIdentity} stayed open`,
    );
    check(
      "adaptive status instrument",
      (await taskField.locator('[data-status="success"]').count()) === 1
        && (await taskField.getByText(/5% canary · 0 errors · p95 184ms/).count()) === 1,
      "success literal plus canary metrics present before promotion",
    );
    startFrame = sentFrames.length;
    await page.getByRole("button", { name: /Promote to 100%/ }).click();
    const promoteFrame = await frameAfter(startFrame, (frame) => frame.type === "event_action" && frame.event_id === 1);
    await page.getByText("Task decided").waitFor();
    check("exact promotion payload", promoteFrame.action === "choose" && promoteFrame.data?.choice === "A", JSON.stringify(promoteFrame));
    await page.reload({ waitUntil: "domcontentloaded" });
    await appReady(page);
    // Every seeded task is settled at this point, so nothing ranks as needing
    // the Owner and the same load-time rule must leave the field ambient —
    // landing on already-decided work would be the opposite of most-needing.
    const settledChips = await page.locator('[data-slot="task-index"] [data-task-id]').evaluateAll(
      (nodes) => nodes.map((node) => node.getAttribute("aria-label")),
    );
    check(
      "nothing needing rests ambient",
      settledChips.length === 3 && settledChips.every((label) => /, done$/.test(label ?? ""))
        && (await ambientField.count()) === 1
        && (await taskField.count()) === 0
        && await composerShell.getAttribute("data-focused") === "false",
      `all-settled load stays ambient; chips ${JSON.stringify(settledChips)}`,
    );
    await page.getByRole("button", { name: /deploy 4821, done/i }).click();
    await page.getByText("Task decided").waitFor();
    startFrame = sentFrames.length;
    await page.getByRole("button", { name: "Reopen" }).click();
    const reopenFrame = await frameAfter(startFrame, (frame) => frame.type === "event_action" && frame.event_id === 1);
    await canaryHeading.waitFor();
    check("settle reload reopen", reopenFrame.action === "reopen", "settled stage survived reload; reopen restored prior canary action stage");

    await page.getByRole("button", { name: /nightly backup, done/i }).click();
    check(
      "non-color status",
      (await taskField.locator('[data-status="success"]').count()) === 1
        && (await taskField.getByText("success", { exact: true }).count()) === 1
        && (await taskField.getByText("0 errors", { exact: true }).count()) === 1,
      "literal success and 0 errors accompany the dot",
    );

    // A chip is acted on through its own ⋯ menu — never a hover ×. Delete is
    // optimistic and its toast carries the honest Undo.
    const nightlyActions = page.getByRole("button", { name: "Actions for nightly backup" });
    await nightlyActions.click();
    const deleteItem = page.getByRole("menuitem", { name: "Delete", exact: true });
    await deleteItem.waitFor();
    const chipCross = await page.locator('[data-slot="task-index"] button[aria-label^="Remove"], [data-slot="task-index"] button[aria-label^="Dismiss"]').count();
    await deleteItem.click();
    await page.locator('[data-task-id="3"]').waitFor({ state: "detached" });
    const afterDelete = {
      chips: await page.locator('[data-slot="task-index"] [data-task-id]').count(),
      taskFields: await taskField.count(),
      focused: await composerShell.getAttribute("data-focused"),
    };
    const undo = page.getByRole("button", { name: "Undo", exact: true });
    await undo.click();
    await page.locator('[data-task-id="3"]').waitFor();
    check(
      "chip menu deletes with undo",
      chipCross === 0 && afterDelete.chips === 2 && afterDelete.taskFields === 0
        && afterDelete.focused === "false"
        && (await page.locator('[data-slot="task-index"] [data-task-id]').count()) === 3,
      `no hover ×; delete swept the focused chip to ambient (${JSON.stringify(afterDelete)}) and Undo restored it`,
    );

    await page.getByRole("button", { name: /deploy 4821, blocked on you/i }).click();
    await textarea.fill("compare blast radius");
    const utilityBefore = await page.evaluate(() => ({
      task: document.querySelector('[data-slot="task-field"]')?.getAttribute("data-task"),
      focused: document.querySelector('[data-slot="composer-shell"]')?.getAttribute("data-focused"),
      draft: document.querySelector('[data-composer="main"]')?.value,
      scroll: document.querySelector('[data-slot="task-scroll"]')?.scrollTop,
    }));
    await more.click();
    const canvasCount = await page.getByRole("menuitem", { name: "Canvas", exact: true }).count();
    const overflowLabel = await more.getAttribute("aria-label");
    const processesRow = page.getByRole("menuitem", { name: /^Processes/ });
    const processesRowLabel = (await processesRow.textContent())?.trim();
    await processesRow.click();
    const processPanel = page.locator('[data-slot="processes-panel"]');
    await processPanel.waitFor();
    check(
      "processes summoned from the overflow",
      overflowLabel === "More actions, 2 processes running"
        && processesRowLabel === "Processes2"
        && (await page.locator('[data-slot="processes-trigger"]').count()) === 0,
      `⋯ reports the running count (${overflowLabel}); row "${processesRowLabel}"; no standing Processes button`,
    );
    check(
      "processes temporary",
      await processPanel.getAttribute("role") === "complementary"
        && (await processPanel.getByText(/^Running \(2\)$/).count()) === 1
        && (await processPanel.getByText(/^Finished \(1\)$/).count()) === 1
        && (await processPanel.locator('[data-slot="task-index"]').count()) === 0,
      "desktop inspector has running/finished groups and no nested destination",
    );
    // Closed through the pane header's own ×, activated from the keyboard: the
    // floating ⋯ overlaps that corner of the docked desktop inspector once
    // touch emulation widens both targets to 44px, so a synthetic pointer click
    // lands on the ⋯ instead. The Esc route to the same close is asserted
    // separately as the run's last gate.
    await processPanel.getByRole("button", { name: "Close Processes" }).press("Enter");
    await processPanel.waitFor({ state: "detached" });
    const utilityAfter = await page.evaluate(() => ({
      task: document.querySelector('[data-slot="task-field"]')?.getAttribute("data-task"),
      focused: document.querySelector('[data-slot="composer-shell"]')?.getAttribute("data-focused"),
      draft: document.querySelector('[data-composer="main"]')?.value,
      scroll: document.querySelector('[data-slot="task-scroll"]')?.scrollTop,
      activeSlot: document.activeElement?.getAttribute("data-slot"),
    }));
    check("desktop utility continuity", utilityAfter.task === utilityBefore.task && utilityAfter.focused === utilityBefore.focused && utilityAfter.draft === utilityBefore.draft && utilityAfter.scroll === utilityBefore.scroll, JSON.stringify(utilityAfter));

    await openProcesses();
    const runningSubagent = processPanel.locator('[data-slot="process-row"][data-kind="subagent"][data-state="running"] > button');
    await runningSubagent.click();
    await processPanel.getByRole("button", { name: "Ask Hirsel to stop" }).click();
    await processPanel.waitFor({ state: "detached" });
    const stopDraft = await textarea.inputValue();
    check(
      "ask-to-stop context",
      stopDraft === "stop process proc-3 (Check the release candidate against the deployme…)"
        && await taskField.getAttribute("data-task") === "1"
        && await composerShell.getAttribute("data-focused") === "true",
      stopDraft,
    );

    await more.click();
    await page.getByRole("menuitem", { name: "Settings", exact: true }).click();
    const settingsPanel = page.locator('[data-slot="settings-panel"]');
    await settingsPanel.waitFor();
    await settingsPanel.getByRole("radio", { name: "Dark" }).click();
    check("settings theme", await page.locator("html.dark").count() === 1, "Dark toggled in temporary Settings inspector");
    await settingsPanel.getByRole("button", { name: "Close Settings" }).press("Enter");
    await settingsPanel.waitFor({ state: "detached" });
    check("settings continuity", await taskField.getAttribute("data-task") === "1" && await textarea.inputValue() === stopDraft, "task focus and composer survive Settings");
    await more.click();
    await page.getByRole("menuitem", { name: "Model settings", exact: false }).click();
    await settingsPanel.waitFor();
    const modelLanding = await page.locator("#settings-models").evaluate((element) => {
      const panel = element.closest('[data-slot="settings-panel"]');
      const rect = element.getBoundingClientRect();
      const panelRect = panel?.getBoundingClientRect();
      return Boolean(panelRect && rect.top >= panelRect.top && rect.top <= panelRect.bottom);
    });
    check("model settings landing", modelLanding, "Models heading is within the opened inspector viewport");
    await settingsPanel.getByRole("button", { name: "Close Settings" }).press("Enter");
    check("canvas honest N/A", canvasCount === 0, "Canvas not seeded; task focus and draft continuity retained");

    await settingsPanel.waitFor({ state: "detached" });
    // A closing utility restores focus a frame after it unmounts, so wait for
    // that restoration to settle before claiming the keyboard — otherwise the
    // chord below lands on a chip that the teardown then takes back.
    await poll("focus settles after the utility closes", async () => {
      const before = await page.evaluate(() => document.activeElement?.tagName ?? null);
      await new Promise((resolve) => setTimeout(resolve, 150));
      const after = await page.evaluate(() => document.activeElement?.tagName ?? null);
      return before === after ? after : null;
    });
    await page.keyboard.press("g");
    await page.keyboard.press("t");
    await poll("task index takes the keyboard", async () =>
      page.evaluate(() => document.activeElement?.getAttribute("data-task-id")));
    const focusId = () => page.evaluate(() => {
      const scroller = document.querySelector('[data-slot="task-scroll"]');
      return {
        focus: document.activeElement?.getAttribute("data-task-id"),
        selected: document.querySelector('[data-task-id][aria-pressed="true"]')?.getAttribute("data-task-id"),
        // Both states now open at the newest line, because the task itself is a
        // pinned card rather than a subject that scrolling away would lose.
        atBottom: scroller
          ? scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight <= 2
          : null,
      };
    });
    const sequence = [];
    for (const key of ["ArrowDown", "End", "Home", "ArrowUp", "ArrowRight"]) {
      await page.keyboard.press(key);
      sequence.push({ key, ...(await poll(`roving ${key} settles`, async () => {
        const item = await focusId();
        return item.atBottom && item.focus ? item : null;
      }, 5_000)) });
    }
    check(
      "full roving navigation",
      sequence.every((item) => item.focus === item.selected && item.atBottom === true)
        && sequence.map((item) => item.focus).join(",") === "2,3,1,3,1",
      JSON.stringify(sequence),
    );
    await page.keyboard.press("/");
    check("slash focuses composer", await textarea.evaluate((element) => element === document.activeElement), "standing composer owns focus");
    await textarea.fill("");
    await page.keyboard.press("?");
    check("question mark is ordinary draft", await textarea.inputValue() === "?" && (await page.getByRole("heading", { name: "Keyboard shortcuts" }).count()) === 0, "composer contains ? and no sheet opened");
    await page.locator('[data-task-id="1"]').focus();
    await page.keyboard.press("?");
    const help = page.getByRole("heading", { name: "Keyboard shortcuts" });
    await help.waitFor();
    check("shortcut truth", (await page.getByText("⌘↵", { exact: true }).count()) === 0, "help opens outside input and claims no generated-submit accelerator");
    await page.keyboard.press("Escape");
    await page.keyboard.press("g");
    await page.keyboard.press("p");
    await processPanel.waitFor();
    await page.keyboard.press("Escape");
    await page.keyboard.press("g");
    await page.keyboard.press("s");
    await settingsPanel.waitFor();
    await page.keyboard.press("Escape");
    check("utility shortcuts remain usable", (await processPanel.count()) === 0 && (await settingsPanel.count()) === 0, "g p and g s open; Escape closes");
    await captureScreenshot(page, "desktop");

    await page.setViewportSize({ width: 390, height: 844 });
    await textarea.fill("draft survives phone utilities");
    await openProcesses();
    await processPanel.waitFor();
    const phoneRole = await processPanel.getAttribute("role");
    const closeProcesses = processPanel.getByRole("button", { name: "Close Processes" });
    const closeProcessBox = await closeProcesses.boundingBox();
    let initialPhoneFocus;
    try {
      initialPhoneFocus = await poll("phone trap initial focus", async () =>
        processPanel.evaluate((panel) => {
          const active = document.activeElement;
          if (!(active instanceof HTMLElement) || !panel.contains(active)) return null;
          const box = active.getBoundingClientRect();
          if (box.width <= 0 || box.height <= 0) return null;
          return {
            label: active.getAttribute("aria-label") ?? active.textContent?.trim() ?? active.tagName,
            width: box.width,
            height: box.height,
          };
        }));
    } catch (error) {
      const diagnostic = await processPanel.evaluate((panel) => {
        const active = document.activeElement;
        const activeBox = active instanceof HTMLElement ? active.getBoundingClientRect() : null;
        return {
          active: active instanceof HTMLElement
            ? active.getAttribute("aria-label") ?? active.textContent?.trim() ?? active.tagName
            : null,
          activeInside: active instanceof HTMLElement && panel.contains(active),
          activeBox: activeBox ? [activeBox.width, activeBox.height] : null,
          candidates: [...panel.querySelectorAll("button, input, textarea, select, [tabindex]")].map((element) => {
            const box = element.getBoundingClientRect();
            return {
              element: element.getAttribute("aria-label") ?? element.textContent?.trim() ?? element.tagName,
              box: [box.width, box.height],
              display: getComputedStyle(element).display,
              visibility: getComputedStyle(element).visibility,
            };
          }),
        };
      });
      throw new Error(`${error.message}; ${JSON.stringify(diagnostic)}`);
    }
    const visibleFocusableCount = await processPanel.evaluate((panel) => {
      const selector = 'a[href], button:not([disabled]), textarea:not([disabled]), input:not([disabled]):not([type="hidden"]), select:not([disabled]), [tabindex]:not([tabindex="-1"])';
      return [...panel.querySelectorAll(selector)].filter((element) => {
        if (!(element instanceof HTMLElement)) return false;
        if (element.closest('[hidden], [inert], [aria-hidden="true"]')) return false;
        const style = getComputedStyle(element);
        const box = element.getBoundingClientRect();
        return style.display !== "none" && style.visibility !== "hidden"
          && box.width > 0 && box.height > 0;
      }).length;
    });
    const tabCycle = [];
    for (let index = 0; index < visibleFocusableCount; index += 1) {
      await page.keyboard.press("Tab");
      tabCycle.push(await poll(`phone Tab ${index + 1} containment`, async () =>
        processPanel.evaluate((panel) => {
          const active = document.activeElement;
          const box = active instanceof HTMLElement ? active.getBoundingClientRect() : null;
          const state = {
            inside: active instanceof HTMLElement && panel.contains(active),
            visible: Boolean(box && box.width > 0 && box.height > 0),
            active: active instanceof HTMLElement
              ? active.getAttribute("aria-label") ?? active.textContent?.trim() ?? active.tagName
              : null,
            tag: active instanceof HTMLElement ? active.tagName : null,
            connected: active instanceof HTMLElement ? active.isConnected : false,
          };
          return state.inside && state.visible ? state : null;
        }), 1_000));
    }
    await page.keyboard.press("Shift+Tab");
    const reverseWrapped = await poll("phone reverse Tab containment", async () =>
      processPanel.evaluate((panel) => {
        const active = document.activeElement;
        const box = active instanceof HTMLElement ? active.getBoundingClientRect() : null;
        return active instanceof HTMLElement && panel.contains(active)
          && Boolean(box && box.width > 0 && box.height > 0);
      }), 1_000);
    const trapped = visibleFocusableCount > 0
      && tabCycle.every(({ inside, visible }) => inside && visible)
      && reverseWrapped;
    await closeProcesses.click();
    await processPanel.waitFor({ state: "detached" });
    // Focus returns to the affordance that summoned the panel — the standing ⋯.
    const phoneFocusRestored = await more.evaluate(
      (element) => element === document.activeElement,
    );
    check(
      "phone modal focus",
      phoneRole === "dialog" && phoneFocusRestored && trapped,
      `role=${phoneRole}; initial=${JSON.stringify(initialPhoneFocus)}; visible-controls=${visibleFocusableCount}; cycle=${JSON.stringify(tabCycle)}; reverse=${reverseWrapped}; trapped=${trapped}; restored=${phoneFocusRestored}`,
    );

    // Touch targets and phone containment are both statements about the FOCUSED
    // field (the card's own choices, the single column under it), so the task
    // has to be open for either to mean anything.
    await ensureFocused(1);
    const touchTargets = await page.evaluate(() => {
      const selectors = {
        tasks: '[data-slot="task-index"] button',
        choices: '[data-slot="task-field"] button',
        send: 'button[aria-label="Send"]',
        overflow: '[data-slot="phone-overflow-trigger"]',
        attach: 'button[aria-label="Attach files"]',
        sendOptions: 'button[aria-label="More send options"]',
      };
      return Object.fromEntries(Object.entries(selectors).map(([name, selector]) => {
        const elements = [...document.querySelectorAll(selector)];
        return [name, elements.map((element) => Math.min(element.getBoundingClientRect().width, element.getBoundingClientRect().height))];
      }));
    });
    const targetsPass = Object.values(touchTargets).flat().every((size) => size >= 44) && (closeProcessBox?.height ?? 0) >= 44;
    check("all named 44px targets", targetsPass, JSON.stringify({ ...touchTargets, utilityClose: closeProcessBox?.height ?? 0 }));
    const phoneContainment = await page.evaluate(() => {
      const strip = document.querySelector('[data-slot="task-index"]');
      const conversation = document.querySelector('[data-slot="task-conversation"]');
      const card = document.querySelector('[data-slot="task-card"]');
      return {
        viewport: innerWidth,
        document: document.documentElement.scrollWidth,
        stripLeft: strip?.getBoundingClientRect().left,
        stripRight: strip?.getBoundingClientRect().right,
        columns: conversation ? getComputedStyle(conversation).gridTemplateColumns : "missing",
        cardWidth: card ? Math.round(card.getBoundingClientRect().width) : null,
        conversationWidth: conversation ? Math.round(conversation.getBoundingClientRect().width) : null,
      };
    });
    check(
      "390px containment",
      phoneContainment.document <= phoneContainment.viewport
        && phoneContainment.stripLeft >= 0 && phoneContainment.stripRight <= 390
        && !phoneContainment.columns.includes(" ")
        && phoneContainment.cardWidth === phoneContainment.conversationWidth,
      JSON.stringify(phoneContainment),
    );
    const phoneTask = await taskField.getAttribute("data-task");
    const phoneTaskButton = page.locator(`[data-task-id="${phoneTask}"]`);
    await phoneTaskButton.click();
    const phoneAmbient = (await ambientField.count()) === 1
      && await composerShell.getAttribute("data-focused") === "false";
    await phoneTaskButton.click();
    check(
      "phone focus continuity",
      phoneAmbient && await taskField.getAttribute("data-task") === phoneTask
        && await composerShell.getAttribute("data-focused") === "true",
      "task clears to ambient and returns through the same strip item",
    );
    await captureScreenshot(page, "phone-390");

    await page.setViewportSize({ width: 320, height: 700 });
    await textarea.fill("x".repeat(180));
    const narrow = await page.evaluate(() => {
      const input = document.querySelector('[data-composer="main"]');
      const composer = document.querySelector('[data-slot="composer-shell"]');
      const strip = document.querySelector('[data-slot="task-index"]');
      const card = document.querySelector('[data-slot="task-card"]');
      const h2 = card?.querySelector("h2");
      const h3 = card?.querySelector("h3");
      return {
        viewport: innerWidth,
        document: document.documentElement.scrollWidth,
        inputScroll: input?.scrollHeight,
        inputClient: input?.clientHeight,
        composerRight: composer?.getBoundingClientRect().right,
        stripScroll: strip?.scrollWidth,
        stripClient: strip?.clientWidth,
        identityCount: document.querySelectorAll('[data-slot="task-field"] h2').length,
        identityRight: h2 ? h2.getBoundingClientRect().right : null,
        identityPx: h2 ? Number.parseFloat(getComputedStyle(h2).fontSize) : 0,
        generatedPx: h3 ? Number.parseFloat(getComputedStyle(h3).fontSize) : 0,
        cardHeight: card ? card.getBoundingClientRect().height : null,
        // The declared ceiling itself, rather than a recomputed 40dvh: the
        // dynamic viewport unit does not always re-resolve after a synthetic
        // viewport resize, and the contract being asserted is that the card
        // stops at its cap and leaves the conversation on screen.
        cardCap: card ? Number.parseFloat(getComputedStyle(card).maxHeight) : null,
        cardOverflowY: card ? getComputedStyle(card).overflowY : null,
        conversationTop: document.querySelector('[data-slot="task-conversation"]')
          ?.getBoundingClientRect().top ?? null,
        viewportHeight: innerHeight,
      };
    });
    check(
      "180-char 320px draft containment",
      narrow.document <= narrow.viewport && narrow.composerRight <= 320
        && narrow.inputScroll >= narrow.inputClient && narrow.stripScroll > narrow.stripClient,
      JSON.stringify(narrow),
    );
    check(
      "single quiet identity inside a capped card",
      narrow.identityCount === 1 && narrow.identityRight <= 320
        && narrow.identityPx < narrow.generatedPx
        && Number.isFinite(narrow.cardCap) && narrow.cardHeight <= narrow.cardCap + 1
        && narrow.cardOverflowY === "auto"
        && narrow.conversationTop < narrow.viewportHeight,
      JSON.stringify(narrow),
    );
    await captureScreenshot(page, "narrow-320");

    // The Esc route out of a utility, DESIGN §4: "closing it returns to the
    // same focus state", and keymap's own Esc ladder yields the key to an open
    // overlay's trap. Deliberately the LAST gate so every other gate above is
    // still executed and reported when this one is red.
    await page.setViewportSize({ width: 1440, height: 1000 });
    await ensureFocused(1);
    // Read through one evaluate rather than locator getters: a lost focus must
    // land as a recorded FAIL row, not as a locator timeout.
    const focusState = () => page.evaluate(() => ({
      task: document.querySelector('[data-slot="task-field"]')?.getAttribute("data-task") ?? null,
      focused: document.querySelector('[data-slot="composer-shell"]')?.getAttribute("data-focused") ?? null,
    }));
    const beforeEscapeClose = await focusState();
    await openProcesses();
    await processPanel.waitFor();
    await page.keyboard.press("Escape");
    await processPanel.waitFor({ state: "detached" });
    const afterEscapeClose = await focusState();
    check(
      "escape closes the utility only",
      afterEscapeClose.task === beforeEscapeClose.task
        && afterEscapeClose.focused === beforeEscapeClose.focused,
      `before ${JSON.stringify(beforeEscapeClose)}; after ${JSON.stringify(afterEscapeClose)}`,
    );
    check("browser console and requests", browserErrors.length === 0 && requestErrors.length === 0, [...browserErrors, ...requestErrors].join("; ") || "no console, page, response, or request failures");
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
  await teardown(children);
  await writeReport(failure);
}

if (failure) {
  console.error(failure);
  console.error(logs.slice(-12).join("\n"));
} else {
  console.log(`Task Margins browser gates passed (${checks.length}/${checks.length}).`);
  console.log(`Report: ${reportDir}/task-margins-latest.md`);
}
