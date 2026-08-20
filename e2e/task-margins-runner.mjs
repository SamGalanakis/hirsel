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
    // ONE column at the reading measure in BOTH states, and the composer is
    // that column's floor: card, conversation, the prose inside it and the
    // capsule share BOTH edges exactly — no ~150px overhang past the last line
    // of text — and the column is centred in the pane rather than pinned left.
    const MEASURE = 672; // --container-measure: 42rem at the 16px root size
    const columnGeometry = await page.evaluate(() => {
      const box = (selector) => {
        const element = document.querySelector(selector);
        if (!element) return null;
        const rect = element.getBoundingClientRect();
        return {
          left: Math.round(rect.left),
          width: Math.round(rect.width),
          height: Math.round(rect.height),
          top: Math.round(rect.top),
        };
      };
      const pane = document.querySelector('[data-slot="task-scroll"]')?.parentElement;
      return {
        card: box('[data-slot="task-card"]'),
        conversation: box('[data-slot="task-conversation"]'),
        // The actual reading column: the flow the messages are laid out in.
        // This is the edge the Owner sees the text end on.
        prose: box('[data-slot="conversation"] > div'),
        composer: box('[data-slot="composer-shell"]'),
        paneCentre: pane ? Math.round(pane.getBoundingClientRect().left + pane.getBoundingClientRect().width / 2) : null,
      };
    });
    const columnParts = [columnGeometry.card, columnGeometry.conversation, columnGeometry.prose, columnGeometry.composer];
    check(
      "one measure-wide column",
      columnParts.every((part) => part && part.left === columnGeometry.card.left)
        && columnParts.every((part) => part.width === MEASURE)
        // Centred in the pane: the column used to sit at the frame's left edge
        // with ~360px of dead space beside it, which is what "not centered"
        // meant. Half-pixel rounding is the only slack allowed.
        && Math.abs(columnGeometry.composer.left + columnGeometry.composer.width / 2 - columnGeometry.paneCentre) <= 1,
      JSON.stringify(columnGeometry),
    );
    // A touch context, so the capsule keeps its 44px row and its Send button:
    // one text row plus the capsule's own padding, never the old 60px slab.
    check(
      "capsule rests one row high",
      columnGeometry.composer.height === 52,
      `resting composer height ${columnGeometry.composer.height}px (touch context: 44px row + py-1)`,
    );
    startFrame = sentFrames.length;
    await textarea.fill("What would make this unsafe?");
    await send.click();
    const taskFrame = await frameAfter(startFrame, (frame) => frame.type === "send_message" && frame.body === "What would make this unsafe?");
    check("exact task frame", taskFrame.ref === 2 && taskFrame.mentions?.length === 1 && taskFrame.mentions[0] === 1, JSON.stringify(taskFrame));

    // ---- Task refs: citing one Task from inside another ---------------------
    // `#` opens the picker under the caret, filters on the name, inserts the
    // Task's ref, and the send carries that citation ALONGSIDE the focused
    // Task's own mention — focus decides where the line lives, the ref decides
    // what it cites.
    const refPicker = page.locator('[data-slot="task-ref-picker"]');
    const refOptions = page.locator("[data-task-ref-option]");
    startFrame = sentFrames.length;
    await textarea.click();
    await textarea.pressSequentially("hold until #");
    await refPicker.waitFor();
    const wholeField = await refOptions.evaluateAll((rows) => rows.map((row) => row.dataset.taskRefOption));
    await textarea.pressSequentially("auth");
    await page.waitForFunction(() => document.querySelectorAll("[data-task-ref-option]").length === 1);
    check(
      "# picker opens on the caret and filters",
      wholeField.length === 3
        && (await refOptions.first().getAttribute("data-task-ref-option")) === "2"
        && (await textarea.getAttribute("aria-activedescendant")) === "task-ref-picker-option-2",
      `lone # offered ${wholeField.join(",")}; "#auth" narrowed to task 2`,
    );

    // Esc is the picker's own rung: it closes the list and leaves the Task open.
    await page.keyboard.press("Escape");
    check(
      "picker Esc closes the picker only",
      (await refPicker.count()) === 0
        && (await taskField.getAttribute("data-task")) === "1"
        && (await deploy.getAttribute("aria-pressed")) === "true"
        && (await textarea.inputValue()) === "hold until #auth",
      "one Escape dismissed the list without clearing task focus or the draft",
    );

    await textarea.pressSequentially("-");
    await refPicker.waitFor();
    await page.keyboard.press("Enter");
    check(
      "Enter inserts the ref rather than sending",
      (await textarea.inputValue()) === "hold until #2 " && (await refPicker.count()) === 0,
      `composer now "${await textarea.inputValue()}"`,
    );

    await send.click();
    const citedFrame = await frameAfter(startFrame, (frame) => frame.type === "send_message" && frame.body === "hold until #2");
    check(
      "cited send carries the ref and the focus",
      citedFrame.ref === 2
        && JSON.stringify(citedFrame.mentions) === JSON.stringify([2, 1]),
      JSON.stringify(citedFrame),
    );

    // The citation comes back as an inline tag in the Task margin, and pressing
    // it moves focus to the Task it names.
    const refTag = page.locator('[data-slot="task-ref-tag"][data-task-ref="2"]');
    await refTag.first().waitFor();
    // Read it through a poll: the margin re-renders as the mock's reply lands,
    // and a node resolved a frame earlier can be detached by the time it is
    // measured (an detached node reports empty computed style, not stale one).
    const tagShape = await page
      .waitForFunction(() => {
        const element = document.querySelector('[data-slot="task-ref-tag"][data-task-ref="2"]');
        if (!element) return null;
        // The app resets `button { font: inherit }` outside the cascade layers,
        // so the ref's monospace lives on the text span inside the control.
        const family = getComputedStyle(element.firstElementChild ?? element).fontFamily;
        if (!family) return null;
        return {
          text: element.textContent.trim(),
          label: element.getAttribute("aria-label"),
          family,
          decoration: getComputedStyle(element).textDecorationLine,
        };
      })
      .then((handle) => handle.jsonValue());
    check(
      "cited task renders as an inline ref tag",
      tagShape.text === "#2" && /mono/i.test(tagShape.family)
        && tagShape.label === "auth pr, task #2" && tagShape.decoration === "underline",
      JSON.stringify(tagShape),
    );
    await refTag.first().click();
    await page.locator('[data-slot="task-field"][data-task="2"]').waitFor();
    check(
      "activating a ref tag focuses that task",
      (await auth.getAttribute("aria-pressed")) === "true"
        && new URL(page.url()).pathname === "/t/2",
      `focus moved to auth pr; address bar ${new URL(page.url()).pathname}`,
    );

    // The ref is the Task's standing address, not a picker-only artefact: it
    // rides the chip ahead of the name, in mono, on the same optical line, and
    // the focused card carries a copyable one.
    const refShape = await page.evaluate(() => {
      const rows = Array.from(document.querySelectorAll("[data-task-id]"));
      const chips = rows.map((row) => {
        const ref = row.querySelector('[data-slot="task-ref"]');
        const name = row.querySelector('[data-slot="task-ref"] + span');
        if (!ref || !name) return null;
        const refBox = ref.getBoundingClientRect();
        const nameBox = name.getBoundingClientRect();
        const style = getComputedStyle(ref);
        return {
          id: row.dataset.taskId,
          text: ref.textContent.trim(),
          family: style.fontFamily,
          mono: /mono/i.test(style.fontFamily),
          leadsName: Math.round(refBox.right) <= Math.round(nameBox.left),
          centreDelta: Math.abs((refBox.top + refBox.bottom) / 2 - (nameBox.top + nameBox.bottom) / 2),
        };
      });
      const copy = document.querySelector('[data-slot="task-ref-copy"]');
      return {
        chips,
        copy: copy
          ? {
            text: copy.textContent.trim(),
            label: copy.getAttribute("aria-label"),
            mono: /mono/i.test(getComputedStyle(copy.firstElementChild ?? copy).fontFamily),
          }
          : null,
      };
    });
    check(
      "every task carries its ref, quietly and in line",
      refShape.chips.length === 3
        && refShape.chips.every((chip) => chip && chip.mono && chip.leadsName && chip.centreDelta <= 1.5)
        && refShape.chips.map((chip) => chip.text).join(",") === "#1,#2,#3"
        && refShape.copy?.text === "#2"
        && refShape.copy.label === "Copy task ref #2"
        && refShape.copy.mono,
      JSON.stringify(refShape),
    );

    // Back out of the citation and re-open the deploy task the rest of the
    // suite is written against.
    await ensureFocused(1);

    const taskScroll = page.locator('[data-slot="task-scroll"]');
    await taskScroll.evaluate((element) => { element.scrollTop = 14; });
    await deploy.click();
    const ambientComposer = await composerShell.evaluate((element) => {
      const rect = element.getBoundingClientRect();
      return {
        left: Math.round(rect.left),
        width: Math.round(rect.width),
        height: Math.round(rect.height),
        top: Math.round(rect.top),
      };
    });
    check(
      "focus clears to ambient",
      (await taskField.count()) === 0
        && (await taskCard.count()) === 0
        && (await ambientField.count()) === 1
        && await composerShell.getAttribute("data-focused") === "false"
        && await deploy.getAttribute("aria-pressed") === "false"
        && ambientComposer.left === columnGeometry.composer.left
        && ambientComposer.width === columnGeometry.composer.width
        && ambientComposer.height === columnGeometry.composer.height
        && ambientComposer.top === columnGeometry.composer.top,
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
    // A cold load at the ROOT, which is what the load-time focus rule is about.
    // (A reload would carry the `/t/1` address of the task just decided, and a
    // deep link is a focus the Owner already chose — it outranks the heuristic
    // by design. That path is proved on its own two checks below.)
    await page.goto(origin, { waitUntil: "domcontentloaded" });
    await appReady(page);
    // Every seeded task is settled at this point, so nothing ranks as needing
    // the Owner and the same load-time rule must leave the field ambient —
    // landing on already-decided work would be the opposite of most-needing.
    const settledChips = await page.locator('[data-slot="task-index"] [data-task-id]').evaluateAll(
      (nodes) => nodes.map((node) => node.getAttribute("aria-label")),
    );
    check(
      "nothing needing rests ambient",
      settledChips.length === 3 && settledChips.every((label) => /, done, task #\d+$/.test(label ?? ""))
        && (await ambientField.count()) === 1
        && (await taskField.count()) === 0
        && await composerShell.getAttribute("data-focused") === "false",
      `all-settled load stays ambient; chips ${JSON.stringify(settledChips)}`,
    );
    check(
      "the address bar carries the ambient root",
      new URL(page.url()).pathname === "/",
      `ambient is addressed as ${new URL(page.url()).pathname}`,
    );
    // A `/t/<id>` deep link is a focus already chosen: it opens that Task on a
    // cold load even when the most-needing rule would have rested ambient.
    await page.goto(`${origin}/t/3`, { waitUntil: "domcontentloaded" });
    await appReady(page);
    await taskCard.waitFor();
    check(
      "deep link opens its task on a cold load",
      (await taskField.getAttribute("data-task")) === "3"
        && (await ambientField.count()) === 0
        && (await page.getByRole("button", { name: /nightly backup, done/i }).getAttribute("aria-pressed")) === "true",
      "/t/3 landed focused on nightly backup over the all-settled ambient rule",
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
    // The floating ⋯ anchors to the HOME FIELD's top-right corner, so a docked
    // utility takes its width out of the box the ⋯ is measured against and the
    // two can never meet. This used to be the opposite: the strip hung off the
    // viewport's top-right, so on a fine pointer the ⋯ sat cramped beside the
    // pane's × and — at this coarse-pointer emulation, where both grow to 44px —
    // covered it outright, which is why the close below had to be a keypress.
    const overlaps = (a, b) =>
      a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom;
    // The dismissed dropdown leaves `pointer-events: none` on <body> for a
    // moment after its own teardown, which makes every hit test answer <html>.
    // Wait for the page to be hit-testable again so the gate below measures
    // occlusion rather than that teardown.
    const hitTestable = (target) =>
      pollReady("pointer hit-testing settles", async () =>
        target.evaluate(() => (document.elementFromPoint(4, 4)?.tagName === "HTML" ? null : true)));
    await hitTestable(page);
    const dockedGeometry = await page.evaluate(() => {
      const box = (selector) => {
        const element = document.querySelector(selector);
        if (!element) return null;
        const rect = element.getBoundingClientRect();
        return {
          left: Math.round(rect.left),
          right: Math.round(rect.right),
          top: Math.round(rect.top),
          bottom: Math.round(rect.bottom),
          width: Math.round(rect.width),
          height: Math.round(rect.height),
        };
      };
      const hit = (selector, within) => {
        const element = document.querySelector(selector);
        if (!element) return null;
        const rect = element.getBoundingClientRect();
        const target = document.elementFromPoint(
          rect.left + rect.width / 2,
          rect.top + rect.height / 2,
        );
        return target?.closest(within) ? "self" : (target?.tagName ?? "nothing");
      };
      return {
        more: box('[data-slot="phone-overflow-trigger"]'),
        strip: box('[data-slot="home-affordances"]'),
        close: box('[data-slot="processes-panel"] button[aria-label="Close Processes"]'),
        panel: box('[data-slot="processes-panel"]'),
        // A hit test is the honest form of "never covered": whatever the paint
        // order, the centre of each control has to belong to that control.
        moreHit: hit('[data-slot="phone-overflow-trigger"]', '[data-slot="phone-overflow-trigger"]'),
        closeHit: hit(
          '[data-slot="processes-panel"] button[aria-label="Close Processes"]',
          'button[aria-label="Close Processes"]',
        ),
      };
    });
    check(
      "the ⋯ clears a docked pane (coarse pointer)",
      !overlaps(dockedGeometry.more, dockedGeometry.close)
        && dockedGeometry.more.right <= dockedGeometry.panel.left
        && dockedGeometry.strip.right <= dockedGeometry.panel.left
        && dockedGeometry.moreHit === "self" && dockedGeometry.closeHit === "self"
        && dockedGeometry.more.height >= 44 && dockedGeometry.more.width >= 44
        && dockedGeometry.close.height >= 44 && dockedGeometry.close.width >= 44,
      JSON.stringify(dockedGeometry),
    );
    // Closed through the pane header's own ×, with a genuine pointer click —
    // the workaround the occlusion used to force is gone. The Esc route to the
    // same close is asserted separately as the run's last gate.
    await processPanel.getByRole("button", { name: "Close Processes" }).click();
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
    // ONE Settings entry. The model choice is a tab inside Settings now, so the
    // menu no longer carries a second row pointing at the same sheet — read the
    // open menu before summoning anything, because the duplicate would be here.
    const settingsEntries = await page.evaluate(() =>
      [...document.querySelectorAll('[role="menuitem"]')]
        .map((item) => (item.getAttribute("aria-label") ?? item.textContent ?? "").trim())
        .filter((name) => /settings/i.test(name)));
    check(
      "settings entry is singular",
      settingsEntries.length === 1 && settingsEntries[0] === "Settings",
      JSON.stringify(settingsEntries),
    );
    await page.getByRole("menuitem", { name: "Settings", exact: true }).click();
    const settingsPanel = page.locator('[data-slot="settings-panel"]');
    await settingsPanel.waitFor();
    // Settings is an infrequent, deep, form-shaped visit, so on desktop it now
    // takes the whole viewport instead of docking as a 340–440px inspector: the
    // overlay IS the viewport, it is an honest modal at this width, and its
    // rows read in the task world's own column (measure + one gutter each side)
    // rather than a rail that wrapped every one of them.
    await hitTestable(page);
    const settingsGeometry = await page.evaluate(() => {
      const rect = (selector) => {
        const element = document.querySelector(selector);
        if (!element) return null;
        const r = element.getBoundingClientRect();
        return {
          left: Math.round(r.left),
          top: Math.round(r.top),
          right: Math.round(r.right),
          width: Math.round(r.width),
          height: Math.round(r.height),
        };
      };
      const panel = document.querySelector('[data-slot="settings-panel"]');
      const more = document.querySelector('[data-slot="phone-overflow-trigger"]');
      const moreRect = more?.getBoundingClientRect();
      return {
        panel: rect('[data-slot="settings-panel"]'),
        column: rect('[data-slot="settings-column"]'),
        // The header's inner row — the box the icon, title and × line up in.
        headerRow: (() => {
          const row = document.querySelector("#settings-pane-title")?.parentElement;
          if (!row) return null;
          const r = row.getBoundingClientRect();
          return { left: Math.round(r.left), right: Math.round(r.right), width: Math.round(r.width) };
        })(),
        role: panel?.getAttribute("role"),
        modal: panel?.getAttribute("aria-modal"),
        position: panel ? getComputedStyle(panel).position : null,
        viewport: { width: innerWidth, height: innerHeight },
        // Nothing floats over a full-screen utility: the ⋯ is behind it, so its
        // own centre belongs to the overlay.
        moreCovered: moreRect
          ? Boolean(
            document
              .elementFromPoint(moreRect.left + moreRect.width / 2, moreRect.top + moreRect.height / 2)
              ?.closest('[data-slot="settings-panel"]'),
          )
          : null,
      };
    });
    const FRAME = 720; // --container-frame: measure (42rem) + one gutter each side
    check(
      "settings is a full-viewport modal reading in the one column",
      settingsGeometry.position === "fixed"
        && settingsGeometry.panel.left === 0 && settingsGeometry.panel.top === 0
        && settingsGeometry.panel.width === settingsGeometry.viewport.width
        && settingsGeometry.panel.height === settingsGeometry.viewport.height
        && settingsGeometry.role === "dialog" && settingsGeometry.modal === "true"
        && settingsGeometry.column.width === FRAME
        // Centred, and the header's title lands on the content's own left edge
        // rather than in the far corner of a 1440px screen.
        && Math.abs(settingsGeometry.column.left + FRAME / 2 - settingsGeometry.viewport.width / 2) <= 1
        && settingsGeometry.headerRow.left === settingsGeometry.column.left
        && settingsGeometry.headerRow.width === FRAME
        && settingsGeometry.moreCovered === true,
      JSON.stringify(settingsGeometry),
    );
    await settingsPanel.getByRole("radio", { name: "Dark" }).click();
    check("settings theme", await page.locator("html.dark").count() === 1, "Dark toggled in the summoned Settings overlay");

    // Settings is side-tab navigation inside that one modal now: a quiet rail at
    // `rail:` and up. The contract under test is the tab contract itself — seven
    // named tabs in one list, one selection, ONE tab stop with the arrows moving
    // inside it, one mounted panel that the selected tab names, and a real swap
    // of content when another tab is taken.
    const settingsTablist = settingsPanel.locator('[role="tablist"]');
    // The Agents tab's own marker. The seeded WebSocket mock reports no prompt
    // snapshot, so the prompt editor itself is not on screen under THIS runner —
    // the host-backed suite drives that editor against a real host. What proves
    // the swap here is the subsection heading the tab always renders.
    const mainAgentHeading = settingsPanel.getByRole("heading", { name: "Main agent", exact: true });
    const darkRadio = settingsPanel.getByRole("radio", { name: "Dark" });
    const readTabs = () => page.evaluate(() => {
      const panel = document.querySelector('[data-slot="settings-panel"]');
      const tabs = [...panel.querySelectorAll('[role="tab"]')];
      const panels = [...panel.querySelectorAll('[role="tabpanel"]')];
      const labelledBy = panels[0]?.getAttribute("aria-labelledby") ?? null;
      return {
        lists: panel.querySelectorAll('[role="tablist"]').length,
        names: tabs.map((tab) => tab.textContent.trim()),
        selected: tabs
          .filter((tab) => tab.getAttribute("aria-selected") === "true")
          .map((tab) => tab.textContent.trim()),
        stops: tabs
          .filter((tab) => tab.getAttribute("tabindex") === "0")
          .map((tab) => tab.textContent.trim()),
        restRoved: tabs
          .filter((tab) => tab.getAttribute("tabindex") !== "0")
          .every((tab) => tab.getAttribute("tabindex") === "-1"),
        panels: panels.length,
        panelLabelledBy: labelledBy
          ? document.getElementById(labelledBy)?.textContent.trim() ?? null
          : null,
      };
    });
    const selectedTab = () => page.evaluate(() =>
      document.querySelector('[data-slot="settings-panel"] [role="tab"][aria-selected="true"]')
        ?.textContent.trim() ?? null);
    const TAB_NAMES = [
      "Appearance",
      "Agents",
      "Providers",
      "Connection & devices",
      "Notifications",
      "About & debug",
      "Plugins",
    ];
    const tabsAtOpen = await readTabs();
    const agentsBefore = await mainAgentHeading.count();
    await settingsTablist.getByRole("tab", { name: "Agents", exact: true }).click();
    await mainAgentHeading.waitFor();
    const tabsOnAgents = await readTabs();
    const darkAfterAgents = await darkRadio.count();
    // Activation follows focus in this list, so each key press IS a selection.
    await settingsTablist.evaluate((node) => node.focus());
    await page.keyboard.press("ArrowDown");
    const afterArrow = await poll("settings ArrowDown selection", async () =>
      (await selectedTab()) === "Providers" ? "Providers" : null, 5_000);
    await page.keyboard.press("End");
    const afterEnd = await poll("settings End selection", async () =>
      (await selectedTab()) === "Plugins" ? "Plugins" : null, 5_000);
    await page.keyboard.press("Home");
    const afterHome = await poll("settings Home selection", async () =>
      (await selectedTab()) === "Appearance" ? "Appearance" : null, 5_000);
    check(
      "settings tabs",
      tabsAtOpen.lists === 1
        && tabsAtOpen.names.join(" · ") === TAB_NAMES.join(" · ")
        && tabsAtOpen.selected.length === 1 && tabsAtOpen.selected[0] === "Appearance"
        && tabsAtOpen.stops.length === 1 && tabsAtOpen.stops[0] === "Appearance"
        && tabsAtOpen.restRoved
        && tabsAtOpen.panels === 1 && tabsAtOpen.panelLabelledBy === "Appearance"
        // The swap is a real swap: Agents' content arrives, Appearance's goes.
        && agentsBefore === 0 && darkAfterAgents === 0
        && tabsOnAgents.selected.length === 1 && tabsOnAgents.selected[0] === "Agents"
        && tabsOnAgents.stops.length === 1 && tabsOnAgents.stops[0] === "Agents"
        && tabsOnAgents.panels === 1 && tabsOnAgents.panelLabelledBy === "Agents"
        && afterArrow === "Providers" && afterEnd === "Plugins" && afterHome === "Appearance",
      JSON.stringify({ tabsAtOpen, agentsBefore, tabsOnAgents, darkAfterAgents, afterArrow, afterEnd, afterHome }),
    );

    // A genuine pointer click: nothing floats over the full-screen overlay's ×.
    await settingsPanel.getByRole("button", { name: "Close Settings" }).click();
    await settingsPanel.waitFor({ state: "detached" });
    check("settings continuity", await taskField.getAttribute("data-task") === "1" && await textarea.inputValue() === stopDraft, "task focus and composer survive Settings");
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
        // Inline Task citations are excluded on purpose, and only they: WCAG
        // 2.5.8's inline exception covers a target sitting in a sentence, whose
        // size is constrained by the line it is set on. Every control that owns
        // its own row is still held to 44px.
        choices: '[data-slot="task-field"] button:not([data-slot="task-ref-tag"])',
        send: 'button[aria-label="Send"]',
        overflow: '[data-slot="phone-overflow-trigger"]',
        attach: 'button[aria-label="Attach files"]',
      };
      return Object.fromEntries(Object.entries(selectors).map(([name, selector]) => {
        const elements = [...document.querySelectorAll(selector)];
        return [name, elements.map((element) => Math.min(element.getBoundingClientRect().width, element.getBoundingClientRect().height))];
      }));
    });
    const targetsPass = Object.values(touchTargets).flat().every((size) => size >= 44)
      && touchTargets.send.length === 1 && touchTargets.attach.length === 1
      && (closeProcessBox?.height ?? 0) >= 44;
    check("all named 44px targets", targetsPass, JSON.stringify({ ...touchTargets, utilityClose: closeProcessBox?.height ?? 0 }));
    // The send-options caret is gone on every pointer type: it opened a menu
    // whose two rows are Enter/Tab on desktop and tap/long-press here, and it
    // was `disabled` — a silent no-op — whenever the draft was empty.
    check(
      "no send-options caret on touch",
      (await page.locator('button[aria-label="More send options"]').count()) === 0
        && (await page.getByRole("menuitem", { name: "Queue for next turn" }).count()) === 0,
      "queueing is the Send long-press here and Tab on desktop; both are printed in the ⌘/ sheet",
    );
    const phoneContainment = await page.evaluate(() => {
      const strip = document.querySelector('[data-slot="task-index"]');
      const conversation = document.querySelector('[data-slot="task-conversation"]');
      const card = document.querySelector('[data-slot="task-card"]');
      const composer = document.querySelector('[data-slot="composer-shell"]');
      const round = (node, side) => (node ? Math.round(node.getBoundingClientRect()[side]) : null);
      return {
        viewport: innerWidth,
        document: document.documentElement.scrollWidth,
        stripLeft: strip?.getBoundingClientRect().left,
        stripRight: strip?.getBoundingClientRect().right,
        columns: conversation ? getComputedStyle(conversation).gridTemplateColumns : "missing",
        cardWidth: round(card, "width"),
        conversationWidth: round(conversation, "width"),
        composerWidth: round(composer, "width"),
        cardLeft: round(card, "left"),
        conversationLeft: round(conversation, "left"),
        composerLeft: round(composer, "left"),
      };
    });
    check(
      "390px containment",
      phoneContainment.document <= phoneContainment.viewport
        && phoneContainment.stripLeft >= 0 && phoneContainment.stripRight <= 390
        && !phoneContainment.columns.includes(" ")
        // The same one column the desktop gate pins, narrowed to the phone:
        // card, conversation and capsule on the same two edges, below the
        // measure because the viewport is.
        && phoneContainment.cardWidth === phoneContainment.conversationWidth
        && phoneContainment.composerWidth === phoneContainment.cardWidth
        && phoneContainment.composerLeft === phoneContainment.cardLeft
        && phoneContainment.conversationLeft === phoneContainment.cardLeft
        && phoneContainment.cardWidth < MEASURE,
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

    // The same one list, on its other axis: below `rail:` the tabs are a
    // horizontal strip above the panel, and the key that reaches along it is
    // ArrowRight rather than the rail's ArrowDown.
    await more.click();
    await page.getByRole("menuitem", { name: "Settings", exact: true }).click();
    await settingsPanel.waitFor();
    const phoneStrip = await page.evaluate(() => {
      const list = document.querySelector('[data-slot="settings-tabs"]');
      if (!list) return null;
      const tabs = [...list.querySelectorAll('[role="tab"]')];
      const first = tabs[0]?.getBoundingClientRect();
      const second = tabs[1]?.getBoundingClientRect();
      return {
        direction: getComputedStyle(list).flexDirection,
        tabs: tabs.length,
        // Horizontal means the second tab sits beside the first, not under it.
        beside: Boolean(first && second && second.left >= first.right - 1
          && Math.abs(second.top - first.top) <= 1),
        selected: tabs.find((tab) => tab.getAttribute("aria-selected") === "true")?.textContent.trim() ?? null,
      };
    });
    // Take the standing tab by pointer rather than calling focus() on the list:
    // the panel's trap claims focus a frame after it mounts, and a synthetic
    // focus placed inside that window is taken straight back. The click changes
    // no selection — Appearance is already the one — it only puts the keyboard
    // where a phone Owner's tap would have put it.
    await settingsTablist.getByRole("tab", { name: "Appearance", exact: true }).click();
    await page.keyboard.press("ArrowRight");
    const phoneAfterArrow = await poll("phone settings ArrowRight selection", async () =>
      (await selectedTab()) === "Agents" ? "Agents" : null, 5_000);
    check(
      "settings tab strip is horizontal on phone",
      phoneStrip?.direction === "row" && phoneStrip.tabs === 7 && phoneStrip.beside === true
        && phoneStrip.selected === "Appearance" && phoneAfterArrow === "Agents",
      JSON.stringify({ ...phoneStrip, phoneAfterArrow }),
    );
    await settingsPanel.getByRole("button", { name: "Close Settings" }).click();
    await settingsPanel.waitFor({ state: "detached" });

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
        composerLeft: composer ? Math.round(composer.getBoundingClientRect().left) : null,
        composerWidth: composer ? Math.round(composer.getBoundingClientRect().width) : null,
        cardLeft: card ? Math.round(card.getBoundingClientRect().left) : null,
        cardWidth: card ? Math.round(card.getBoundingClientRect().width) : null,
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
        && narrow.inputScroll >= narrow.inputClient && narrow.stripScroll > narrow.stripClient
        // Still one column at 320: the grown capsule holds the card's edges.
        && narrow.composerLeft === narrow.cardLeft
        && narrow.composerWidth === narrow.cardWidth,
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
    // The fine-pointer capsule, in its own context — `hasTouch` is a context
    // option, so a desktop pointer cannot be reached from the touch one above.
    // Enter is the send there, so the capsule carries no Send button and no
    // send-options caret, and it rests at one text row: 44px, not the 60px
    // slab that overhung the prose.
    const fineContext = await browser.newContext({
      viewport: { width: 1440, height: 1000 },
      hasTouch: false,
      serviceWorkers: "block",
      colorScheme: "light",
      reducedMotion: "reduce",
    });
    await fineContext.addInitScript((value) => localStorage.setItem("hirsel.token", value), token);
    const finePage = await fineContext.newPage();
    finePage.on("console", (message) => {
      if (message.type() === "error") browserErrors.push(`fine-pointer: ${message.text()}`);
    });
    finePage.on("pageerror", (error) => browserErrors.push(`fine-pointer: ${error.message}`));
    const fineFrames = [];
    finePage.on("websocket", (socket) => {
      socket.on("framesent", (event) => {
        try { fineFrames.push(JSON.parse(String(event.payload))); } catch { /* non-JSON */ }
      });
    });
    await finePage.goto(origin, { waitUntil: "domcontentloaded" });
    await appReady(finePage);
    const fineComposer = finePage.getByRole("textbox", { name: "Message Hirsel" });
    await fineComposer.fill("Enter is the send on a fine pointer");
    await fineComposer.press("Enter");
    const fineSent = await poll("fine-pointer send frame", async () =>
      fineFrames.find((frame) => frame.type === "send_message" && frame.body === "Enter is the send on a fine pointer"));
    const fineGeometry = await finePage.evaluate(() => {
      const box = (selector) => {
        const element = document.querySelector(selector);
        if (!element) return null;
        const rect = element.getBoundingClientRect();
        return { left: Math.round(rect.left), width: Math.round(rect.width), height: Math.round(rect.height) };
      };
      return {
        composer: box('[data-slot="composer-shell"]'),
        prose: box('[data-slot="conversation"] > div'),
        sendButtons: document.querySelectorAll('button[aria-label="Send"]').length,
        carets: document.querySelectorAll('button[aria-label="More send options"]').length,
        attach: document.querySelectorAll('button[aria-label="Attach files"]').length,
      };
    });
    check(
      "fine pointer: no send cluster, one-row capsule, same edges as the prose",
      fineSent.ref !== undefined
        && fineGeometry.sendButtons === 0 && fineGeometry.carets === 0
        && fineGeometry.attach === 1
        && fineGeometry.composer.height === 44
        && fineGeometry.composer.width === MEASURE
        && fineGeometry.prose.left === fineGeometry.composer.left
        && fineGeometry.prose.width === fineGeometry.composer.width,
      JSON.stringify(fineGeometry),
    );

    // The same no-overlap contract at the OTHER pointer type. A fine pointer is
    // where the collision used to read as "cramped side by side" rather than
    // "covered", and it is the emulation the coarse gate above cannot reach
    // (`hasTouch` is a context option). Both targets, both pointers, one rule.
    const fineMore = finePage.locator('[data-slot="phone-overflow-trigger"]');
    await fineMore.click();
    await finePage.getByRole("menuitem", { name: /^Processes/ }).click();
    const finePanel = finePage.locator('[data-slot="processes-panel"]');
    await finePanel.waitFor();
    await hitTestable(finePage);
    const fineDocked = await finePage.evaluate(() => {
      const box = (selector) => {
        const element = document.querySelector(selector);
        if (!element) return null;
        const r = element.getBoundingClientRect();
        return {
          left: Math.round(r.left),
          right: Math.round(r.right),
          top: Math.round(r.top),
          bottom: Math.round(r.bottom),
        };
      };
      const hit = (selector, within) => {
        const element = document.querySelector(selector);
        if (!element) return null;
        const r = element.getBoundingClientRect();
        const target = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2);
        return target?.closest(within) ? "self" : (target?.tagName ?? "nothing");
      };
      return {
        more: box('[data-slot="phone-overflow-trigger"]'),
        strip: box('[data-slot="home-affordances"]'),
        close: box('[data-slot="processes-panel"] button[aria-label="Close Processes"]'),
        panel: box('[data-slot="processes-panel"]'),
        moreHit: hit('[data-slot="phone-overflow-trigger"]', '[data-slot="phone-overflow-trigger"]'),
        closeHit: hit(
          '[data-slot="processes-panel"] button[aria-label="Close Processes"]',
          'button[aria-label="Close Processes"]',
        ),
      };
    });
    check(
      "the ⋯ clears a docked pane (fine pointer)",
      !overlaps(fineDocked.more, fineDocked.close)
        && fineDocked.more.right <= fineDocked.panel.left
        && fineDocked.strip.right <= fineDocked.panel.left
        && fineDocked.moreHit === "self" && fineDocked.closeHit === "self",
      JSON.stringify(fineDocked),
    );
    await finePanel.getByRole("button", { name: "Close Processes" }).click();
    await finePanel.waitFor({ state: "detached" });

    // "Jump to latest" belongs to the reading column, not to the pane: centred
    // on the pane it drifted right of the column and landed on the prose, and
    // at `bottom-1` it touched the capsule's rim. Long sends here (rather than
    // in the shared page) are what make the field scrollable enough to offer
    // it, without disturbing the attribution gates above.
    for (let index = 0; index < 4; index += 1) {
      await fineComposer.fill(`paragraph ${index} ${"scrollable ".repeat(60)}`);
      await fineComposer.press("Enter");
      await poll(`fine-pointer long send ${index}`, async () =>
        fineFrames.find((frame) => frame.type === "send_message" && frame.body.startsWith(`paragraph ${index}`)));
    }
    const fineScroll = finePage.locator('[data-slot="task-scroll"]');
    const jump = finePage.locator('[data-slot="jump-to-latest"]');
    // A programmatic pin suppresses the affordance for its settle window, so
    // keep nudging the scroller to the top until the pill is actually offered
    // rather than racing that window once.
    await poll("jump-to-latest offered", async () => {
      // Re-scroll from a non-zero offset every attempt: parking at 0 twice in a
      // row emits no scroll event at all, so a single nudge that lands inside
      // the pin's settle window would never be re-measured.
      await fineScroll.evaluate((element) => { element.scrollTop = 40; });
      await fineScroll.evaluate((element) => { element.scrollTop = 0; });
      return (await jump.count()) === 1 ? true : null;
    });
    const jumpGeometry = await finePage.evaluate(() => {
      const rect = (selector) => {
        const element = document.querySelector(selector);
        if (!element) return null;
        const r = element.getBoundingClientRect();
        return { left: Math.round(r.left), right: Math.round(r.right), top: Math.round(r.top), bottom: Math.round(r.bottom) };
      };
      return { pill: rect('[data-slot="jump-to-latest"]'), composer: rect('[data-slot="composer-shell"]') };
    });
    const jumpCentre = (jumpGeometry.pill.left + jumpGeometry.pill.right) / 2;
    const columnCentre = (jumpGeometry.composer.left + jumpGeometry.composer.right) / 2;
    check(
      "jump-to-latest centres on the column, clear of the capsule",
      Math.abs(jumpCentre - columnCentre) <= 1
        && jumpGeometry.pill.left >= jumpGeometry.composer.left
        && jumpGeometry.pill.right <= jumpGeometry.composer.right
        // Honest air above the capsule rather than resting on its rim.
        && jumpGeometry.composer.top - jumpGeometry.pill.bottom >= 8,
      JSON.stringify({ ...jumpGeometry, jumpCentre, columnCentre }),
    );
    await jump.click();
    await jump.waitFor({ state: "detached" });
    await fineContext.close();

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
