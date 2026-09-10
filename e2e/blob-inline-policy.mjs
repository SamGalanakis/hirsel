import assert from "node:assert/strict";
import { createServer } from "node:net";
import { fileURLToPath } from "node:url";
import { chromium } from "../app/node_modules/playwright/index.mjs";
import { WebSocket } from "../app/node_modules/ws/wrapper.mjs";
import { pollReady, startHost, teardown } from "./lib/harness.mjs";

const root = fileURLToPath(new URL("..", import.meta.url));
const children = [];
const logs = [];
const token = "isolated-blob-policy-token";
const marker = "HIRSEL_INERT_SVG_MARKER";

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
  ({ dataDir } = await startHost({
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
  await teardown(children, { dataDirs: [dataDir] });
}
