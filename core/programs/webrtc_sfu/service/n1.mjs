// N1 browser_room (webrtc.md): real Chromium and Firefox against the SFU deployment.
//
//   node core/programs/webrtc_sfu/service/n1.mjs --sfu <work>/sfu --web <work>/web [--case NAME]...
//
// Each scenario starts its own service (`run.py --build-only` builds it), opens pages with fake
// devices, and judges by getStats (inbound bytes and frames, the selected pair's candidate
// type) and by what the service prints (TURN allocations, revocations), never by timing alone.
// Playwright (playwright-core 1.63.0) is installed into ~/.cache/hirsel/n1 on first use.
import assert from 'node:assert/strict';
import {spawn, execFileSync} from 'node:child_process';
import {existsSync, mkdirSync} from 'node:fs';
import {createRequire} from 'node:module';
import {homedir} from 'node:os';
import {join, resolve} from 'node:path';

const PLAYWRIGHT = '1.63.0';

function option(name, fallback) {
  const i = process.argv.indexOf(`--${name}`);
  return i < 0 ? fallback : process.argv[i + 1];
}
function options(name) {
  return process.argv.flatMap((a, i) => a === `--${name}` ? [process.argv[i + 1]] : []);
}

function playwright() {
  const cache = join(homedir(), '.cache', 'hirsel', 'n1');
  const pkg = join(cache, 'node_modules', 'playwright-core', 'package.json');
  if (!existsSync(pkg) || createRequire(import.meta.url)(pkg).version !== PLAYWRIGHT) {
    mkdirSync(cache, {recursive: true});
    execFileSync('npm', ['install', '--prefix', cache, '--no-save', '--no-audit', '--no-fund', `playwright-core@${PLAYWRIGHT}`], {stdio: 'inherit'});
    execFileSync(join(cache, 'node_modules', '.bin', 'playwright-core'), ['install', 'chromium', 'firefox'], {stdio: 'inherit'});
  }
  return createRequire(join(cache, 'node_modules', '/'))('playwright-core');
}

const sfu = resolve(option('sfu', 'sfu'));
const listenPort = Number(option('port', '7800'));
const mediaPort = Number(option('media', '7810'));
const turnPort = Number(option('turn', '3478'));
const only = options('case');
const pw = playwright();
const results = [];
const open = new Set();

function log(...a) {
  console.log(`[${new Date().toISOString().slice(11, 23)}]`, ...a);
}

// One service run: its stdout lines, and a waiter on them.
async function service(extra = []) {
  const args = [`--hirsel-n-place=1048576`, '--listen', `127.0.0.1:${listenPort}`, '--media', `127.0.0.1:${mediaPort}`,
    '--turn', `127.0.0.1:${turnPort}`, ...extra];
  const child = spawn(sfu, args, {stdio: ['ignore', 'pipe', 'pipe']});
  const lines = [];
  let partial = '';
  const onData = chunk => {
    partial += chunk;
    const parts = partial.split('\n');
    partial = parts.pop();
    for (const line of parts) {
      lines.push(line);
      if (!line.startsWith('turn ')) log('service:', line);
    }
  };
  child.stdout.setEncoding('utf8').on('data', onData);
  child.stderr.setEncoding('utf8').on('data', onData);
  const s = {
    child, lines,
    async until(test, ms, what) {
      const end = Date.now() + ms;
      while (!lines.some(test)) {
        if (child.exitCode !== null) throw new Error(`service exited (${child.exitCode}) waiting for ${what}`);
        if (Date.now() > end) throw new Error(`service did not print ${what}`);
        await new Promise(r => setTimeout(r, 50));
      }
      return lines.find(test);
    },
    async stop() {
      if (child.exitCode === null) {
        child.kill('SIGTERM');
        await new Promise(r => { child.once('exit', r); setTimeout(r, 3000); });
        if (child.exitCode === null) child.kill('SIGKILL');
      }
    },
  };
  await s.until(l => l.startsWith('listening '), 60000, 'listening');
  await s.until(l => l.startsWith('turn listening '), 10000, 'turn listening');
  return s;
}

const browsers = {};
async function browser(kind) {
  if (browsers[kind]) return browsers[kind];
  if (kind === 'chromium') {
    browsers[kind] = await pw.chromium.launch({args: [
      '--use-fake-device-for-media-stream', '--use-fake-ui-for-media-stream',
      '--allow-loopback-in-peer-connection', '--autoplay-policy=no-user-gesture-required',
    ]});
  } else {
    browsers[kind] = await pw.firefox.launch({env: process.env.N1_MOZ_LOG ? {...process.env, MOZ_LOG: process.env.N1_MOZ_LOG, MOZ_LOG_FILE: process.env.N1_MOZ_LOG_FILE} : undefined, firefoxUserPrefs: {
      // Fake devices that count as capturing (permission.fake): without it Firefox gathers
      // on the default route only, never on loopback, whatever ice.loopback says.
      'media.navigator.streams.fake': true, 'media.navigator.permission.disabled': true,
      'media.navigator.permission.fake': true, 'media.peerconnection.ice.loopback': true,
      'media.peerconnection.ice.obfuscate_host_addresses': false,
      'media.autoplay.default': 0, 'media.autoplay.block-webaudio': false,
    }});
  }
  return browsers[kind];
}

async function page(kind, name) {
  const context = await (await browser(kind)).newContext();
  const p = await context.newPage();
  p.on('console', m => { if (m.type() === 'error' || m.type() === 'warning' || m.text().startsWith('n1: ')) log(`${name}:`, m.text()); });
  p.on('pageerror', e => log(`${name} pageerror:`, e.message));
  await p.goto(`http://127.0.0.1:${listenPort}/`);
  await p.waitForFunction(() => window.n1 !== undefined);
  open.add(p);
  p.on('close', () => open.delete(p));
  // Every page call is bounded, so a hung promise fails the case instead of the run.
  const bounded = (what, promise, ms = 60000) => Promise.race([promise,
    new Promise((_, reject) => setTimeout(() => reject(new Error(`${name}: ${what} did not settle in ${ms} ms`)), ms))]);
  const call = (fn, arg) => bounded(fn, p.evaluate(([f, a]) => window.n1[f](...a), [fn, arg]));
  return {
    kind, name, page: p, context,
    start: o => call('start', [o]),
    subscribe: (id, k, e) => p.evaluate(([i, k, e]) => window.n1.subscribe(BigInt(i), k, e), [id, k, e]),
    retarget: (id, e) => p.evaluate(([i, e]) => window.n1.retarget(BigInt(i), e), [id, e]),
    level: v => call('level', [v]),
    leave: () => call('leave', []),
    stats: () => call('stats', []),
    state: () => call('state', []),
    async close() { await context.close(); },
  };
}

const sleep = ms => new Promise(r => setTimeout(r, ms));

// Wait until the page has seen `n` Published signals from other peers.
async function publications(p, n) {
  await p.page.waitForFunction(n => window.n1.state().published.filter(x => x.peer !== window.n1.state().self).length >= n, n, {timeout: 20000});
  const s = await p.state();
  return s.published.filter(x => x.peer !== s.self);
}

// The inbound-rtp entry of the receiving section a subscription filled.
function inboundOf(stats, id) {
  const sub = stats.subs.find(s => s.id === String(id));
  assert(sub, `no subscription ${id}`);
  return stats.inbound.find(i => i.mid === sub.mid);
}

// Wait until `test(stats)` holds, sampling every 250 ms.
async function poll(p, test, ms, what) {
  const end = Date.now() + ms;
  let last;
  while (Date.now() < end) {
    last = await p.stats();
    if (test(last)) return last;
    await sleep(250);
  }
  throw new Error(`${p.name}: ${what}; last stats ${JSON.stringify(last)}`);
}

async function bytesOf(p, id) {
  const i = inboundOf(await p.stats(), id);
  return i ? i.bytes : 0;
}

async function check(name, fn) {
  if (only.length && !only.includes(name)) return;
  log(`=== ${name}`);
  const started = Date.now();
  try {
    await fn();
    results.push({name, ok: true});
    log(`PASS ${name} (${((Date.now() - started) / 1000).toFixed(1)} s)`);
  } catch (e) {
    results.push({name, ok: false, error: String(e.stack || e)});
    log(`FAIL ${name}: ${e.stack || e}`);
    for (const p of open) {
      const s = await p.evaluate(() => window.n1.state()).catch(() => undefined);
      if (s) log(`page ${p.url()} self ${s.self}:\n  ${(s.remote ?? '').split('\r\n').filter(l => /^a=(fingerprint|candidate|setup)/.test(l)).join('\n  ')}\n  ${(s.local ?? '').split('\r\n').filter(l => /^a=(fingerprint|setup)/.test(l)).join('\n  ')}\n  ${s.log.join('\n  ')}\n  signals ${JSON.stringify(s.signals, (_, v) => typeof v === 'bigint' ? String(v) : v)}`);
    }
  }
}

// Two pages, each publishing Opus and three-encoding VP8 and subscribing to the other's.
async function pair(kinds, relay) {
  const [a, b] = [await page(kinds[0], `${kinds[0]}-a`), await page(kinds[1], `${kinds[1]}-b`)];
  const o = {peers: 2, audio: true, video: true, receiveAudio: 2, receiveVideo: 2, relay, turn: `127.0.0.1:${turnPort}`};
  await Promise.all([a.start(o), b.start(o)]);
  if (process.env.N1_SDP) for (const x of [a, b]) {
    const st = await x.state();
    log(`${x.name} local:\n${st.local}\n${x.name} remote:\n${st.remote}`);
  }
  for (const [x, y] of [[a, b], [b, a]]) {
    const pubs = await publications(x, 2);
    for (const p of pubs) await x.subscribe(p.id, p.kind === 0 ? 'audio' : 'video', 2);
    x.pubs = pubs;
    x.peer = y;
  }
  return [a, b];
}

const audioOf = p => p.pubs.find(x => x.kind === 0);
const videoOf = p => p.pubs.find(x => x.kind !== 0);

// Both directions carry media the subscriber decodes, and the publisher sends three encodings.
async function flowing(x, pairType) {
  const s = await poll(x, s => {
    const a = inboundOf(s, audioOf(x).id), v = inboundOf(s, videoOf(x).id);
    return a && v && a.bytes > 5000 && v.frames > 30 && s.pair;
  }, 30000, 'no audio and video frames from the other page');
  const encoding = s.outbound.filter(o => o.kind === 'video' && o.bytes > 0);
  assert.equal(encoding.length, 3, `${x.name} sends ${encoding.length} encodings, not three: ${JSON.stringify(s.outbound)}`);
  if (pairType) assert.equal(s.pair.local, 'relay', `${x.name} selected ${JSON.stringify(s.pair)}`);
  log(`${x.name}: audio ${inboundOf(s, audioOf(x).id).bytes} B, video ${inboundOf(s, videoOf(x).id).frames} frames ` +
      `${inboundOf(s, videoOf(x).id).width}x${inboundOf(s, videoOf(x).id).height}, pair ${JSON.stringify(s.pair)}`);
  return s;
}

async function scenarioCall() {
  const svc = await service();
  let a, b;
  try {
    // N1_CALL=chromium,chromium (or any two kinds) swaps the call's browsers, to tell a
    // browser's behaviour from the SFU's.
    await check('connect', async () => { [a, b] = await pair((process.env.N1_CALL ?? 'chromium,firefox').split(',')); });
    if (!a) return;
    // A quiet room link stays up: the service's room client pings after its idle window
    // (runner's wire_client), so after N1_QUIET_S quiet seconds it is still the same peer.
    await check('quiet_room_keeps_service', async () => {
      if (!process.env.N1_QUIET_S) return;
      const quietEnd = Date.now() + Number(process.env.N1_QUIET_S) * 1000;
      while (Date.now() < quietEnd) { await sleep(5000); log(`roster ${(await a.state()).peers}`); }
      for (const x of [a, b]) {
        const st = await x.state();
        assert(st.peers.includes(st.service), `${x.name}: service ${st.service} is gone from the room: ${st.peers}`);
      }
    });
    await check('publish_subscribe_both_directions', async () => {
      await flowing(a);
      await flowing(b);
    });
    // RTP-14: a Chromium subscriber keeps flexfec-03 (page.ts preferVp8), takes the
    // service's re-offer with an `a=ssrc-group:FEC-FR` pair for its video section, and
    // receives FlexFEC repair packets there (keyframes are protected at any loss). A
    // Firefox subscriber, which offers no flexfec-03, is answered without it and gets
    // no FEC-FR pair.
    await check('flexfec_per_leg', async () => {
      for (const x of [a, b]) {
        const v = videoOf(x).id;
        const st = await x.state();
        const mid = (await x.stats()).subs.find(s => s.id === String(v)).mid;
        const section = (st.remote ?? '').split(/\r\nm=/).find(m => m.includes(`\r\na=mid:${mid}\r\n`)) ?? '';
        if (x.kind === 'chromium') {
          assert(/a=rtpmap:\d+ flexfec-03\/90000/.test(section), `${x.name}: the service dropped flexfec-03 from section ${mid}`);
          assert(/a=ssrc-group:FEC-FR \d+ \d+/.test(section), `${x.name}: no FEC-FR pair in section ${mid}`);
          const s = await poll(x, s => (inboundOf(s, v)?.fec ?? 0) > 0, 30000, 'no FlexFEC packets on the video section');
          log(`${x.name}: ${inboundOf(s, v).fec} FlexFEC packets, ${inboundOf(s, v).fecDiscarded} discarded, ${inboundOf(s, v).nacks} NACKs`);
        } else {
          assert(!/flexfec-03/.test(section) && !/FEC-FR/.test(section), `${x.name}: FlexFEC in a section it never offered: ${section}`);
        }
      }
    });
    // The subscriber retargets the publisher's video to encoding 0 (rid q) and back to 2
    // (rid f), and its decoded width follows the width the publisher reports sending on that
    // rid (outbound-rtp; encodings are indexed in the offer's simulcast order q, h, f).
    await check('layer_switch', async () => {
      const RID = ['q', 'h', 'f'];
      for (const x of [a, b]) {
        const v = videoOf(x).id;
        // The encoding the subscriber decodes: the rid whose publisher-side width it shows.
        const current = async () => {
          const sent = (await x.peer.stats()).outbound.filter(o => o.kind === 'video' && o.width > 0);
          const got = inboundOf(await x.stats(), v);
          if (!got || !(got.width > 0)) return undefined;
          const e = RID.findIndex(r => sent.some(o => o.rid === r && o.width === got.width));
          return e < 0 ? undefined : {e, width: got.width};
        };
        // Wait until the subscriber decodes an encoding `ok` accepts.
        const settle = async (ok, what) => {
          const end = Date.now() + Number(process.env.N1_SETTLE_MS ?? 30000);
          let next = 0;
          while (Date.now() < end) {
            const c = await current();
            if (c && ok(c.e)) return c;
            if (Date.now() > next) { next = Date.now() + 5000; log(`${x.name}: decoding ${JSON.stringify(c)}; receiving ${JSON.stringify(inboundOf(await x.stats(), v))}`); }
            await sleep(250);
          }
          throw new Error(`${x.name}: ${what}; sending ${JSON.stringify((await x.peer.stats()).outbound)}, receiving ${JSON.stringify(inboundOf(await x.stats(), v))}`);
        };
        // RTC-6: the effective target is the target lowered by the subscriber's estimate, so
        // Target(f) may land below f; it must still lie above q, and Target(q) must be q.
        const first = await settle(e => e > 0, 'never received a layer above q');
        await x.retarget(v, 0);
        const low = await settle(e => e === 0, 'never received the q layer after Target(q)');
        assert(low.width < first.width, `${x.name}: q is ${low.width} wide and ${RID[first.e]} ${first.width}`);
        const framesAt = inboundOf(await x.stats(), v).frames;
        await poll(x, s => inboundOf(s, v).frames > framesAt + 10, 10000, 'the q layer stopped');
        await x.retarget(v, 2);
        const back = await settle(e => e > 0, 'never left q after Target(f)');
        log(`${x.name}: ${RID[first.e]} ${first.width} -> q ${low.width} -> ${RID[back.e]} ${back.width} wide`);
      }
    });
    // Firefox leaves the room; the service ends its seat (Revoke, then Fenced to the room),
    // and Chromium's sections from it receive nothing more.
    await check('revoke_stops_media', async () => {
      await b.leave();
      await a.page.waitForFunction(() => window.n1.state().fenced.length > 0, null, {timeout: 20000});
      await sleep(1000);
      const first = [await bytesOf(a, audioOf(a).id), await bytesOf(a, videoOf(a).id)];
      await sleep(3000);
      const second = [await bytesOf(a, audioOf(a).id), await bytesOf(a, videoOf(a).id)];
      assert.deepEqual(second, first, `media still arrives after Fenced: ${first} -> ${second}`);
      // Firefox's own receive side ends too: its seat is gone.
      const b1 = [await bytesOf(b, audioOf(b).id), await bytesOf(b, videoOf(b).id)];
      await sleep(2000);
      const b2 = [await bytesOf(b, audioOf(b).id), await bytesOf(b, videoOf(b).id)];
      assert.deepEqual(b2, b1, `the revoked page still receives: ${b1} -> ${b2}`);
    });
  } finally {
    await a?.close(); await b?.close();
    await svc.stop();
  }
}

// The revoke the other way round: Chromium leaves, Firefox stops receiving from it.
async function scenarioRevokeChromium() {
  if (only.length && !only.includes('revoke_stops_media_chromium_leaves')) return;
  const svc = await service();
  let a, b;
  try {
    await check('revoke_stops_media_chromium_leaves', async () => {
      [b, a] = await pair(['firefox', 'chromium']);
      await flowing(b);
      await a.leave();
      await b.page.waitForFunction(() => window.n1.state().fenced.length > 0, null, {timeout: 20000});
      await sleep(1000);
      const first = [await bytesOf(b, audioOf(b).id), await bytesOf(b, videoOf(b).id)];
      await sleep(3000);
      const second = [await bytesOf(b, audioOf(b).id), await bytesOf(b, videoOf(b).id)];
      assert.deepEqual(second, first, `media still arrives after Fenced: ${first} -> ${second}`);
    });
  } finally {
    await a?.close(); await b?.close();
    await svc.stop();
  }
}

// audio_forward = 1: of two audio publications only the speaking one is forwarded (RTC-6 via
// the RFC 6464 level); swapping who speaks swaps which section receives.
async function scenarioAudioForward(subscriberKind) {
  const name = `audio_forward_gate_${subscriberKind}`;
  if (only.length && !only.includes(name)) return;
  const svc = await service(['--audio-forward', '1']);
  const pages = [];
  try {
    await check(name, async () => {
      const loud = await page('chromium', 'loud');
      const quiet = await page('firefox', 'quiet');
      const sub = await page(subscriberKind, `${subscriberKind}-listener`);
      pages.push(loud, quiet, sub);
      const all = {peers: 3, turn: `127.0.0.1:${turnPort}`};
      await Promise.all([
        loud.start({...all, audio: true, level: 0.8}),
        quiet.start({...all, audio: true, level: 0}),
        sub.start({...all, receiveAudio: 2}),
      ]);
      const pubs = await publications(sub, 2);
      // Which publication is whose: the pages report their own peer ids.
      const loudSelf = (await loud.state()).self, quietSelf = (await quiet.state()).self;
      const L = pubs.find(p => p.peer === loudSelf), Q = pubs.find(p => p.peer === quietSelf);
      assert(L && Q, `publications ${JSON.stringify(pubs)}`);
      await sub.subscribe(L.id, 'audio', 0);
      await sub.subscribe(Q.id, 'audio', 0);
      await poll(sub, s => (inboundOf(s, L.id)?.bytes ?? 0) > 3000, 20000, 'the speaking publication is not forwarded');
      const q1 = await bytesOf(sub, Q.id), l1 = await bytesOf(sub, L.id);
      await sleep(2000);
      const q2 = await bytesOf(sub, Q.id), l2 = await bytesOf(sub, L.id);
      assert(l2 > l1 + 2000, `the speaking publication stalled: ${l1} -> ${l2}`);
      assert(q2 - q1 < 200, `the silent publication is forwarded with audio_forward 1: ${q1} -> ${q2}`);
      log(`loud ${l1} -> ${l2}, quiet ${q1} -> ${q2}`);
      // Swap: the quiet page speaks and the loud one falls silent.
      await quiet.level(0.8);
      await loud.level(0);
      await poll(sub, s => (inboundOf(s, Q.id)?.bytes ?? 0) > q2 + 3000, 20000, 'the new speaker is never forwarded');
      await sleep(1000);
      const l3 = await bytesOf(sub, L.id), q3 = await bytesOf(sub, Q.id);
      await sleep(2000);
      const l4 = await bytesOf(sub, L.id), q4 = await bytesOf(sub, Q.id);
      assert(q4 > q3 + 2000, `the new speaker stalled: ${q3} -> ${q4}`);
      assert(l4 - l3 < 200, `the now-silent publication is still forwarded: ${l3} -> ${l4}`);
      log(`after swap: loud ${l3} -> ${l4}, quiet ${q3} -> ${q4}`);
    });
  } finally {
    for (const p of pages) await p.close();
    await svc.stop();
  }
}

// iceTransportPolicy "relay": every byte goes through the service's TURN relay over `transport`.
// Each TURN request the service captured, by the subject in its USERNAME (`expiry:subject`):
// the method with its integrity attributes (MI is MESSAGE-INTEGRITY, MI256 its SHA-256 form,
// PA a PASSWORD-ALGORITHM), and the XOR-PEER-ADDRESS ports of CreatePermission and ChannelBind.
function turnRequests(lines) {
  const names = {0x001: 'Binding', 0x003: 'Allocate', 0x004: 'Refresh', 0x008: 'CreatePermission', 0x009: 'ChannelBind'};
  const by = {};
  for (const line of lines) {
    const m = /^turn (udp|tcp) \d+ ([0-9a-f]+)$/.exec(line);
    if (!m) continue;
    const b = Buffer.from(m[2], 'hex');
    const t = b.readUInt16BE(0);
    const method = ((t & 0x3e00) >> 2) | ((t & 0x00e0) >> 1) | (t & 0x000f);
    let user = 'anonymous';
    const attrs = [];
    const ports = [];
    for (let at = 20; at + 4 <= b.length;) {
      const type = b.readUInt16BE(at), len = b.readUInt16BE(at + 2);
      if (type === 0x0006) user = b.subarray(at + 4, at + 4 + len).toString('utf8').split(':').pop();
      if (type === 0x0008) attrs.push('MI');
      if (type === 0x001c) attrs.push('MI256');
      if (type === 0x001d) attrs.push('PA');
      if (type === 0x0012) ports.push(b.readUInt16BE(at + 6) ^ 0x2112);
      at += 4 + ((len + 3) & ~3);
    }
    const seen = by[user] ??= {kinds: new Set(), permit: new Set(), bind: new Set()};
    seen.kinds.add(`${names[method] ?? method.toString(16)}${attrs.length ? `+${attrs.join('+')}` : ''}`);
    if (method === 0x008) ports.forEach(p => seen.permit.add(p));
    if (method === 0x009) ports.forEach(p => seen.bind.add(p));
  }
  return by;
}

async function scenarioRelay(transport) {
  const name = `relay_only_${transport}`;
  if (only.length && !only.includes(name)) return;
  const svc = await service();
  let a, b;
  try {
    await check(name, async () => {
      [a, b] = await pair(['chromium', 'firefox'], transport);
      for (const x of [a, b]) {
        const s = await flowing(x, 'relay');
        assert.equal(s.pair.relayProtocol ?? transport, transport, `${x.name} relays over ${s.pair.relayProtocol}`);
      }
      // The service's relay holds an allocation for each page.
      await svc.until(l => /^allocations ([2-9]|\d\d)/.test(l), 5000, 'two allocations');
      // Leaving revokes the TURN credentials the admission minted (TURN-10, webrtc.md "Strict firewalls").
      await b.leave();
      await svc.until(l => l.startsWith('revoked '), 10000, 'a TURN revocation');
      // What each browser's TURN client sent, from the service's `turn` captures: the
      // integrity attributes on its requests (turn.md's MD5 question) and the peer ports
      // its CreatePermissions carry (the RFC 8656 section 9.1 question).
      const self = {[(await a.state()).self]: a.name, [(await b.state()).self]: b.name};
      for (const [who, seen] of Object.entries(turnRequests(svc.lines))) {
        const peer = who.split('-').pop();
        log(`${name}: ${self[peer] ?? who} sent ${[...seen.kinds].sort().join(', ')}; CreatePermission ports ${[...seen.permit].sort().join(' ') || 'none'}; ChannelBind ports ${[...seen.bind].sort().join(' ') || 'none'}`);
      }
    });
  } finally {
    await a?.close(); await b?.close();
    await svc.stop();
  }
}

try {
  await scenarioCall();
  await scenarioRevokeChromium();
  await scenarioAudioForward('chromium');
  await scenarioAudioForward('firefox');
  await scenarioRelay('udp');
  await scenarioRelay('tcp');
} finally {
  for (const b of Object.values(browsers)) await b.close();
}
const failed = results.filter(r => !r.ok);
log(`N1: ${results.length - failed.length}/${results.length} passed`);
for (const r of results) log(`${r.ok ? 'PASS' : 'FAIL'} ${r.name}`);
process.exit(failed.length || !results.length ? 1 : 0);
