import assert from "node:assert/strict";
import { execFileSync, spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { mkdtemp, rm } from "node:fs/promises";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { chromium } from "../../app/node_modules/playwright/index.mjs";
import { WebSocket } from "../../app/node_modules/ws/wrapper.mjs";

export const repoRoot = fileURLToPath(new URL("../..", import.meta.url));

function isInside(path, parent) {
  return resolve(path).startsWith(`${resolve(parent)}/`);
}

export function hostProfile(environment = process.env) {
  const profile = environment.HIRSEL_E2E_PROFILE ?? "debug";
  if (profile !== "debug" && profile !== "release") {
    throw new Error("HIRSEL_E2E_PROFILE must be 'debug' or 'release'");
  }
  return profile;
}

export function cargoTargetDirectory(root = repoRoot, environment = process.env) {
  if (environment.CARGO_TARGET_DIR) return resolve(root, environment.CARGO_TARGET_DIR);
  return JSON.parse(execFileSync(
    "cargo",
    ["metadata", "--no-deps", "--format-version", "1"],
    { cwd: root, encoding: "utf8", env: environment },
  )).target_directory;
}

export function hostBinary(root = repoRoot, environment = process.env) {
  return join(cargoTargetDirectory(root, environment), hostProfile(environment), "hirsel-host");
}

export function resolveChromiumExecutable(environment = process.env) {
  return environment.CHROMIUM_EXECUTABLE || undefined;
}

export function chromiumLaunchOptions(options = {}, environment = process.env) {
  const executablePath = resolveChromiumExecutable(environment);
  return { headless: true, ...options, ...(executablePath ? { executablePath } : {}) };
}

export function launchBrowser(options = {}, environment = process.env) {
  return chromium.launch(chromiumLaunchOptions(options, environment));
}

export function isolatedUrl(value, variableName = "HIRSEL_E2E_URL") {
  assert(value, `Set ${variableName} to an isolated loopback service URL.`);
  const url = new URL(value);
  assert(["127.0.0.1", "localhost", "[::1]"].includes(url.hostname), `${variableName} must use a loopback host.`);
  assert.notEqual(url.port, "3076", `${variableName} must never use the live Host port 3076.`);
  return url.href.replace(/\/$/, "");
}

export async function unusedPort() {
  const server = createServer();
  await new Promise((resolveListen, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolveListen);
  });
  const address = server.address();
  assert(address && typeof address !== "string");
  assert.notEqual(address.port, 3076);
  await new Promise((resolveClose, reject) => server.close(error => error ? reject(error) : resolveClose()));
  return address.port;
}

export async function poll(label, predicate, timeoutMs = 30_000, intervalMs = 100) {
  const deadline = Date.now() + timeoutMs;
  let lastError;
  while (Date.now() < deadline) {
    try {
      const value = await predicate();
      if (value) return value;
    } catch (error) {
      lastError = error;
    }
    await new Promise(resolveWait => setTimeout(resolveWait, intervalMs));
  }
  throw new Error(`${label} timed out${lastError ? `: ${lastError.message}` : ""}`);
}

export function startProcess(command, args, { logs, ...options } = {}) {
  const child = spawn(command, args, {
    stdio: options.stdio ?? ["ignore", "pipe", "pipe"],
    ...options,
    detached: false,
  });
  if (logs) {
    for (const stream of [child.stdout, child.stderr]) {
      stream?.on("data", chunk => logs.push(chunk.toString().trim()));
    }
  }
  return child;
}

function waitForExit(child, timeoutMs) {
  if (!child || child.exitCode !== null || child.signalCode !== null) return Promise.resolve(true);
  return new Promise(resolveWait => {
    let settled = false;
    const finish = exited => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      resolveWait(exited);
    };
    const timer = setTimeout(() => finish(false), timeoutMs);
    child.once("exit", () => finish(true));
  });
}

export async function stopProcess(child, timeoutMs = 5_000) {
  if (!child || child.exitCode !== null || child.signalCode !== null) return;
  try {
    child.kill("SIGTERM");
  } catch (error) {
    if (error?.code === "ESRCH") return;
    throw error;
  }
  if (await waitForExit(child, timeoutMs)) return;
  try {
    child.kill("SIGKILL");
  } catch (error) {
    if (error?.code === "ESRCH") return;
    throw error;
  }
  await waitForExit(child, timeoutMs);
}

export async function startHost({
  root = repoRoot,
  dataDir,
  dataDirPrefix = "hirsel-e2e-data-",
  token = `e2e-${crypto.randomUUID()}`,
  port,
  agent = "scripted",
  driver = "fake",
  provider = "anthropic",
  env = {},
  logs,
  stdio,
} = {}) {
  const ownedDataDir = dataDir ? null : await mkdtemp(join(tmpdir(), dataDirPrefix));
  const effectiveDataDir = resolve(dataDir ?? ownedDataDir);
  const runtimeDir = await mkdtemp(join(tmpdir(), "hirsel-e2e-runtime-"));
  assert(!isInside(runtimeDir, root), "Host cwd must be outside the checkout.");
  const effectivePort = port ?? await unusedPort();
  assert.notEqual(effectivePort, 3076);
  const binary = hostBinary(root);
  if (!existsSync(binary)) {
    await Promise.all([runtimeDir, ownedDataDir].filter(Boolean).map(path => rm(path, { recursive: true, force: true })));
    throw new Error(`Host binary is missing at ${binary}; build it with cargo build --workspace --all-targets${hostProfile() === "release" ? " --release" : ""}.`);
  }
  const environment = Object.fromEntries(Object.entries({
    ...process.env,
    HIRSEL_TOKEN: token,
    HIRSEL_AGENT: agent,
    HIRSEL_DRIVER: driver,
    HIRSEL_PROVIDER: provider,
    HIRSEL_DEBUG: "1",
    HIRSEL_IROH: "0",
    HIRSEL_DATA_DIR: effectiveDataDir,
    HIRSEL_CONFIG: join(effectiveDataDir, "hirsel.toml"),
    HIRSEL_TEMPLATES_DIR: join(root, "templates"),
    HIRSEL_APP_DIR: join(root, "app", "dist"),
    HIRSEL_LISTEN: `127.0.0.1:${effectivePort}`,
    ...env,
  }).filter(([, value]) => value !== undefined));
  const child = startProcess(binary, [], { cwd: runtimeDir, env: environment, logs, stdio });
  const url = isolatedUrl(`http://127.0.0.1:${effectivePort}`);
  return { child, binary, dataDir: effectiveDataDir, ownedDataDir, runtimeDir, port: effectivePort, token, url };
}

export async function stopHost(host, timeoutMs = 5_000) {
  if (!host) return;
  await stopProcess(host.child, timeoutMs);
  await Promise.all([host.runtimeDir, host.ownedDataDir].filter(Boolean).map(path => rm(path, { recursive: true, force: true })));
}

export function request({ url, token, frame, expected, timeoutMs = 10_000, onHello, includeHistoryId = false }) {
  return new Promise((resolveRequest, reject) => {
    const socket = new WebSocket(`${url.replace(/^http/, "ws")}/ws`);
    let settled = false;
    const finish = (callback, value) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      socket.close();
      callback(value);
    };
    const timer = setTimeout(() => finish(reject, new Error(`Timed out waiting for ${expected}`)), timeoutMs);
    socket.on("error", error => finish(reject, error));
    socket.on("open", () => socket.send(JSON.stringify({ type: "hello", auth: { static_token: token } })));
    socket.on("message", raw => {
      const message = JSON.parse(raw.toString());
      if (message.type === "hello_ok") {
        onHello?.(message);
        if (frame) socket.send(JSON.stringify(includeHistoryId ? { ...frame, history_id: message.history_id } : frame));
        else if (expected === "hello_ok") finish(resolveRequest, message);
      } else if (message.type === expected) {
        finish(resolveRequest, message);
      } else if (message.type === "error" && expected !== "error") {
        finish(reject, new Error(message.detail));
      }
    });
  });
}

export function hello(url, token, timeoutMs = 10_000) {
  return request({ url, token, expected: "hello_ok", timeoutMs });
}
