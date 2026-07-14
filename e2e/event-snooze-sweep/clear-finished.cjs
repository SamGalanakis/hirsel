#!/usr/bin/env node
// Drive the wave-3 sweep over the REAL wire: connect to the host's /ws, complete
// the hello handshake, and send {"type":"clear_finished_events"} — byte-identical
// to the web client's "Clear finished (n)". Exits 0 once the frame is sent on an
// authenticated socket (the runbook asserts the storage effects via /debug/events);
// exits non-zero on auth failure, socket error, or timeout.
//
// Dependency-free: uses Node's built-in WebSocket client (global since Node 22),
// so it runs from any neutral cwd with no module resolution at all.
const [url, token] = process.argv.slice(2);
if (!url || !token) {
  console.error("usage: clear-finished.cjs <ws-url> <token>");
  process.exit(2);
}
if (typeof WebSocket !== "function") {
  console.error("this script needs Node >= 22 (built-in WebSocket)");
  process.exit(2);
}

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
    socket.send(JSON.stringify({ type: "clear_finished_events" }));
    // Give the frame a beat to flush through the socket before closing.
    setTimeout(() => {
      clearTimeout(deadline);
      socket.close();
      console.log("clear_finished_events sent");
      process.exit(0);
    }, 500);
  } else if (frame.type === "error") {
    console.error(`host error: ${frame.detail}`);
    process.exit(1);
  }
});

socket.addEventListener("error", () => {
  console.error("socket error");
  process.exit(1);
});
