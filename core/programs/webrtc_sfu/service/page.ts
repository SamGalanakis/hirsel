// The browser end of the SFU deployment (webrtc.md N1). The page joins the service's room
// through the shared client SDK, offers once with its publishing and receiving sections,
// and answers the service's re-offers. Signals travel as `signal.Signal` room messages
// (RTC-8). `window.n1` is what the N1 harness drives; the buttons drive the same calls.
// Written by `hirsel sdk ts` (the client SDK and this place's bindings); run.py points esbuild there.
import {Client} from 'generated/client.js';
import {BrowserStorage} from 'generated/browser.js';
import {bind, identity, kind, want} from 'generated/entries.js';
import {Frame} from 'generated/frame.js';

type Signal = any;
const enc = new TextEncoder();
const dec = new TextDecoder();

export interface Start {
  // Peers (the service and other browsers) to wait for before offering.
  peers: number;
  // 'udp' or 'tcp' for a relay-only session over TURN on that transport; absent for direct.
  relay?: 'udp' | 'tcp';
  turn?: string;
  audio?: boolean;
  video?: boolean;
  // Sections the page receives on: one per subscription it may make.
  receiveAudio?: number;
  receiveVideo?: number;
  // The audio level of the published tone, 0 to 1 (0 is silence).
  level?: number;
}

const log: string[] = [];
const signals: {from: number; direct: boolean; tag: string; values: unknown[]}[] = [];
const published: {peer: number; id: bigint; kind: number; encodings: number}[] = [];
const fenced: {peer: number; epoch: bigint}[] = [];
let room: any;
let pc: RTCPeerConnection | undefined;
let service: number | undefined;
let held: MediaStream | undefined;
let servers: {username: string; credential: string} | undefined;
let answered = false;
let gain: GainNode | undefined;
let voice: MediaStreamTrack | undefined;
const subs: {id: bigint; kind: 'audio' | 'video'; mid: string | null}[] = [];
const waiters: (() => void)[] = [];

function note(line: string) {
  log.push(`${(performance.now() / 1000).toFixed(3)} ${line}`);
  console.log(`n1: ${line}`);
  const status = document.querySelector('#log');
  if (status) status.textContent = log.slice(-30).join('\n');
}

function wake() {
  for (const w of waiters.splice(0)) w();
}

async function until(test: () => boolean, ms: number, what: string) {
  const end = performance.now() + ms;
  while (!test()) {
    if (performance.now() > end) throw new Error(`timed out waiting for ${what}`);
    await new Promise<void>(resolve => { waiters.push(resolve); setTimeout(resolve, 50); });
  }
}

// The generated codec's shape for a sum value: `{tag}`, `{tag, value}` or `{tag, values}`.
function parts(s: Signal): unknown[] {
  if ('values' in s) return s.values;
  if ('value' in s) return [s.value];
  return [];
}

function say(s: Signal, to?: number) {
  room.send(s, to);
}

async function onSignal(from: number, s: Signal, direct: boolean) {
  const values = parts(s);
  signals.push({from, direct, tag: s.tag, values});
  note(`<- ${from}${direct ? '' : ' (room)'} ${s.tag}`);
  switch (s.tag) {
    case 'Servers': {
      service = from;
      servers = {username: dec.decode(values[0] as Uint8Array), credential: dec.decode(values[1] as Uint8Array)};
      break;
    }
    case 'Answer': {
      if (!direct || !pc) break;
      await pc.setRemoteDescription({type: 'answer', sdp: dec.decode(values[1] as Uint8Array)});
      answered = true;
      break;
    }
    case 'Offer': {
      // The service's re-offer (RTC-4): answer it with the same version.
      if (!direct || !pc) break;
      await pc.setRemoteDescription({type: 'offer', sdp: dec.decode(values[1] as Uint8Array)});
      const answer = await pc.createAnswer();
      await pc.setLocalDescription(answer);
      say({tag: 'Answer', values: [values[0], enc.encode(pc.localDescription!.sdp)]}, from);
      break;
    }
    case 'Published': {
      published.push({peer: Number(values[0]), id: values[1] as bigint, kind: Number(values[2]), encodings: Number(values[3])});
      break;
    }
    case 'Fenced': {
      fenced.push({peer: Number(values[0]), epoch: values[1] as bigint});
      break;
    }
  }
  wake();
}

// A steady 440 Hz tone at `level`, from WebAudio where the browser runs it; headless
// Firefox's AudioContext never leaves "suspended", so there the fake microphone's
// steady tone (media.navigator.streams.fake) stands in. Silence is `enabled = false`,
// which sends digital silence on either source.
async function tone(level: number): Promise<MediaStreamTrack> {
  const context = new AudioContext();
  const running = await Promise.race([context.resume().then(() => true), new Promise<boolean>(r => setTimeout(() => r(false), 1500))]);
  if (running && context.state === 'running') {
    const oscillator = context.createOscillator();
    oscillator.frequency.value = 440;
    gain = context.createGain();
    gain.gain.value = level;
    const sink = context.createMediaStreamDestination();
    oscillator.connect(gain).connect(sink);
    oscillator.start();
    voice = sink.stream.getAudioTracks()[0];
  } else {
    void context.close();
    note('tone from the fake microphone');
    voice = (await navigator.mediaDevices.getUserMedia({audio: true})).getAudioTracks()[0];
  }
  voice.enabled = level > 0;
  return voice;
}

function preferVp8(t: RTCRtpTransceiver) {
  const caps = RTCRtpReceiver.getCapabilities('video');
  if (!caps || !t.setCodecPreferences) return;
  const vp8 = caps.codecs.filter(c => /VP8|rtx/i.test(c.mimeType));
  t.setCodecPreferences(vp8);
}

async function start(o: Start) {
  const url = new URL('/hirsel', location.href);
  url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:';
  const client = await Client.open({
    url: url.href, frame: Frame, wants: want, kinds: kind, identity, storage: new BrowserStorage(`n1-${crypto.randomUUID()}`),
    onDiagnostic: (message: string) => note(`diagnostic ${message}`),
  });
  await client.whenReady();
  const api: any = bind(client);
  room = api.room.main.admit(undefined, {
    onMessage: (from: number, message: Signal, direct: boolean) => { void onSignal(from, message, direct); },
    onPeers: () => wake(),
    onEnd: (reason: unknown) => note(`room ended ${JSON.stringify(reason, (_, v) => typeof v === 'bigint' ? String(v) : v)}`),
  });
  await until(() => room.self !== undefined && room.peers().size >= o.peers + 1 && servers !== undefined, 20000, 'the room and Servers');
  note(`self ${room.self}, service ${service}, ${room.peers().size} peers`);
  const config: RTCConfiguration = {bundlePolicy: 'max-bundle', rtcpMuxPolicy: 'require'};
  if (o.relay) {
    config.iceServers = [{urls: `turn:${o.turn}?transport=${o.relay}`, ...servers!}];
    config.iceTransportPolicy = 'relay';
  }
  // Firefox offers loopback and private host candidates only to a page that is capturing
  // (with media.navigator.permission.fake), so a page that publishes nothing still opens
  // the fake microphone and holds it while the call lasts.
  if (!o.audio && !o.video) held = await navigator.mediaDevices.getUserMedia({audio: true}).catch(() => undefined);
  pc = new RTCPeerConnection(config);
  pc.oniceconnectionstatechange = async () => {
    note(`ice ${pc!.iceConnectionState}`);
    if (pc!.iceConnectionState === 'failed') for (const c of (await stats()).candidates) note(c);
  };
  pc.onconnectionstatechange = async () => {
    note(`connection ${pc!.connectionState}`);
    if (pc!.connectionState !== 'failed') return;
    const report = await pc!.getStats();
    report.forEach((r: any) => {
      if (r.type === 'transport' || r.type === 'candidate-pair' || r.type.endsWith('candidate') || r.type === 'certificate') note(`${r.type} ${JSON.stringify(r)}`);
    });
  };
  if (o.audio) {
    note('tone');
    pc.addTransceiver(await tone(o.level ?? 0.5), {direction: 'sendonly'});
  }
  if (o.video) {
    note('camera');
    const media = await navigator.mediaDevices.getUserMedia({video: {width: 1280, height: 720, frameRate: 30}});
    const t = pc.addTransceiver(media.getVideoTracks()[0], {direction: 'sendonly', sendEncodings: [
      {rid: 'q', scaleResolutionDownBy: 4, maxBitrate: 150000},
      {rid: 'h', scaleResolutionDownBy: 2, maxBitrate: 500000},
      {rid: 'f', maxBitrate: 1500000},
    ]});
    preferVp8(t);
  }
  for (let i = 0; i < (o.receiveAudio ?? 0); i++) pc.addTransceiver('audio', {direction: 'recvonly'});
  for (let i = 0; i < (o.receiveVideo ?? 0); i++) preferVp8(pc.addTransceiver('video', {direction: 'recvonly'}));
  note('offering');
  const offer = await pc.createOffer();
  await pc.setLocalDescription(offer);
  say({tag: 'Offer', values: [1n, enc.encode(pc.localDescription!.sdp)]}, service);
  await until(() => answered, 20000, 'the answer');
  await until(() => pc!.connectionState === 'connected', 20000, 'DTLS');
  return {self: room.self, service};
}

// The receiving sections in the order the service fills them (RTC-8: the first free
// section the server may send on, of the publication's kind).
function receiving(kind: 'audio' | 'video'): RTCRtpTransceiver[] {
  return pc!.getTransceivers().filter(t => t.receiver.track.kind === kind && (t.direction === 'recvonly' || t.currentDirection === 'recvonly' || t.direction === 'sendrecv'));
}

function layer(encoding: number) {
  return {tag: 'Layer', values: [encoding, 0, 255]};
}

function subscribe(id: bigint | number, kind: 'audio' | 'video', encoding = 2) {
  const n = subs.filter(s => s.kind === kind).length;
  const t = receiving(kind)[n];
  subs.push({id: BigInt(id), kind, mid: t?.mid ?? null});
  say({tag: 'Subscribe', values: [BigInt(id), kind === 'video' ? layer(encoding) : layer(0)]}, service);
}

function retarget(id: bigint | number, encoding: number) {
  say({tag: 'Target', values: [BigInt(id), layer(encoding)]}, service);
}

function level(value: number) {
  if (gain) gain.gain.value = value;
  if (voice) voice.enabled = value > 0;
}

function leave() {
  room.leave();
}

async function stats() {
  const report = await pc!.getStats();
  const inbound: Record<string, unknown>[] = [];
  const outbound: Record<string, unknown>[] = [];
  let pair: Record<string, unknown> | undefined;
  const byId = new Map<string, any>();
  report.forEach((s: any) => byId.set(s.id, s));
  report.forEach((s: any) => {
    if (s.type === 'inbound-rtp') {
      inbound.push({mid: s.mid ?? null, kind: s.kind, ssrc: s.ssrc, bytes: s.bytesReceived, packets: s.packetsReceived,
        frames: s.framesDecoded ?? null, width: s.frameWidth ?? null, height: s.frameHeight ?? null,
        lost: s.packetsLost ?? null, nacks: s.nackCount ?? null, plis: s.pliCount ?? null, firs: s.firCount ?? null, dropped: s.framesDropped ?? null, keyframes: s.keyFramesDecoded ?? null});
    } else if (s.type === 'outbound-rtp') {
      outbound.push({mid: s.mid ?? null, kind: s.kind, rid: s.rid ?? null, bytes: s.bytesSent, frames: s.framesEncoded ?? null,
        width: s.frameWidth ?? null, height: s.frameHeight ?? null, limit: s.qualityLimitationReason ?? null,
        plis: s.pliCount ?? null, firs: s.firCount ?? null, nacks: s.nackCount ?? null, keyframes: s.keyFramesEncoded ?? null, target: s.targetBitrate ?? null});
    } else if (s.type === 'candidate-pair' && (s.selected || s.nominated) && s.state === 'succeeded') {
      const local = byId.get(s.localCandidateId);
      const remote = byId.get(s.remoteCandidateId);
      pair = {local: local?.candidateType, relayProtocol: local?.relayProtocol ?? null, protocol: local?.protocol,
        remote: remote?.candidateType, remoteAddress: `${remote?.address}:${remote?.port}`};
    }
  });
  // Transport-level selected pair (Chromium reports it on the transport).
  report.forEach((s: any) => {
    if (s.type === 'transport' && s.selectedCandidatePairId) {
      const p = byId.get(s.selectedCandidatePairId);
      const local = p && byId.get(p.localCandidateId);
      const remote = p && byId.get(p.remoteCandidateId);
      if (local) pair = {local: local.candidateType, relayProtocol: local.relayProtocol ?? null, protocol: local.protocol,
        remote: remote?.candidateType, remoteAddress: `${remote?.address}:${remote?.port}`};
    }
  });
  const candidates: string[] = [];
  report.forEach((s: any) => {
    if (s.type === 'local-candidate' || s.type === 'remote-candidate') {
      candidates.push(`${s.type} ${s.candidateType} ${s.protocol} ${s.address}:${s.port}${s.relayProtocol ? ' via ' + s.relayProtocol : ''}`);
    } else if (s.type === 'candidate-pair') {
      const l = byId.get(s.localCandidateId), r = byId.get(s.remoteCandidateId);
      candidates.push(`pair ${s.state} ${l?.address}:${l?.port} -> ${r?.address}:${r?.port} sent ${s.requestsSent ?? '?'} got ${s.responsesReceived ?? '?'}`);
    }
  });
  return {inbound, outbound, pair, candidates, subs: subs.map(s => ({...s, id: String(s.id)}))};
}

function state() {
  return {
    self: room?.self, service, peers: room ? [...room.peers().keys()].map(Number) : [], log: log.slice(),
    signals: signals.map(s => ({...s, values: s.values.map(v => v instanceof Uint8Array ? `${v.length} bytes` : typeof v === 'bigint' ? String(v) : v)})),
    published: published.map(p => ({...p, id: String(p.id)})),
    fenced: fenced.map(f => ({...f, epoch: String(f.epoch)})),
    remote: pc?.remoteDescription?.sdp,
    local: pc?.localDescription?.sdp,
  };
}

(window as any).n1 = {start, subscribe, retarget, level, leave, stats, state, until};

// By hand: Join publishes a tone and the camera and subscribes to every publication.
document.querySelector('#join')?.addEventListener('click', async () => {
  const relay = (document.querySelector('#relay') as HTMLSelectElement).value as '' | 'udp' | 'tcp';
  await start({peers: 1, audio: true, video: true, receiveAudio: 4, receiveVideo: 4, relay: relay || undefined,
    turn: `${location.hostname}:${(document.querySelector('#turn') as HTMLInputElement).value}`});
  setInterval(() => {
    for (const p of published) {
      if (p.peer !== room.self && !subs.some(s => s.id === p.id)) subscribe(p.id, p.kind === 0 ? 'audio' : 'video');
    }
  }, 500);
});
