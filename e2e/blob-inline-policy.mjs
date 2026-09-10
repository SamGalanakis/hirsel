import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { createServer } from "node:net";
import { mkdtemp, rm } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";
import { chromium } from "../app/node_modules/playwright/index.mjs";
import { WebSocket } from "../app/node_modules/ws/wrapper.mjs";

const root = fileURLToPath(new URL("..", import.meta.url));
const children = [];
const logs = [];
const token = "isolated-blob-policy-token";
const marker = "HIRSEL_INERT_SVG_MARKER";

function startProcess(children, command, args, options = {}, logs = []) {
  const child = spawn(command, args, {
    ...options,
    detached: process.platform !== "win32",
    stdio: options.stdio ?? ["ignore", "pipe", "pipe"],
  });
  children.push(child);
  for (const stream of [child.stdout, child.stderr]) stream?.on("data", chunk => logs.push(chunk.toString().trim()));
  return child;
}

async function startBlobHost({ root, children, logs, port, token, env = {} }) {
  const dataDir = await mkdtemp(join(tmpdir(), "hirsel-blob-policy-"));
  const build = spawnSync("cargo", ["build", "--workspace", "--all-targets"], { cwd: root, encoding: "utf8" });
  if (build.status !== 0) throw new Error(`Host build failed: ${build.stderr || build.stdout}`);
  const metadata = spawnSync("cargo", ["metadata", "--no-deps", "--format-version", "1"], { cwd: root, encoding: "utf8" });
  if (metadata.status !== 0) throw new Error(`Cargo metadata failed: ${metadata.stderr || metadata.stdout}`);
  const binary = join(JSON.parse(metadata.stdout).target_directory, "debug", "hirsel-host");
  const environment = Object.fromEntries(Object.entries({
    ...process.env,
    HIRSEL_TOKEN: token,
    HIRSEL_AGENT: "scripted",
    HIRSEL_DRIVER: "fake",
    HIRSEL_PROVIDER: "anthropic",
    HIRSEL_DEBUG: "1",
    HIRSEL_IROH: "0",
    HIRSEL_DATA_DIR: dataDir,
    HIRSEL_TEMPLATES_DIR: `${root}/templates`,
    HIRSEL_LISTEN: `127.0.0.1:${port}`,
    ...env,
  }).filter(([, value]) => value !== undefined));
  startProcess(children, binary, [], { cwd: dataDir, env: environment }, logs);
  return { dataDir, binary };
}

async function pollReady(label, probe, timeoutMs = 30_000, intervalMs = 75) {
  const deadline = Date.now() + timeoutMs;
  let lastError;
  while (Date.now() < deadline) {
    try {
      const result = await probe();
      if (result) return result;
    } catch (error) {
      lastError = error;
    }
    await new Promise(resolve => setTimeout(resolve, intervalMs));
  }
  throw new Error(`${label} did not settle${lastError ? `: ${lastError.message}` : ""}`);
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
  return new Promise(resolve => {
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

async function teardownBlobHost(children, { timeoutMs = 1_000, dataDirs = [] } = {}) {
  for (const child of children) signalProcess(child, "SIGTERM");
  await Promise.all(children.map(child => waitForExit(child, timeoutMs)));
  for (const child of children) signalProcess(child, "SIGKILL");
  await Promise.all(children.map(child => waitForExit(child, timeoutMs)));
  await Promise.all(dataDirs.filter(Boolean).map(dataDir => rm(dataDir, { recursive: true, force: true })));
}

async function unusedPort() {
  const server = createServer();
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  const address = server.address();
  assert(address && typeof address !== "string");
  await new Promise((resolve, reject) => server.close(error => error ? reject(error) : resolve()));
  return address.port;
}

function request(url, frame, expected) {
  return new Promise((resolve, reject) => {
    const socket = new WebSocket(`${url.replace(/^http/, "ws")}/ws`);
    const timer = setTimeout(() => {
      socket.close();
      reject(new Error(`Timed out waiting for ${expected}`));
    }, 5_000);
    socket.on("error", reject);
    socket.on("open", () => socket.send(JSON.stringify({ type: "hello", auth: { static_token: token } })));
    socket.on("message", raw => {
      const message = JSON.parse(raw.toString());
      if (message.type === "hello_ok") socket.send(JSON.stringify(frame));
      if (message.type === expected) {
        clearTimeout(timer);
        socket.close();
        resolve(message);
      }
      if (message.type === "error") {
        clearTimeout(timer);
        socket.close();
        reject(new Error(message.detail));
      }
    });
  });
}

const port = await unusedPort();
const host = `http://127.0.0.1:${port}`;
let dataDir;
let browser;
try {
  ({ dataDir } = await startBlobHost({
    root,
    children,
    logs,
    port,
    token,
    agent: "scripted",
    driver: "fake",
    provider: "anthropic",
    dataDirPrefix: "hirsel-blob-policy-",
    env: { ANTHROPIC_API_KEY: "", OPENAI_API_KEY: "", OPENROUTER_API_KEY: "" },
  }));
  await pollReady("isolated Hirsel Host", async () => (await fetch(`${host}/readyz`)).ok);
  const svg = `<svg xmlns="http://www.w3.org/2000/svg"><script>document.title='${marker}'</script><text>inert marker</text></svg>`;
  const uploaded = await request(host, {
    type: "upload_blob",
    client_id: "svg-upload",
    name: "proof.svg",
    mime: "image/svg+xml",
    data_b64: Buffer.from(svg).toString("base64"),
  }, "blob_ok");
  const signed = await request(host, {
    type: "get_blob_url",
    client_id: "svg-url",
    blob_id: uploaded.blob.id,
  }, "blob_url");
  const response = await fetch(`${host}${signed.url}`);
  assert.equal(response.status, 200);

  browser = await chromium.launch({
    headless: true,
    executablePath: process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH
      ?? "/home/sam/.cache/ms-playwright/chromium_headless_shell-1234/chrome-headless-shell-linux64/chrome-headless-shell",
  });
  const page = await browser.newPage();
  const unsafe = process.env.HIRSEL_EXPECT_UNSAFE_INLINE === "1";
  if (unsafe) {
    assert.match(response.headers.get("content-disposition") ?? "", /^inline;/);
    await page.goto(`${host}${signed.url}`);
    await page.waitForFunction(expected => document.title === expected, marker);
  } else {
    assert.match(response.headers.get("content-disposition") ?? "", /^attachment;/);
    assert.equal(response.headers.get("x-content-type-options"), "nosniff");
    assert.equal(response.headers.get("content-security-policy"), "sandbox; default-src 'none'");
    const downloadPromise = page.waitForEvent("download");
    await page.goto(`${host}${signed.url}`).catch(error => {
      if (!String(error).includes("Download is starting")) throw error;
    });
    const download = await downloadPromise;
    assert.equal(download.suggestedFilename(), "proof.svg");
    assert.notEqual(await page.title(), marker);
  }
  console.log(JSON.stringify({ unsafeSvgExecuted: unsafe, disposition: response.headers.get("content-disposition") }));
} finally {
  await browser?.close();
  await teardownBlobHost(children, { dataDirs: [dataDir] });
}
