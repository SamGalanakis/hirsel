import { mkdtemp } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
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
  };
  await run("Thread smoke", process.execPath, ["e2e/thread-smoke.mjs"], hostEnvironment);
  await run("Artifact Thread smoke", process.execPath, ["e2e/artifact-thread-smoke.mjs"], hostEnvironment);
  await run("Thread showcase smoke", process.execPath, ["e2e/thread-showcase-smoke.mjs"], hostEnvironment);

  const vitePort = await unusedPort();
  const viteUrl = `http://127.0.0.1:${vitePort}`;
  vite = startProcess(process.execPath, [
    join(repoRoot, "app", "node_modules", "vite", "bin", "vite.js"),
    "--config",
    join(repoRoot, "app", "artifact-preview.config.ts"),
    "--host",
    "127.0.0.1",
    "--port",
    String(vitePort),
    "--strictPort",
  ], { cwd: join(repoRoot, "app"), env: process.env });
  await poll("artifact preview readiness", async () => {
    if (vite.exitCode !== null || vite.signalCode !== null) {
      throw new Error(`Artifact preview exited (${vite.exitCode ?? vite.signalCode})`);
    }
    return (await fetch(`${viteUrl}/tools/artifact-smoke.html`)).ok;
  }, 30_000);
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
