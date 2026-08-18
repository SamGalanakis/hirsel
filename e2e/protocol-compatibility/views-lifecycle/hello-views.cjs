#!/usr/bin/env node
// Print one authenticated hello_ok frame so reconnect View replay is observed on the wire.
const [url, token] = process.argv.slice(2);
if (!url || !token) {
  console.error("usage: hello-views.cjs <ws-url> <token>");
  process.exit(2);
}
if (typeof WebSocket !== "function") {
  console.error("this script needs Node >= 22 (built-in WebSocket)");
  process.exit(2);
}

let done = false;
const socket = new WebSocket(url);
const deadline = setTimeout(() => {
  console.error("timed out before hello_ok");
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
