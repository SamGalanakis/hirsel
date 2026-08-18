#!/usr/bin/env node
import { spawn, spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("..", import.meta.url));
const reportDir = `${root}/e2e/reports`;
const reportBase = `${reportDir}/task-host-external-smoke-latest`;
const port = 39131;
const origin = `http://127.0.0.1:${port}`;
const token = "external-smoke-local-owner";
const startedAt = new Date().toISOString();
const requestedRun = process.argv.includes("--run");
const authorized = process.env.HIRSEL_EXTERNAL_SMOKE === "1";
const codexAuthPresent = Boolean(process.env.HOME)
  && existsSync(join(process.env.HOME, ".codex", "auth.json"));
const anthropicPresent = Boolean(process.env.ANTHROPIC_API_KEY);
const provider = process.env.HIRSEL_SMOKE_PROVIDER
  || (anthropicPresent ? "anthropic" : codexAuthPresent ? "codex" : null);
const configured = provider === "anthropic" ? anthropicPresent : provider === "codex" ? codexAuthPresent : false;
let child;
let dataDir;
let result = {
  runner: "task-host-external-smoke",
  status: "not_executed",
  started_at: startedAt,
  completed_at: null,
  command: "HIRSEL_EXTERNAL_SMOKE=1 HIRSEL_SMOKE_PROVIDER=<codex|anthropic> node e2e/task-host-external-smoke.mjs --run",
  provider: provider ?? "unselected",
  credential_source: provider === "codex"
    ? "codex OAuth file present"
    : provider === "anthropic"
      ? "ANTHROPIC_API_KEY present"
      : "none detected",
  credentials_redacted: true,
  external_calls: 0,
  checks: [],
  reason: null,
};

async function writeReport() {
  result.completed_at = new Date().toISOString();
  await mkdir(reportDir, { recursive: true });
  await writeFile(`${reportBase}.json`, `${JSON.stringify(result, null, 2)}\n`);
  await writeFile(`${reportBase}.md`, [
    "# Optional external-model Host smoke",
    "",
    `Status: **${result.status.toUpperCase()}**`,
    "",
    `- Started: \`${result.started_at}\``,
    `- Completed: \`${result.completed_at}\``,
    `- Provider: \`${result.provider}\``,
    `- Credential source: ${result.credential_source}; values were never read into evidence`,
    `- External calls: ${result.external_calls}`,
    `- Command: \`${result.command}\``,
    `- Reason: ${result.reason ?? "completed"}`,
    "",
    ...result.checks.map((check) => `- ${check.passed ? "PASS" : "FAIL"}: ${check.name} — ${check.evidence}`),
    "",
  ].join("\n"));
}

async function poll(label, probe, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  let lastError;
  while (Date.now() < deadline) {
    try {
      const value = await probe();
      if (value) return value;
    } catch (error) {
      lastError = error;
    }
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  throw new Error(`${label} timed out${lastError ? `: ${lastError.message}` : ""}`);
}

async function debug(path, options = {}) {
  const response = await fetch(`${origin}${path}`, {
    ...options,
    headers: {
      authorization: `Bearer ${token}`,
      ...(options.body ? { "content-type": "application/json" } : {}),
    },
  });
  if (!response.ok) throw new Error(`${path} returned HTTP ${response.status}`);
  return response.json();
}

async function run() {
  dataDir = await mkdtemp(join(tmpdir(), "hirsel-external-smoke-"));
  const build = spawnSync("cargo", ["build", "-p", "hirsel-host", "--bin", "hirsel-host"], {
    cwd: root,
    encoding: "utf8",
  });
  if (build.status !== 0) throw new Error("Host build failed");
  const metadata = spawnSync("cargo", ["metadata", "--no-deps", "--format-version", "1"], {
    cwd: root,
    encoding: "utf8",
  });
  if (metadata.status !== 0) throw new Error("Cargo metadata failed");
  const binary = join(JSON.parse(metadata.stdout).target_directory, "debug", "hirsel-host");
  child = spawn(binary, [], {
    cwd: dataDir,
    detached: process.platform !== "win32",
    stdio: ["ignore", "ignore", "ignore"],
    env: {
      ...process.env,
      HIRSEL_TOKEN: token,
      HIRSEL_AGENT: "lash",
      HIRSEL_PROVIDER: provider,
      HIRSEL_DEBUG: "1",
      HIRSEL_IROH: "0",
      HIRSEL_DATA_DIR: dataDir,
      HIRSEL_TEMPLATES_DIR: `${root}/templates`,
      HIRSEL_LISTEN: `127.0.0.1:${port}`,
    },
  });
  await poll("Host readiness", async () => (await debug("/debug/health")).ok, 60_000);
  result.checks.push({ name: "production Host ready", passed: true, evidence: `loopback port ${port}` });
  const seeded = await debug("/debug/seed-adaptive-task", { method: "POST", body: "{}" });
  await debug("/debug/event-action", {
    method: "POST",
    body: JSON.stringify({
      event_id: seeded.id,
      action: "advance",
      data: { confirmation: "ready" },
    }),
  });
  result.external_calls = 1;
  const recomposed = await poll("external Agent recomposition", async () => {
    const { events } = await debug("/debug/events");
    const event = events.find((candidate) => candidate.id === seeded.id);
    return event?.ui?.children?.some((node) => node.type === "status") ? event : null;
  }, 120_000);
  result.checks.push({
    name: "external Agent recomposed exact Task",
    passed: recomposed.id === seeded.id && recomposed.anchor === seeded.anchor,
    evidence: `same id=${recomposed.id}; same anchor=${recomposed.anchor}; status=${recomposed.status}`,
  });
  result.status = "passed";
  result.reason = "single bounded external turn completed";
}

try {
  if (!requestedRun) {
    result.reason = "check-only invocation; pass --run and explicit HIRSEL_EXTERNAL_SMOKE=1 to authorize one model turn";
  } else if (!authorized) {
    result.reason = "not authorized: HIRSEL_EXTERNAL_SMOKE=1 is required";
  } else if (!configured) {
    result.reason = "selected provider has no existing credential path";
  } else {
    await run();
  }
} catch (error) {
  result.status = "failed";
  result.reason = error.message;
  process.exitCode = 1;
} finally {
  if (child) {
    try {
      if (process.platform === "win32") child.kill("SIGTERM");
      else process.kill(-child.pid, "SIGTERM");
    } catch { /* owned process already exited */ }
  }
  if (dataDir) await rm(dataDir, { recursive: true, force: true });
  await writeReport();
}

console.log(`${result.status}: ${result.reason}`);
