// Screenshot harness for the impeccable design loop.
// Usage: node tools/shot.mjs <outdir> <tag>
// The seeded mock (8799) is stateful; the wrapper restarts it before each run,
// and mutating scenes (that SEND) run last so read-only scenes stay clean.
import { chromium, devices } from "playwright";
import { mkdirSync } from "node:fs";
import { setTimeout as sleep } from "node:timers/promises";

const OUT = process.argv[2];
const TAG = process.argv[3] || "base";
const MAIN = "http://localhost:5199/";
const EMPTY = "http://localhost:5198/";
const DEAD = "http://localhost:5197/";
const TOKEN = "dev-token";
mkdirSync(OUT, { recursive: true });

const PHONE = { width: 390, height: 844 };
const DESK = { width: 1280, height: 900 };
const SPLIT = { width: 1000, height: 900 };

let browser;
async function ctxFor(viewport, { withToken = true, touch = false } = {}) {
  const base = touch
    ? { ...devices["iPhone 13"], viewport, isMobile: true, hasTouch: true }
    : { viewport, deviceScaleFactor: 2 };
  const ctx = await browser.newContext(base);
  if (withToken) {
    await ctx.addInitScript((tok) => {
      try { localStorage.setItem("hirsel.token", tok); } catch {}
    }, TOKEN);
  }
  return ctx;
}

async function shot(page, name) {
  await page.screenshot({ path: `${OUT}/${TAG}__${name}.png` });
  console.log("shot", name);
}

async function scene(name, viewport, url, fn, opts = {}) {
  const touch = name.startsWith("phone-");
  const ctx = await ctxFor(viewport, { ...opts, touch });
  const page = await ctx.newPage();
  try {
    await page.goto(url, { waitUntil: "domcontentloaded" });
    await sleep(opts.settle ?? 1400);
    await fn(page);
    await shot(page, name);
  } catch (e) {
    console.error("FAIL", name, e.message.split("\n")[0]);
  } finally {
    await ctx.close();
  }
}

const noop = async () => {};

async function typeSend(p, text) {
  const ta = p.locator('[data-composer="main"]');
  await ta.click();
  await ta.type(text, { delay: 8 });
}

async function main() {
  browser = await chromium.launch();

  // ---------- READ-ONLY scenes (clean seeded state) ----------
  await scene("phone-tokengate", PHONE, MAIN, noop, { withToken: false });
  await scene("desk-tokengate", DESK, MAIN, noop, { withToken: false });

  await scene("desk-connecting", DESK, DEAD, noop, { settle: 500 });
  await scene("phone-connecting", PHONE, DEAD, noop, { settle: 500 });
  await scene("desk-reconnecting", DESK, DEAD, async () => { await sleep(6000); }, { settle: 500 });

  await scene("phone-empty-chat", PHONE, EMPTY, noop);
  await scene("desk-empty-chat", DESK, EMPTY, noop);
  await scene("desk-empty-pings", DESK, EMPTY, noop);

  await scene("phone-chat", PHONE, MAIN, noop);
  await scene("desk-chat", DESK, MAIN, noop);
  await scene("split-chat", SPLIT, MAIN, noop);

  await scene("phone-tray-expanded", PHONE, MAIN, async (p) => {
    await p.click('[data-slot="tray-bar"]');
    await sleep(500);
  });

  await scene("desk-pings-done", DESK, MAIN, async (p) => {
    await p.getByText(/^Done \(/).first().click();
    await sleep(400);
  });

  await scene("desk-mention", DESK, MAIN, async (p) => {
    await typeSend(p, "ping @");
    await sleep(500);
  });
  await scene("phone-mention", PHONE, MAIN, async (p) => {
    await typeSend(p, "ping @");
    await sleep(500);
  });

  await scene("desk-composer-typing", DESK, MAIN, async (p) => {
    await typeSend(p, "Ship it — but hold the migration until the backup verifies.");
    await sleep(300);
  });

  // Side chat (read-only: opens a side session but doesn't touch main transcript)
  await scene("desk-sidechat", DESK, MAIN, async (p) => {
    await p.getByText("Discuss").first().click();
    await sleep(1700);
  });
  await scene("split-sidechat", SPLIT, MAIN, async (p) => {
    await p.click('[data-slot="tray-bar"]');
    await sleep(400);
    await p.locator('[data-slot="tray-panel"]').getByText("Discuss").first().click();
    await sleep(1700);
  });
  await scene("phone-sidechat", PHONE, MAIN, async (p) => {
    await p.click('[data-slot="tray-bar"]');
    await sleep(400);
    await p.locator('[data-slot="tray-panel"]').getByText("Discuss").first().click();
    await sleep(1700);
  });

  await scene("desk-processes", DESK, MAIN, async (p) => {
    await p.click('[aria-label="Processes"]');
    await sleep(600);
  });
  await scene("phone-processes", PHONE, MAIN, async (p) => {
    await p.click('[aria-label="Processes"]');
    await sleep(600);
  });

  // ---------- MUTATING scenes (SEND — run last) ----------
  await scene("desk-timeline", DESK, MAIN, async (p) => {
    await typeSend(p, "timeline");
    await p.keyboard.press("Enter");
    await sleep(1500);
  });
  await scene("desk-queued", DESK, MAIN, async (p) => {
    await typeSend(p, "hold");
    await p.keyboard.press("Enter");
    await sleep(700);
    await typeSend(p, "and then run the tests");
    await p.keyboard.press("Tab");
    await sleep(600);
  });
  await scene("phone-timeline", PHONE, MAIN, async (p) => {
    await typeSend(p, "monitor");
    await p.click('[aria-label="Send"]');
    await sleep(1500);
  });
  await scene("desk-timeline-done", DESK, MAIN, async (p) => {
    await typeSend(p, "timeline");
    await p.keyboard.press("Enter");
    await sleep(4500);
    const chip = p.getByText(/turn details|tools/i).first();
    if (await chip.count()) { await chip.click().catch(() => {}); await sleep(300); }
  });

  await browser.close();
}
main().catch((e) => { console.error(e); process.exit(1); });
