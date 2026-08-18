#!/usr/bin/env node
// Request a blob-scoped signed URL over the real Owner WebSocket protocol.
// Dependency-free: Node >=22 provides the global WebSocket client.
const [url, token, blobId] = process.argv.slice(2);
if (!url || !token || !blobId) {
  console.error("usage: get-blob-url.cjs <ws-url> <token> <blob-id>");
  process.exit(2);
}
if (typeof WebSocket !== "function") {
  console.error("this script needs Node >= 22 (built-in WebSocket)");
  process.exit(2);
}

const clientId = `e2e-blob-url-${Date.now()}`;
let done = false;
const socket = new WebSocket(url);
const deadline = setTimeout(() => {
  console.error("timed out before blob_url");
  process.exit(1);
}, 10_000);

socket.addEventListener("open", () => {
  socket.send(JSON.stringify({ type: "hello", token, last_seen_msg_id: null }));
});

socket.addEventListener("message", (event) => {
  let frame;
  try {
    frame = JSON.parse(String(event.data));
  } catch {
    return;
  }
  if (frame.type === "hello_ok") {
    socket.send(
      JSON.stringify({ type: "get_blob_url", client_id: clientId, blob_id: blobId }),
    );
  } else if (frame.type === "blob_url" && frame.client_id === clientId) {
    done = true;
    clearTimeout(deadline);
    console.log(JSON.stringify(frame));
    socket.close();
  } else if (frame.type === "error") {
    console.error(`host error: ${frame.detail}`);
    process.exit(1);
  }
});

socket.addEventListener("close", () => {
  if (!done) process.exit(1);
});

socket.addEventListener("error", () => {
  if (!done) {
    console.error("socket error");
    process.exit(1);
  }
});
