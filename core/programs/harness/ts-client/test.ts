// Drives client.ts against a running scripted gateway (the `gateway` program beside
// this file) through a same-origin server that also serves example.html, as a page
// would use it. The oracle is a fresh reader: a raw stream's first image plus raw
// history pages, parsed here without client.ts.
//
//   node test.ts <built gateway>          run the checks
//   node test.ts <built gateway> serve    serve the example page and keep running
import { spawn } from "node:child_process";
import { randomBytes } from "node:crypto";
import { readFileSync } from "node:fs";
import http from "node:http";
import { stripTypeScriptTypes } from "node:module";
import { Client, type State } from "./client.ts";

const here = new URL(".", import.meta.url);
const gateway = spawn(process.argv[2], [], { stdio: ["pipe", "pipe", "inherit"] });
let controlReply: { ok: (id: number) => void; fail: (error: Error) => void } | null = null;
let downs = 0;
const upstream = await new Promise<number>((ok, fail) => {
  gateway.on("exit", (code) => {
    const error = new Error(`gateway exited ${code}`);
    fail(error);
    controlReply?.fail(error);
  });
  let out = "";
  gateway.stdout.on("data", (b: Buffer) => {
    out += b;
    for (let i; (i = out.indexOf("\n")) >= 0; out = out.slice(i + 1)) {
      const line = out.slice(0, i);
      const [word, n] = line.split(" ");
      if (word === "port") ok(Number(n));
      if (word === "down") downs = Number(n);
      if (word === "control" || word === "control-error") {
        const reply = controlReply;
        controlReply = null;
        if (word === "control") reply?.ok(Number(n));
        else reply?.fail(new Error(line));
      }
    }
  });
});

// ---- the same-origin server, with faults the checks switch on ----
let dropAnswer = false; // forward the next POST, then cut the browser's connection unanswered
let dropped = 0;
let busyHistory = false;
let history503s = 0;
let streamsOpened = 0;
const streams = new Set<http.ServerResponse>();
const files: Record<string, [string, string]> = {
  "/": ["text/html", readFileSync(new URL("example.html", here), "utf8")],
  "/client.js": ["text/javascript", stripTypeScriptTypes(readFileSync(new URL("client.ts", here), "utf8"))],
};
const server = http.createServer((req, res) => {
  const file = files[req.url ?? ""];
  if (file) { res.writeHead(200, { "Content-Type": file[0] }); res.end(file[1]); return; }
  if (busyHistory && req.url?.includes("/history")) {
    busyHistory = false;
    history503s += 1;
    res.writeHead(503).end();
    return;
  }
  const drop = req.method === "POST" && dropAnswer;
  if (drop) dropAnswer = false;
  if (req.url?.includes("/events")) { streamsOpened += 1; streams.add(res); res.on("close", () => streams.delete(res)); }
  const up = http.request({ port: upstream, host: "127.0.0.1", path: req.url, method: req.method, headers: req.headers }, (answer) => {
    if (drop) { dropped += 1; answer.resume(); answer.on("end", () => req.socket.destroy()); return; }
    res.writeHead(answer.statusCode ?? 502, answer.headers);
    answer.pipe(res);
  });
  up.on("error", () => res.destroy());
  res.on("close", () => up.destroy());
  req.pipe(up);
});
await new Promise<void>((ok) => server.listen(0, "127.0.0.1", ok));
const base = `http://127.0.0.1:${(server.address() as { port: number }).port}`;
if (process.argv[3] === "serve") {
  console.log(`open ${base}/`);
  await new Promise(() => {});
}

// ---- the oracle: a fresh reader, parsed by hand ----
const credential = () => randomBytes(16).toString("base64");
async function transcript(session = 0): Promise<Map<number, string>> {
  const auth = { Authorization: `Bearer ${me}` };
  const abort = new AbortController();
  const r = await fetch(`${base}/sessions/${session}/events`, { headers: auth, signal: abort.signal });
  const reader = r.body!.pipeThrough(new TextDecoderStream()).getReader();
  let text = "";
  while (!text.includes("event: batch\n")) {
    const { value, done } = await reader.read();
    if (done) throw new Error("fresh stream ended before its first batch");
    text += value ?? "";
  }
  abort.abort();
  const rows = new Map<number, string>();
  for (const m of text.slice(0, text.indexOf("event: batch\n")).matchAll(/^data: (.*)$/gm)) {
    const row = JSON.parse(m[1]);
    rows.set(row.at, JSON.stringify(row.line));
  }
  let before = Math.min(...[...rows.keys()].filter((at) => at > 0));
  while (before > 1) {
    const page = await (await fetch(`${base}/sessions/${session}/history?before=${before}&lines=3`, { headers: auth })).json();
    for (const row of page.rows) rows.set(row.at, JSON.stringify(row.line));
    before = page.from;
  }
  return rows;
}
const flat = (m: Map<number, string>) => JSON.stringify([...m].sort((a, b) => a[0] - b[0]));
const held = (s: State) => {
  const m = new Map(s.rows.map((r) => [r.at, JSON.stringify(r.line)] as [number, string]));
  m.set(0, JSON.stringify({ Status: s.status }));
  return m;
};

// ---- the checks ----
let state: State | null = null;
const drafts: { text: string; offset: number }[] = [];
const me = credential();
const client = new Client({
  base, session: 0, credential: me, tail: 4, page: 3, staleMs: 4000,
  onChange(s) { state = s; if (s.draft) drafts.push({ text: s.draft.text, offset: s.draft.offset }); },
});
const until = async (what: string, ok: () => boolean, ms = 15000) => {
  for (const end = Date.now() + ms; !ok(); await new Promise((r) => setTimeout(r, 20)))
    if (Date.now() > end) throw new Error(`timed out waiting for ${what}: ${JSON.stringify(state)}`);
};
const check = (what: string, ok: boolean) => { if (!ok) throw new Error(`${what}: ${JSON.stringify(state)}`); console.log(`ok ${what}`); };
const assistant = (s: State | null) => s?.rows.filter((r) => "Line" in r.line && r.line.Line[0] === "assistant").at(-1);
const users = (rows: Map<number, string>) => [...rows.values()].filter((l) => l.includes('"user"')).length;
let forkClient: Client | null = null;
let forkReader: Client | null = null;
const close = () => {
  client.stop(); forkClient?.stop(); forkReader?.stop();
  server.close();
  for (const s of streams) s.destroy();
  gateway.stdin.end();
};
const command = (action: string, at: number): Promise<number> => new Promise((ok, fail) => {
  controlReply = { ok, fail };
  gateway.stdin.write(JSON.stringify({ action, at, credential: me }) + "\n");
});

try {
  check("session created", (await (await fetch(`${base}/sessions`, { method: "POST", body: "k0", headers: { Authorization: `Bearer ${me}` } })).json()) === 0);
  client.start();
  await until("the first image", () => state?.live === true);

  // A send whose answer is lost is resent at the same position and runs once.
  dropAnswer = true;
  const sent = await client.send("The team meeting moved to Thursday.", "k1");
  check("a lost answer is resent and answered", dropped === 1 && sent.status === 202 && client.position === 1);
  await until("the approval hold", () => state?.status === "waiting");

  const waits = await client.pendingWaits();
  check("note_delete is the pending approval", waits.length === 1 && waits[0].run === 0 && waits[0].round === 1 && waits[0].ordinal === 1);
  const { run, round, ordinal, key } = waits[0];
  dropAnswer = true;
  const ruled = await client.decide({ run, round, ordinal, key, allow: true });
  check("the decision is approved once", dropped === 2 && ruled.status === 200 && ruled.body === "Approved" && client.position === 2);
  await until("the first answer", () => state?.status === "answered");
  const forkAt = assistant(state)!.at;
  const pinned = await transcript();

  // The place unloads under the open stream, and the stream lives on.
  const opened = streamsOpened;
  const before = downs;
  await until("the place to unload", () => downs > before);
  await new Promise((r) => setTimeout(r, 500));
  check("the stream outlives the unload", streamsOpened === opened && state?.live === true);

  // A later answer is drafted; the committed row replaces the draft.
  drafts.length = 0;
  check("a second send", (await client.send("Thanks.", "k2")).status === 202);
  await until("the second answer", () => state?.status === "answered" && state.draft === null && assistant(state)?.at !== undefined && drafts.length > 2);
  const second = assistant(state)!;
  const answerText = "Line" in second.line ? second.line.Line[1] : "";
  check("every draft is a slice of the answer",
    drafts.filter((d) => d.text !== "running tool").every((d) => answerText.slice(d.offset, d.offset + d.text.length) === d.text) &&
    drafts.some((d) => d.text.length > 0 && d.text !== "running tool"));

  // Cut the stream mid-answer: the client reconnects, and the new image replaces the old.
  drafts.length = 0;
  check("a third send", (await client.send("And the notes?", "k3")).status === 202);
  await until("a draft to start", () => drafts.some((d) => d.text !== "running tool" && d.text.length > 0));
  const cutAt = streamsOpened;
  for (const s of streams) s.destroy();
  await until("the reconnect", () => streamsOpened > cutAt && state?.live === true);
  await until("the third answer", () => state?.status === "answered" && state.draft === null && (assistant(state)?.at ?? 0) > second.at);

  // Page back to the start through a temporary refusal from the server.
  busyHistory = true;
  while (await client.loadOlder());
  check("history retries a 503", history503s === 1);
  const whole = await transcript();
  check("the client holds what a fresh reader sees", flat(held(state!)) === flat(whole));
  check("each message ran once", users(whole) === 3);

  // A Reset drops earlier pages: after a reconnect only the tail is held, until paged again.
  const resetAt = streamsOpened;
  for (const s of streams) s.destroy();
  await until("the reset", () => streamsOpened > resetAt && state?.live === true);
  check("a reset keeps only the new image", (state?.rows.length ?? 0) <= 4 && state!.older);
  while (await client.loadOlder());
  check("paging after a reset rebuilds the transcript", flat(held(state!)) === flat(await transcript()));
  // Fork the first answer's cutoff after two more turns have reached the source.
  const forkSession = await client.fork(forkAt, "fork");
  check("a repeated fork key returns the same session", (await client.fork(forkAt, "fork")) === forkSession);
  let forkState: State | null = null;
  forkClient = new Client({ base, session: forkSession, credential: me, onChange: (s) => { forkState = s; } });
  forkClient.start();
  check("the fork accepts its own turn", (await forkClient.send("fork only", "f1")).status === 202);
  await until("the fork's answer", () => forkState?.status === "answered" && (assistant(forkState)?.at ?? 0) > forkAt);
  forkClient.stop();

  let forkImage: State | null = null;
  forkReader = new Client({ base, session: forkSession, credential: me, tail: 1, page: 64, onChange: (s) => { forkImage = s; } });
  forkReader.start();
  await until("the fork tail", () => forkImage?.live === true && forkImage.status === "answered");
  while (await forkReader.loadOlder());
  const forkRows = held(forkImage!);
  const inherited = new Map([...forkRows].filter(([at]) => at <= forkAt));
  check("fork paging sees the pinned prefix and its own rows",
    flat(inherited) === flat(pinned) && flat(forkRows) === flat(await transcript(forkSession)) &&
    users(forkRows) === 2 && [...forkRows.values()].includes(JSON.stringify({ Line: ["user", "fork only"] })));
  forkReader.stop();

  // Remove the run owner's read access while their draft is visible.
  drafts.length = 0;
  check("a final send", (await client.send("This draft will lose its reader.", "k4")).status === 202);
  await until("a draft before revocation", () => drafts.some((d) => d.text.length > 0));
  await command("revoke", client.position);
  await until("revocation clears the client", () => state?.live === false && state.rows.length === 0 && state.draft === null && state.status === "");
  const refused = await fetch(`${base}/sessions/0/history?before=3&lines=2`, { headers: { Authorization: `Bearer ${me}` } });
  check("revoked history and client stay empty", refused.status === 403 && !(await client.loadOlder()));
  console.log("PASS");
  close();
} catch (e) {
  console.error(`FAIL ${(e as Error).message}`);
  close();
  process.exitCode = 1;
}
