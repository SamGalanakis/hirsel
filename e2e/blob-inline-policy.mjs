import assert from "node:assert/strict";
import { launchBrowser, poll, repoRoot, request, startHost, stopHost } from "./lib/harness.mjs";

const logs = [];
const token = "isolated-blob-policy-token";
const marker = "HIRSEL_INERT_SVG_MARKER";

let hostProcess;
let browser;
try {
  hostProcess = await startHost({
    root: repoRoot,
    logs,
    token,
    dataDirPrefix: "hirsel-blob-policy-",
    env: { ANTHROPIC_API_KEY: "", OPENAI_API_KEY: "", OPENROUTER_API_KEY: "" },
  });
  const host = hostProcess.url;
  await poll("isolated Hirsel Host", async () => (await fetch(`${host}/readyz`)).ok);
  const svg = `<svg xmlns="http://www.w3.org/2000/svg"><script>document.title='${marker}'</script><text>inert marker</text></svg>`;
  const uploaded = await request({ url: host, token, frame: {
    type: "upload_blob",
    client_id: "svg-upload",
    name: "proof.svg",
    mime: "image/svg+xml",
    data_b64: Buffer.from(svg).toString("base64"),
  }, expected: "blob_ok", timeoutMs: 5_000 });
  const signed = await request({ url: host, token, frame: {
    type: "get_blob_url",
    client_id: "svg-url",
    blob_id: uploaded.blob.id,
  }, expected: "blob_url", timeoutMs: 5_000 });
  const response = await fetch(`${host}${signed.url}`);
  assert.equal(response.status, 200);

  browser = await launchBrowser();
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
  await stopHost(hostProcess);
}
