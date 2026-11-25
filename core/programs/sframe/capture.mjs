// Law G2's Chromium capture (docs/spec/sframe.md SFRAME-14). Run by `oracle.py g2`, not by hand.
//
//     node capture.mjs CHROME INJECT.json OUT.json
//
// Opens two loopback RTCPeerConnections in headless Chromium, sends Chromium's fake camera as H.264
// with an RTCRtpScriptTransform on each side, and records every encoded frame the sender
// produced and every frame the receiver's transform got, joined by RTP timestamp. Each item
// of INJECT.json ({id, key, hex}) replaces the data of the next sender frame of the same
// type (key or delta), so its received bytes show what Chrome's packetizer, depacketizer
// and SPS rewriter do to it.
import { readFileSync, writeFileSync } from "node:fs";

const PLAYWRIGHT = process.env.PLAYWRIGHT_CORE ?? "/tmp/pwv/node_modules/playwright-core/index.mjs";
const { chromium } = await import(PLAYWRIGHT);
const [chrome, injectPath, outPath] = process.argv.slice(2);
const inject = JSON.parse(readFileSync(injectPath, "utf8"));

const worker = `
let queue = [];
let gap = 0;
const hex = (u) => Array.from(u, (b) => b.toString(16).padStart(2, "0")).join("");
const bytes = (h) => new Uint8Array(h.match(/../g).map((x) => parseInt(x, 16)));
onmessage = (e) => { queue = e.data.map((i) => ({ id: i.id, key: i.key, data: bytes(i.hex) })); };
onrtctransform = (ev) => {
  const t = ev.transformer;
  const side = t.options.side;
  t.readable.pipeThrough(new TransformStream({
    transform(frame, c) {
      const ts = frame.getMetadata().rtpTimestamp ?? frame.timestamp;
      const key = frame.type === "key";
      if (side === "send") {
        const orig = hex(new Uint8Array(frame.data));
        let id = null;
        // Every third frame carries an item, so the pacer and bandwidth estimate keep up.
        if (queue.length && queue[0].key === key && (key || ++gap % 3 === 0)) {
          const item = queue.shift();
          id = item.id;
          frame.data = item.data.buffer.slice(0);
        }
        // No generateKeyFrame(): in Chromium 153 it stops the encoder. The receiver below
        // decodes nothing, so its keyframe requests bring a keyframe every few frames.
        postMessage({ side, ts, key, id, hex: orig, left: queue.length });
      } else {
        // Recorded and not decoded: a sealed frame fed to the decoder stalls the session.
        postMessage({ side, ts, key, hex: hex(new Uint8Array(frame.data)) });
        return;
      }
      c.enqueue(frame);
    },
  })).pipeTo(t.writable);
};
`;

const page = `<!doctype html>
<script>
window.log = { sent: [], recv: [] };
window.start = async (items) => {
  const w = new Worker(URL.createObjectURL(new Blob([${JSON.stringify(worker)}], { type: "text/javascript" })));
  w.onmessage = (e) => (e.data.side === "send" ? log.sent : log.recv).push(e.data);
  w.postMessage(items);
  // Chromium's fake camera (--use-fake-device-for-media-stream) runs at a steady 30 fps; a
  // canvas captureStream in a headless page stalls the encoder.
  const media = await navigator.mediaDevices.getUserMedia({ video: { width: 160, height: 120, frameRate: 30 } });
  const track = media.getVideoTracks()[0];
  const a = new RTCPeerConnection();
  const b = new RTCPeerConnection();
  a.onicecandidate = (e) => e.candidate && b.addIceCandidate(e.candidate);
  b.onicecandidate = (e) => e.candidate && a.addIceCandidate(e.candidate);
  b.ontrack = (e) => { e.receiver.transform = new RTCRtpScriptTransform(w, { side: "recv" }); };
  const tr = a.addTransceiver(track, { direction: "sendonly" });
  const h264 = RTCRtpSender.getCapabilities("video").codecs.filter(
    (x) => x.mimeType === "video/H264" && (x.sdpFmtpLine || "").includes("packetization-mode=1"));
  tr.setCodecPreferences(h264);
  tr.sender.transform = new RTCRtpScriptTransform(w, { side: "send" });
  const p = tr.sender.getParameters();
  p.encodings[0].maxBitrate = 2000000;
  await tr.sender.setParameters(p);
  await a.setLocalDescription();
  await b.setRemoteDescription(a.localDescription);
  await b.setLocalDescription();
  await a.setRemoteDescription(b.localDescription);
};
</script>`;

const browser = await chromium.launch({ executablePath: chrome, args: [
  "--use-fake-ui-for-media-stream",
  "--use-fake-device-for-media-stream",
  "--disable-background-timer-throttling",
  "--disable-renderer-backgrounding",
  "--disable-backgrounding-occluded-windows",
]  });
try {
  const tab = await browser.newPage();
  // A secure origin for getUserMedia, served by the route below; nothing leaves the machine.
  await tab.route("https://capture.invalid/", (r) => r.fulfill({ body: page, contentType: "text/html" }));
  await tab.goto("https://capture.invalid/");
  console.error("chromium", browser.version());
  await tab.evaluate((items) => window.start(items), inject);
  const want = new Set(inject.map((i) => i.id));
  const deadline = Date.now() + 180000;
  let log;
  for (;;) {
    await new Promise((r) => setTimeout(r, 500));
    log = await tab.evaluate(() => window.log);
    const sentIds = new Map(log.sent.filter((s) => s.id !== null).map((s) => [s.ts, s.id]));
    const got = new Set(log.recv.filter((r) => sentIds.has(r.ts)).map((r) => sentIds.get(r.ts)));
    const recvTs = new Set(log.recv.map((r) => r.ts));
    const own = log.sent.filter((s) => s.id === null && recvTs.has(s.ts));
    const ownAfterKey = own.length - own.findIndex((s) => s.key);
    const ready = own.some((s) => s.key) && ownAfterKey >= 3;
    if (([...want].every((id) => got.has(id)) && ready) || Date.now() > deadline) break;
  }
  writeFileSync(outPath, JSON.stringify({ chromium: browser.version(), ...log }));
} finally {
  await browser.close();
}
