#!/usr/bin/env node
import { fileURLToPath } from "node:url";
import { PORTS, hasCodexAuth, pollReady, startHost, teardown, writeReport } from "./lib/harness.mjs";

const root = fileURLToPath(new URL("..", import.meta.url));
const reportDir = `${root}/e2e/reports`;
const port = PORTS.externalSmoke.host;
const origin = `http://127.0.0.1:${port}`;
const token = "external-smoke-local-owner";
const startedAt = new Date().toISOString();
const requestedRun = process.argv.includes("--run");
const authorized = process.env.HIRSEL_EXTERNAL_SMOKE === "1";
const codexAuthPresent = hasCodexAuth(process.env.HOME);
const anthropicPresent = Boolean(process.env.ANTHROPIC_API_KEY);
const provider = process.env.HIRSEL_SMOKE_PROVIDER
  || (anthropicPresent ? "anthropic" : codexAuthPresent ? "codex" : null);
const configured = provider === "anthropic" ? anthropicPresent : provider === "codex" ? codexAuthPresent : false;
const children = [];
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

async function poll(label, probe, timeoutMs) {
  return pollReady(label, probe, timeoutMs, 100);
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
  ({ dataDir } = await startHost({
    root,
    children,
    port,
    token,
    agent: "lash",
    provider,
    dataDirPrefix: "hirsel-external-smoke-",
  }));
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
  await teardown(children, { dataDirs: [dataDir] });
  result.completed_at = new Date().toISOString();
  await writeReport({
    root,
    reportDir,
    basename: "task-host-external-smoke-latest",
    title: "Optional external-model Host smoke",
    report: result,
    details: [
      `- Provider: \`${result.provider}\``,
      `- Credential source: ${result.credential_source}; values were never read into evidence`,
      `- External calls: ${result.external_calls}`,
      `- Reason: ${result.reason ?? "completed"}`,
    ],
  });
}

console.log(`${result.status}: ${result.reason}`);
