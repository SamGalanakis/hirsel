import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  launchBrowser,
  poll,
  repoRoot,
  startHost,
  startProcess,
  stopHost,
  stopProcess,
  unusedPort,
} from "./lib/harness.mjs";

function run(label, command, args, env = {}) {
  return new Promise((resolveRun, reject) => {
    const child = startProcess(command, args, {
      cwd: repoRoot,
      env: { ...process.env, ...env },
      stdio: "inherit",
    });
    child.once("error", reject);
    child.once("exit", (code, signal) => {
      if (code === 0) resolveRun();
      else reject(new Error(`${label} exited with ${code ?? signal}`));
    });
  });
}

const token = `e2e-${crypto.randomUUID()}`;
const evidenceDir = await mkdtemp(join(tmpdir(), "hirsel-e2e-evidence-"));
let host;
let vite;
const hostLogs = [];
try {
  await run("Harness tests", process.execPath, ["--test", "e2e/lib/harness.test.mjs", "e2e/product-runbook-oracles.test.mjs"]);
  host = await startHost({
    token,
    logs: hostLogs,
    env: { ANTHROPIC_API_KEY: "", OPENAI_API_KEY: "", OPENROUTER_API_KEY: "" },
  });
  await poll("isolated Hirsel Host readiness", async () => {
    if (host.child.exitCode !== null || host.child.signalCode !== null) {
      throw new Error(`Host exited (${host.child.exitCode ?? host.child.signalCode})`);
    }
    return (await fetch(`${host.url}/readyz`)).ok;
  }, 60_000);

  const hostEnvironment = {
    HIRSEL_THREAD_SMOKE_URL: host.url,
    HIRSEL_THREAD_SMOKE_TOKEN: token,
    HIRSEL_THREAD_SMOKE_ADAPTIVE: "1",
    HIRSEL_THREAD_SMOKE_ARTIFACTS: evidenceDir,
    HIRSEL_ARTIFACT_HOST_URL: host.url,
    HIRSEL_ARTIFACT_HOST_TOKEN: token,
    HIRSEL_OPENUI_SHOTS: join(evidenceDir, "openui"),
  };
  await run("Thread smoke", process.execPath, ["e2e/thread-smoke.mjs"], hostEnvironment);
  await run("Artifact Thread smoke", process.execPath, ["e2e/artifact-thread-smoke.mjs"], hostEnvironment);
  await run("Thread showcase smoke", process.execPath, ["e2e/thread-showcase-smoke.mjs"], hostEnvironment);
  await run("OpenUI artifact smoke", process.execPath, ["e2e/openui-artifact-smoke.mjs"], hostEnvironment);

  const vitePort = await unusedPort();
  const viteUrl = `http://127.0.0.1:${vitePort}`;
  // Start the optimizer cold so the dependency scan is guaranteed to report,
  // and never reuse a half-populated cache from an earlier run.
  await rm(join(repoRoot, "app", "node_modules", ".vite-artifact-preview"), { recursive: true, force: true });
  const viteLogs = [];
  vite = startProcess(process.execPath, [
    join(repoRoot, "app", "node_modules", "vite", "bin", "vite.js"),
    "--config",
    join(repoRoot, "app", "artifact-preview.config.ts"),
    "--host",
    "127.0.0.1",
    "--port",
    String(vitePort),
    "--strictPort",
  ], { cwd: join(repoRoot, "app"), env: process.env, logs: viteLogs });
  const previewAlive = () => {
    if (vite.exitCode !== null || vite.signalCode !== null) {
      throw new Error(`Artifact preview exited (${vite.exitCode ?? vite.signalCode})`);
    }
    return true;
  };
  await poll("artifact preview readiness", async () => {
    previewAlive();
    return (await fetch(`${viteUrl}/tools/artifact-smoke.html`)).ok;
  }, 30_000);
  // The dev server discovers dynamically imported dependencies only while a
  // browser walks the module graph, and finishes by reloading every open page
  // ("optimized dependencies changed. reloading"). Landing mid-test, that
  // reload resets the smoke's artifact and its assertions time out on content
  // that was replaced by the reload. Walk the graph here and wait for the
  // optimizer's own completion line, so the reload is spent before the smoke.
  const warmup = await launchBrowser();
  try {
    const page = await warmup.newPage();
    await page.goto(`${viteUrl}/tools/artifact-smoke.html`);
    await page.frameLocator("iframe").getByRole("button", { name: "Count 0", exact: true }).waitFor();
    await poll("artifact preview dependency optimization", () => {
      previewAlive();
      return viteLogs.some(line => /dependencies optimized/.test(line));
    }, 120_000);
  } finally {
    await warmup.close();
  }
  await run("Artifact runtime smoke", process.execPath, ["e2e/artifact-runtime-smoke.mjs"], {
    HIRSEL_ARTIFACT_TEST_URL: viteUrl,
  });

  await run("Blob inline policy", process.execPath, ["e2e/blob-inline-policy.mjs"]);
  console.log(`E2E suite passed with isolated evidence under ${evidenceDir}`);
} catch (error) {
  if (hostLogs.length) console.error(hostLogs.slice(-40).join("\n"));
  throw error;
} finally {
  await stopProcess(vite);
  await stopHost(host);
}
