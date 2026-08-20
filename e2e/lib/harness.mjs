import { spawn, spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";

export const PORTS = Object.freeze({
  taskMargins: Object.freeze({ mock: 39127, vite: 39128 }),
  taskHost: Object.freeze({ host: 39129, vite: 39130 }),
  externalSmoke: Object.freeze({ host: 39131 }),
  responsiveSweep: Object.freeze({ mock: 39132, vite: 39133 }),
});

export function gitOutput(root, args) {
  const result = spawnSync("git", args, { cwd: root, encoding: "utf8" });
  return result.status === 0 ? result.stdout.trim() : "unavailable";
}

export function recordCheck(checks, name, passed, evidence) {
  checks.push({ name, passed: Boolean(passed), evidence });
  if (!passed) throw new Error(`${name}: ${evidence}`);
}

export function startProcess(children, command, args, options = {}, logs = []) {
  const child = spawn(command, args, {
    ...options,
    detached: process.platform !== "win32",
    stdio: options.stdio ?? ["ignore", "pipe", "pipe"],
  });
  children.push(child);
  for (const stream of [child.stdout, child.stderr]) {
    stream?.on("data", (chunk) => logs.push(chunk.toString().trim()));
  }
  return child;
}

export async function pollReady(label, probe, timeoutMs = 30_000, intervalMs = 75) {
  const deadline = Date.now() + timeoutMs;
  let lastError;
  while (Date.now() < deadline) {
    try {
      const result = await probe();
      if (result) return result;
    } catch (error) {
      lastError = error;
    }
    await new Promise((resolve) => setTimeout(resolve, intervalMs));
  }
  throw new Error(`${label} did not settle${lastError ? `: ${lastError.message}` : ""}`);
}

export async function appReady(page) {
  await page.locator('[data-slot="composer-shell"]').waitFor();
  await page.locator('[data-slot="task-index"] [data-task-id]').first().waitFor();
}

function signalProcess(child, signal) {
  try {
    if (child.exitCode !== null || child.signalCode !== null) return;
    if (process.platform === "win32") child.kill(signal);
    else process.kill(-child.pid, signal);
  } catch {
    // The owned process group already exited.
  }
}

function waitForExit(child, timeoutMs) {
  if (child.exitCode !== null || child.signalCode !== null) return Promise.resolve();
  return new Promise((resolve) => {
    let settled = false;
    const finish = () => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      resolve();
    };
    const timer = setTimeout(finish, timeoutMs);
    child.once("exit", finish);
  });
}

export async function teardown(children, { timeoutMs = 1_000, dataDirs = [] } = {}) {
  for (const child of children) signalProcess(child, "SIGTERM");
  await Promise.all(children.map((child) => waitForExit(child, timeoutMs)));
  for (const child of children) signalProcess(child, "SIGKILL");
  await Promise.all(children.map((child) => waitForExit(child, timeoutMs)));
  await Promise.all(dataDirs.filter(Boolean).map((dataDir) => rm(dataDir, { recursive: true, force: true })));
}

export async function startHost({ root, children, logs, port, token, agent, driver, provider, dataDirPrefix }) {
  const dataDir = await mkdtemp(join(tmpdir(), dataDirPrefix));
  const build = spawnSync("cargo", ["build", "-p", "hirsel-host", "--bin", "hirsel-host"], {
    cwd: root,
    encoding: "utf8",
  });
  if (build.status !== 0) throw new Error(`Host build failed: ${build.stderr || build.stdout}`);
  const metadata = spawnSync("cargo", ["metadata", "--no-deps", "--format-version", "1"], {
    cwd: root,
    encoding: "utf8",
  });
  if (metadata.status !== 0) throw new Error(`Cargo metadata failed: ${metadata.stderr || metadata.stdout}`);
  const binary = join(JSON.parse(metadata.stdout).target_directory, "debug", "hirsel-host");
  const environment = Object.fromEntries(Object.entries({
    ...process.env,
    HIRSEL_TOKEN: token,
    HIRSEL_AGENT: agent,
    HIRSEL_DRIVER: driver,
    HIRSEL_PROVIDER: provider,
    HIRSEL_DEBUG: "1",
    HIRSEL_IROH: "0",
    HIRSEL_DATA_DIR: dataDir,
    HIRSEL_TEMPLATES_DIR: `${root}/templates`,
    HIRSEL_LISTEN: `127.0.0.1:${port}`,
  }).filter(([, value]) => value !== undefined));
  startProcess(children, binary, [], { cwd: dataDir, env: environment }, logs);
  return { dataDir, binary };
}

export async function writeReport({
  root,
  reportDir,
  basename,
  title,
  report,
  error = null,
  details = [],
}) {
  const dirty = gitOutput(root, ["status", "--porcelain=v1"]);
  const completedAt = new Date().toISOString();
  const provenance = report.provenance ?? {
    revision: report.revision ?? gitOutput(root, ["rev-parse", "HEAD"]),
    dirty: Boolean(dirty && dirty !== "unavailable"),
    dirty_entries: dirty && dirty !== "unavailable" ? dirty.split("\n").length : 0,
    started_at: report.started_at,
    completed_at: completedAt,
    command: report.command,
  };
  const result = {
    ...report,
    provenance: { ...provenance, completed_at: completedAt },
    status: error ? "failed" : report.status ?? "passed",
    error: error?.message ?? report.error ?? null,
  };
  if ("started_at" in result) result.completed_at = completedAt;
  await mkdir(reportDir, { recursive: true });
  await writeFile(`${reportDir}/${basename}.json`, `${JSON.stringify(result, null, 2)}\n`);

  const lines = [
    `# ${title}`,
    "",
    `Status: **${result.status.toUpperCase()}**`,
    "",
    `- Revision: \`${result.provenance.revision}\``,
    `- Worktree: **${result.provenance.dirty ? `dirty (${result.provenance.dirty_entries} entries)` : "clean"}**`,
    `- Started: \`${result.provenance.started_at}\``,
    `- Completed: \`${result.provenance.completed_at}\``,
    `- Command: \`${result.provenance.command}\``,
  ];
  if (result.services?.length) {
    lines.push(`- Services: ${result.services.map((service) => `${service.kind} (\`${service.command}\`, ${service.cwd}, port ${service.port ?? "n/a"})`).join("; ")}`);
  }
  if (result.browser) lines.push(`- Browser: ${JSON.stringify(result.browser)}`);
  const screenshots = result.screenshots ?? (result.screenshot ? [result.screenshot] : []);
  if (screenshots.length) lines.push(`- Screenshots: ${screenshots.map((path) => `\`${path}\``).join(", ")}`);
  lines.push(...details, "", "| Gate | Check | Evidence |", "| --- | --- | --- |", ...result.checks.map(({ name, passed, evidence }) =>
    `| ${passed ? "PASS" : "FAIL"} | ${name} | ${String(evidence).replaceAll("|", "\\|")} |`));
  if (result.reason) lines.push("", `Reason: ${result.reason}`);
  if (result.error) lines.push("", `Failure: ${result.error}`);
  lines.push("");
  await writeFile(`${reportDir}/${basename}.md`, lines.join("\n"));
  return result;
}

export function hasCodexAuth(home) {
  return Boolean(home) && existsSync(join(home, ".codex", "auth.json"));
}
