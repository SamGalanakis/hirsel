import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { randomBytes } from "node:crypto";
import { createInterface } from "node:readline";
import { setTimeout as sleep } from "node:timers/promises";
import { Client, type State } from "./client.ts";

const [, , executable, mode, message] = process.argv;
const scripted = mode === "--scripted";
const credential = randomBytes(16).toString("base64");
const clients: Client[] = [];
let gateway: ChildProcessWithoutNullStreams | undefined;
let closed: Promise<number | null> | undefined;
let failed: string | undefined;
let port: number | undefined;
let diffLine: string | undefined;
let diffError: string | undefined;
let requests = 0;
let hit = 0;
let miss = 0;
let usageReports = 0;

async function until<T>(read: () => T | undefined, problem: string, ms = 90000): Promise<T> {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    if (failed) throw new Error(failed);
    const value = read();
    if (value !== undefined) return value;
    await sleep(25);
  }
  throw new Error(problem);
}

function startGateway(path: string) {
  const child = spawn(path, []);
  gateway = child;
  child.stderr.pipe(process.stderr);
  closed = new Promise((ok) => child.once("close", ok));
  child.on("error", () => { failed = "Start the gateway with serve.sh, then retry."; });
  child.on("exit", (status) => {
    if (status !== 0) failed ??= "Start the gateway with serve.sh, then retry.";
    for (const client of clients) client.stop();
  });
  createInterface({ input: child.stdout }).on("line", (line) => {
    if (line.startsWith("port ")) port = Number(line.slice(5));
    else if (line.startsWith("diff ")) diffLine = line.slice(5);
    else if (line.startsWith("diff-error ")) diffError = line.slice(11);
    else if (line.startsWith("Build with --limit ")) console.log(line);
    else if (line.startsWith("notes: ")) failed = line.slice(7);
    else if (line === "OpenRouter request attempted") requests += 1;
    else if (line.startsWith("OpenRouter prompt tokens: ")) {
      const match = /^OpenRouter prompt tokens: hit (\d+), miss (\d+)$/.exec(line);
      if (match) {
        usageReports += 1;
        hit += Number(match[1]);
        miss += Number(match[2]);
      }
    } else if (line.startsWith("OpenRouter ")) failed = line;
  });
}

async function createSession(base: string): Promise<number> {
  let response: Response;
  try {
    response = await fetch(`${base}/sessions`, {
      method: "POST",
      body: "harness demo",
      headers: { Authorization: `Bearer ${credential}` },
      signal: AbortSignal.timeout(5000),
    });
  } catch {
    throw new Error("Start the gateway with serve.sh --gateway and use http://127.0.0.1:<port>.");
  }
  if (response.status !== 201) throw new Error(`Could not create the session (HTTP ${response.status}). Check the gateway address and capacity.`);
  return response.json();
}

function open(base: string, session: number) {
  let state: State | undefined;
  const client = new Client({ base, session, credential, onChange(s) { state = s; } });
  clients.push(client);
  client.start();
  return { client, read: () => state };
}

type Session = ReturnType<typeof open>;
const last = (s?: State) => s?.rows.at(-1)?.at ?? 0;
function progress(session: Session, after: number, status: string) {
  return until(() => {
    const s = session.read();
    if (s && last(s) > after) {
      if (s.status === "stopped" || s.status === "out of rounds" || s.status === "cancelled") {
        throw new Error(`The run ${s.status}. Check the gateway output and try a short message.`);
      }
      if (s.status === status) return s;
    }
  }, `Timed out waiting for ${status}. Check the gateway output.`);
}

async function send(session: Session, text: string, key: string) {
  const reply = await session.client.send(text, key);
  if (reply.status !== 202) throw new Error(`Message refused (HTTP ${reply.status}). Check the session and credential.`);
}

function answer(s: State) {
  for (const { at, line } of [...s.rows].reverse()) {
    if ("Line" in line && line.Line[0] === "assistant" && line.Line[1].trim()) return { at, text: line.Line[1].trim() };
  }
  throw new Error("The model returned no text. Try a different prompt.");
}

async function run(base: string, chatOnly: boolean) {
  const parent = open(base, await createSession(base));
  const prompt = scripted || chatOnly ? "Reply with READY and nothing else."
    : 'Use note_add once to save "The team meeting moved to Thursday." After the tool returns, reply with SAVED.';
  await send(parent, message ?? prompt, "chat");
  const chat = await progress(parent, 0, "answered");
  const first = answer(chat);
  if (scripted && first.text !== "READY") throw new Error("Unexpected scripted chat answer.");
  if (!scripted && !chatOnly && !message) {
    const tools = chat.rows.filter(({ line }) => "Line" in line && line.Line[0] === "tool");
    if (tools.length !== 1) throw new Error("Expected one note_add result from the live chat.");
  }
  console.log(`Chat ${first.text}`);
  if (chatOnly) return;

  const forkId = await parent.client.fork(first.at, "demo fork");
  const child = open(base, forkId);
  await send(child, "Reply with FORKED and nothing else.", "fork chat");
  const forked = answer(await progress(child, first.at, "answered"));
  if (scripted && forked.text !== "FORKED") throw new Error("Unexpected scripted fork answer.");
  console.log(`Fork ${forkId} ${forked.text}`);
  child.client.stop();

  gateway!.stdin.write(`diff ${forkId}\n`);
  const raw = await until(() => {
    if (diffError) throw new Error(diffError);
    return diffLine;
  }, "The gateway did not answer the cache diff. Check its output.");
  const diff = JSON.parse(raw);
  if (diff.first_section !== "release-preview" || JSON.stringify(diff.sections) !== '["release-preview"]' || diff.tools || diff.prefix) {
    throw new Error(`Unexpected cache diff ${raw}`);
  }
  console.log(`Cache diff ${raw}`);
  if (!scripted) return;

  await send(parent, "File an example release.", "release");
  await progress(parent, first.at, "waiting");
  const filed = await parent.client.releases();
  if (filed.length !== 1) throw new Error("Expected one filed release. Check the tool results and try again.");
  console.log(`Filed ${filed[0].proposal.summary}`);
  const waiting = last(parent.read());
  const settled = await parent.client.settle(filed[0].id, "Publish");
  if (settled.status !== 200 || settled.body !== "Recorded") throw new Error("The release decision was refused. List the proposals again.");
  const result = answer(await progress(parent, waiting, "answered"));
  if (result.text !== "approved, not deployed") throw new Error("Unexpected scripted release result.");
  if ((await parent.client.releases()).length !== 0) throw new Error("The release is still listed as filed.");
  console.log(`Release ${result.text}`);
}

try {
  if (executable === "--connect") await run(mode, true);
  else {
    startGateway(executable);
    const listening = await until(() => port, "Start the gateway with serve.sh, then retry.", 15000);
    const base = `http://127.0.0.1:${listening}`;
    if (!scripted) console.log(`OpenRouter model ${process.env.OPENROUTER_MODEL}`);
    await run(base, false);
  }
} catch (error) {
  console.error(failed ?? (error as Error).message);
  process.exitCode = 1;
} finally {
  for (const client of clients) client.stop();
  gateway?.stdin.end();
  if (closed && (await closed) !== 0 && !process.exitCode) {
    console.error(failed ?? "The gateway shut down with an error. Check its output.");
    process.exitCode = 1;
  }
  if (mode === "--live") console.log(`OpenRouter requests ${requests}, prompt tokens hit ${hit}, miss ${miss}, usage reports ${usageReports}`);
}
if (!process.exitCode) console.log("PASS");
