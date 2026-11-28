import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {Client, type Installation, type Socket, type Storage, type Subscription} from '@hirsel/client';
import {MemoryStorage} from '@hirsel/client/memory';
import {NodeStorage} from '@hirsel/client/node';
import type {Part} from './generated/codec.js';
import {reopenWhileCalling, watchItem} from './subscription-cache.ts';

// The generated codecs are frozen at module initialization. Observe their real methods
// before freezing them, then restore Object.freeze before opening the client.
const codecCounts = new WeakMap<object, {encodes: number; decodes: number}>();
let countExchange = false;
async function loadCodecs() {
  const freeze = Object.freeze;
  Object.freeze = <T,>(value: T): Readonly<T> => {
    if (value && typeof value === 'object' &&
        'encode' in value && typeof value.encode === 'function' &&
        'decode' in value && typeof value.decode === 'function') {
      const {encode, decode} = value;
      const counts = {encodes: 0, decodes: 0};
      codecCounts.set(value, counts);
      value.encode = (input: unknown) => {
        if (countExchange) counts.encodes++;
        return encode.call(value, input);
      };
      value.decode = (input: unknown) => {
        if (countExchange) counts.decodes++;
        return decode.call(value, input);
      };
    }
    return freeze(value);
  };
  try {
    return await import('./generated/codec.js');
  } finally {
    Object.freeze = freeze;
  }
}
const codec = await loadCodecs();
type Frame = import('./generated/frame.js').Frame;
const {Frame} = codec;
const {bind, identity, kind, tables, want} = await import('./generated/entries.js');

const produced = await readFile(new URL('./payload.bin', import.meta.url));
const replyEnd = 4 + produced.readUInt32LE();
const payload = produced.subarray(4, replyEnd);
const batch = produced.subarray(replyEnd);
const caller = {issuer: 2, subject: new Uint8Array(16).fill(1)};
let diagnostic: (error: Error) => void = console.error;

// MemoryStorage hands every caller a mutable working shell while keeping the committed shell
// isolated: mutations before or after write, or without one, never become committed state.
async function memoryStorageIsolation() {
  const storage = new MemoryStorage(), key = 'pending-' + '0'.repeat(32);
  const record: Installation['streams'][string] = {
    caller, next: 0n, retired: false, minter: null, sent: false,
    pending: {p: 0n, want: {identity: new Uint8Array([1]), args_fp: 1n, result_fp: 2n},
      args: new Uint8Array([7]), slot: 0, inflight: false, untouched: true, remintsLeft: 1},
  };
  await storage.write({version: 4, installation: 2n, generation: 1n, directory: [key], streams: {[key]: record}});
  record.next = 9n;
  record.pending!.inflight = true;
  const committed = await storage.read();
  assert.equal(committed?.streams[key].next, 0n, 'mutating the written record must not commit');
  assert.equal(committed?.streams[key].pending?.inflight, false, 'mutating the written pending shell must not commit');
  committed!.streams[key].next = 8n;
  committed!.streams[key].pending!.untouched = false;
  const reread = await storage.read();
  assert.equal(reread?.streams[key].next, 0n, 'mutating a read record without write must not commit');
  assert.equal(reread?.streams[key].pending?.untouched, true, 'mutating a read pending shell without write must not commit');
}
await memoryStorageIsolation();

type SentCall = {stream: {t: bigint; n: bigint}; p: bigint; args: Uint8Array};
function openSocket(captured?: SentCall[], onSend?: () => void): Socket {
  let version = 1n;
  const emit = (frame: Frame) => {
    const data = Frame.encode(frame);
    queueMicrotask(() => socket.onmessage?.({data}));
  };
  const socket: Socket = {
    binaryType: '', onopen: null, onmessage: null, onclose: null, onerror: null,
    send(data) {
      onSend?.();
      const frame = Frame.decode(data);
      if (frame.tag === 'Hello') {
        assert.deepEqual(frame.values[1], want);
        emit({tag: 'Welcome', values: [caller, 1, 262144n, identity.digest, identity.target, identity.n_place, 14000000n, 5184000000n]});
        want.forEach((_, i) => emit({tag: 'Placed', value: {tag: 'Ready', value: i}}));
      } else if (frame.tag === 'Subscribe') {
        const [id, slot, args] = frame.values;
        assert.equal(id, 0);
        assert.equal(slot, 1);
        assert.deepEqual(args, new Uint8Array());
        const initial = Frame.decode(batch);
        assert.equal(initial.tag, 'Batch');
        version = initial.values[0];
        queueMicrotask(() => socket.onmessage?.({data: batch}));
      } else {
        assert.equal(frame.tag, 'Call');
        const [stream, p, slot, , args] = frame.values;
        assert.equal(slot, 0);
        if (captured) captured.push({stream, p, args: args.slice()});
        else assert.deepEqual(args, new Uint8Array());
        emit({tag: 'Batch', values: [version, [], [], [{stream, p, outcome: {tag: 'Returned', value: payload}}]]});
      }
    },
    close() {},
  };
  queueMicrotask(() => socket.onopen?.());
  return socket;
}

const wireCounts = {callEncodes: 0, batchDecodes: 0, sends: 0};
const clientFrame = {
  encode(frame: Frame) {
    if (countExchange && frame.tag === 'Call') wireCounts.callEncodes++;
    return Frame.encode(frame);
  },
  decode(bytes: Uint8Array) {
    const frame = Frame.decode(bytes);
    if (countExchange && frame.tag === 'Batch') wireCounts.batchDecodes++;
    return frame;
  },
};
const client = await Client.open({
  url: 'ws://scripted', frame: clientFrame, wants: want, kinds: kind, identity,
  socket: () => openSocket(undefined, () => { if (countExchange) wireCounts.sends++; }),
  storage: new NodeStorage('state'), onDiagnostic: message => diagnostic(new Error(message)),
});
try {
  await client.whenReady();
  const api = bind(client);
  // The fixture returns Value(Some(None), 2^53 + 1). The wire call wraps it in Returned.
  const expected: Awaited<ReturnType<typeof api.call.packed_consumer.packed_reply>> = {
    tag: 'Returned', value: {tag: 'Value', values: [{tag: 'Some', value: {tag: 'None'}}, 9007199254740993n]},
  };
  assert.deepEqual(await api.call.packed_consumer.packed_reply(undefined), expected);
  const clone = globalThis.structuredClone;
  let clones = 0;
  globalThis.structuredClone = value => { clones++; return clone(value); };
  countExchange = true;
  try {
    assert.deepEqual(await api.call.packed_consumer.packed_reply(undefined), expected);
  } finally {
    countExchange = false;
    globalThis.structuredClone = clone;
  }
  assert.deepEqual({
    argumentEncodes: codecCounts.get(codec.Args0)?.encodes,
    resultDecodes: codecCounts.get(codec.Result0)?.decodes,
    ...wireCounts,
  }, {argumentEncodes: 1, resultDecodes: 1, callEncodes: 1, batchDecodes: 1, sends: 1},
  'one steady-state packed exchange must perform each client codec operation and wire send once');
  assert.equal(clones, 0, 'steady-state dispatch/settle must not deep-clone lane state');

  // Raw wire calls own their input bytes on submission and their returned bytes on completion.
  // Mutating either one must not change what went on the wire or make a duplicate outcome look different.
  const captured: SentCall[] = [], diagnostics: string[] = [];
  let rawSocket: Socket | undefined;
  const memory = new MemoryStorage();
  const raw = await Client.open({
    url: 'ws://scripted', frame: Frame, wants: want, kinds: kind, identity,
    socket: () => rawSocket = openSocket(captured), storage: memory,
    onDiagnostic: message => diagnostics.push(message),
  });
  await raw.whenReady();
  const input = Buffer.from([7]);
  const result = raw.call(want[0], input, bytes => bytes);
  input[0] = 99;
  const outcome = await result;
  assert.equal(outcome.tag, 'Returned');
  assert.deepEqual(captured[0]?.args, new Uint8Array([7]), 'mutating submitted args must not alter the wire request');
  if (outcome.tag !== 'Returned') throw new Error('unreachable');
  const answer = outcome.value, original = answer.slice(), sent = captured[0];
  answer[0] ^= 0xff;
  rawSocket!.onmessage?.({data: Frame.encode({tag: 'Batch', values: [2n, [], [], [
    {stream: sent.stream, p: sent.p, outcome: {tag: 'Returned', value: original}},
  ]]})});
  await new Promise(resolve => setTimeout(resolve, 10));
  assert.equal(diagnostics.some(message => /disagree|mismatch/.test(message)), false,
    'mutating a returned result must not alias the stored terminal outcome');
  await raw.close();

  // A write failed after an earlier commit leaves that earlier committed state intact: the
  // rolled-back in-flight mutation is never half-visible in MemoryStorage.
  const inner = new MemoryStorage();
  let armed = false, writes = 0;
  let committed: Installation | null = null;
  const failing: Storage = {
    lock: (name, wait) => inner.lock(name, wait),
    read: records => inner.read(records),
    write: async (state, records) => {
      if (!armed) return inner.write(state, records);
      writes++;
      if (writes === 2) throw new Error('abort write');
      await inner.write(state, records);
      if (writes === 1) committed = await inner.read();
    },
  };
  const aborted = await Client.open({
    url: 'ws://scripted', frame: Frame, wants: want, kinds: kind, identity,
    socket: () => openSocket(), storage: failing,
  });
  await aborted.whenReady();
  armed = true;
  void aborted.call(want[0], new Uint8Array(), bytes => bytes).catch(() => {});
  await new Promise(resolve => setTimeout(resolve, 20));
  assert.ok(committed, 'the first dispatch write should commit before the aborted write');
  assert.deepEqual(await inner.read(), committed, 'aborted write must not expose partial mutations');
  await aborted.close();
  assert.deepEqual(tables.map(({name, id, owner, kind}) => [name, id, owner, kind]),
    [['items', 1, 0, 1], ['by_id', 2, 1, 2]]);
  type ItemRow = {slot: 'items'; key: bigint; value: {tag: 'None'} | {tag: 'Some'; value: {title: string; done: boolean}}};
  const item: ItemRow = {slot: 'items', key: 9007199254740993n,
    value: {tag: 'Some', value: {title: 'crème', done: true}}};
  const displayed: string[] = [];
  const sub = await new Promise<Subscription<undefined, {items: ItemRow}, {items: bigint}>>((resolve, reject) => {
    diagnostic = reject;
    const sub = watchItem(api, item.key, (current, change, text) => {
      if (change.invalidated) return;
      displayed.push(text);
      assert.deepEqual(current.value(), {tag: 'Ready', value: undefined});
      const rows = current.rows('items');
      if (rows.tag !== 'Ready') {
        reject(new Error('completion did not publish the rows'));
        return;
      }
      const item = rows.value[0];
      assert.deepEqual(current.get('items', item.key), {tag: 'Ready', value: item});
      assert.strictEqual(change.rows[0].new, item);
      resolve(current);
    }, reject);
    assert.deepEqual(sub.value(), {tag: 'Pending'});
  });
  assert.deepEqual(sub.rows('items'), {tag: 'Ready', value: [item]});
  assert.deepEqual(await reopenWhileCalling(api, sub, item.key), expected);
  assert.deepEqual(displayed, ['crème: done (total: 1)', 'crème: open (total: 1)', 'crème: done (total: 1)']);
  let ran = false;
  assert.deepEqual(await api.call.packed_consumer.packed_reply(undefined, {optimistic(draft) {
    if (false) {
      // @ts-expect-error The generated slot's key is a bigint.
      draft.put(sub, 'items', 1, item);
      // @ts-expect-error The generated slot's key is a bigint.
      draft.delete(sub, 'items', 1);
    }
    assert.deepEqual(draft.get(sub, 'items', item.key), item);
    draft.put(sub, 'items', item.key, item);
    draft.value(sub, undefined);
    ran = true;
  }}), expected);
  assert(ran);
} finally {
  await client.close();
}

// The room: what only the SDK decides. A set() before membership becomes the member's
// first state, a reconnect re-sends the newest one to the new membership, a direct
// send names its peer, and a Part applies joined, states, messages, left in order.
type Sent = Extract<Frame, {tag: 'Set' | 'Send' | 'Subscribe' | 'Unsubscribe'}>;
const sent: Sent[] = [];
const sockets: Socket[] = [];
const roomSocket = (): Socket => {
  const emit = (frame: Frame) => { const data = Frame.encode(frame); queueMicrotask(() => socket.onmessage?.({data})); };
  const socket: Socket & {bufferedAmount: number} = {
    binaryType: '', bufferedAmount: 0, onopen: null, onmessage: null, onclose: null, onerror: null,
    send(data) {
      const frame = Frame.decode(data);
      if (frame.tag === 'Hello') {
        emit({tag: 'Welcome', values: [caller, 1, 262144n, identity.digest, identity.target, identity.n_place, 14000000n, 5184000000n]});
        want.forEach((_, i) => emit({tag: 'Placed', value: {tag: 'Ready', value: i}}));
      } else if (frame.tag === 'Set' || frame.tag === 'Send' || frame.tag === 'Subscribe' || frame.tag === 'Unsubscribe') {
        sent.push(frame);
        if (frame.tag === 'Subscribe') {
          const [id, slot] = frame.values;
          assert.equal(slot, 2);
          emit({tag: 'Batch', values: [1n, [id], [{sub: id, value: codec.Value2.encode({tag: 'Join'}), rows: []}], []]});
        }
      }
    },
    close() {},
  };
  sockets.push(socket);
  queueMicrotask(() => socket.onopen?.());
  return socket;
};
const live = (part: Partial<Part> & {sub: number}): Frame => ({tag: 'Live', value: [{
  complete: false, you: {tag: 'None'}, incarnation: [1n, 2n], revision: 1n, snapshot: true,
  joined: [], states: [], msgs: [], left: [], gap: 0, lost: 0, full: false, ...part,
}]});
const deliver = async (frame: Frame) => {
  sockets.at(-1)!.onmessage?.({data: Frame.encode(frame)});
  await new Promise(resolve => setTimeout(resolve, 0));
};
const roomClient = await Client.open({
  url: 'ws://scripted', frame: Frame, wants: want, kinds: kind, identity, socket: roomSocket, storage: new NodeStorage('room-state'),
  clock: {now: () => performance.now(), random: () => 0, set: (fn, ms) => setTimeout(fn, ms), clear: id => clearTimeout(id as never)},
});
try {
  await roomClient.whenReady();
  const api = bind(roomClient);
  const lost: number[] = [], heard: string[] = [], seatEvents: string[] = [], peerSeat: string[] = [];
  const roster = (peers: ReturnType<typeof room.peers>) =>
    [...peers].map(([id, p]) => `${id}:${p.state ? `${p.state.x},${p.state.y}` : '-'}`).join(' ');
  const room = api.room.packed_consumer.packed_room(undefined, {
    onLost: n => lost.push(n),
    onSeat: (self, previous) => seatEvents.push(`${previous ?? 'none'}>${self ?? 'none'}`),
    onPeers: (peers, change) => {
      heard.push(`${change.renewed ? 'roster' : 'peers'} ${roster(peers)}`);
      peerSeat.push(`${change.renewed ? 'renewed' : 'continued'}:${change.selfChanged ? 'self-changed' : 'self-same'}`);
    },
    onMessage: (from, note, direct) => heard.push(`${from} ${direct ? 'direct' : 'room'} ${note.tag} ${roster(room.peers())}`),
  });
  const waitFor = async (done: () => boolean) => {
    for (let i = 0; i < 100 && !done(); i++) await new Promise(resolve => setTimeout(resolve, 1));
    assert(done());
  };
  await waitFor(() => room.value().tag !== 'Pending');
  assert.deepEqual(room.value(), {tag: 'Ready', value: {tag: 'Join'}});
  const first = (sent[0] as Extract<Sent, {tag: 'Subscribe'}>).values[0];
  room.set({x: 1, y: 2});
  room.set({x: 3, y: 4});
  room.send({tag: 'Ping', value: 1n});
  assert.equal(room.self, undefined);
  assert.deepEqual(lost, [1]);
  assert.equal(sent.length, 1, 'nothing room-bound is sent before membership');
  // @ts-expect-error A room's state is its registered State type.
  if (false) room.set({x: 'left'});
  assert.throws(() => room.send({tag: 'Text', value: 'x'.repeat(64)}), RangeError);

  await deliver(live({sub: first, complete: true, you: {tag: 'Some', value: 5}, joined: [[5, caller]]}));
  assert.equal(room.self, 5);
  assert.deepEqual(sent.slice(1), [{tag: 'Set', values: [first, {tag: 'Some', value: codec.State2.encode({x: 3, y: 4})}]}]);

  room.send({tag: 'Text', value: 'hi'}, 7);
  assert.deepEqual(sent.at(-1), {tag: 'Send', values: [first, {tag: 'Some', value: 7}, codec.Message2.encode({tag: 'Text', value: 'hi'})]});
  await deliver(live({sub: first, joined: [[7, caller]], states: [[7, {tag: 'Some', value: codec.State2.encode({x: 8, y: 9})}]],
    msgs: [[7, true, codec.Message2.encode({tag: 'Ping', value: 2n})]], left: [7]}));
  assert.deepEqual(heard, ['roster 5:-', '7 direct Ping 5:- 7:8,9', 'peers 5:-']);

  // A new session: the old membership ends, the room resubscribes under a fresh id, and the
  // newest set() reaches the new membership once its `you` arrives.
  room.set({x: 6, y: 7});
  const before = sent.length;
  sockets.at(-1)!.onclose?.({code: 1006});
  await waitFor(() => sent.slice(before).some(f => f.tag === 'Subscribe'));
  assert.equal(room.self, undefined);
  assert.deepEqual(seatEvents, ['none>5', '5>none'], 'ending the session reports the lost seat');
  assert.equal(heard.at(-1), 'peers 5:-', 'the roster stays as last told across a session end');
  const again = sent.slice(before).find(f => f.tag === 'Subscribe') as Extract<Sent, {tag: 'Subscribe'}>;
  assert(again.values[0] > first);
  await deliver(live({sub: again.values[0], complete: true, you: {tag: 'Some', value: 6}, joined: [[6, caller]]}));
  assert.equal(room.self, 6);
  assert.deepEqual(seatEvents, ['none>5', '5>none', 'none>6'], 'rejoining with a new peer id reports the new seat');
  assert.deepEqual(peerSeat, ['renewed:self-changed', 'continued:self-same', 'continued:self-changed'],
    'the same-incarnation rejoin marks selfChanged even though renewed is false');
  assert.deepEqual(heard.at(-1), 'peers 6:-', 'the same incarnation\'s image omits the old admission 5');
  assert.deepEqual(sent.at(-1), {tag: 'Set', values: [again.values[0], {tag: 'Some', value: codec.State2.encode({x: 6, y: 7})}]});
  room.leave();
  await waitFor(() => sent.at(-1)?.tag === 'Unsubscribe');
  assert.deepEqual(sent.at(-1), {tag: 'Unsubscribe', value: again.values[0]});

  // Roster identity (webrtc RTC-8, RUNNER-SPEC Incarnation and revision), by hand: a roster
  // starts at revision 1 and each admission or removal adds 1. The two little-endian words
  // carry the 16 incarnation bytes in order.
  const a: [bigint, bigint] = [0x0807060504030201n, 0x100f0e0d0c0b0a09n], aHex = '0102030405060708090a0b0c0d0e0f10';
  const b: [bigint, bigint] = [0xf0n, 0x0fn << 56n], bHex = 'f000000000000000000000000000000f';
  const other = {issuer: 2, subject: new Uint8Array(16).fill(3)};
  const changes: string[] = [];
  const life = api.room.packed_consumer.packed_room(undefined, {
    onPeers: (peers, c) => changes.push(`${c.incarnation === aHex ? 'A' : c.incarnation === bHex ? 'B' : '?'}@${c.revision ?? '-'}`
      + `${c.renewed ? ' renewed' : ''} +${c.joined} -${c.left} ~${c.set} [${roster(peers)}]`),
  });
  const subscribed = async (after: number) => {
    await waitFor(() => sent.slice(after).some(f => f.tag === 'Subscribe'));
    return (sent.slice(after).find(f => f.tag === 'Subscribe') as Extract<Sent, {tag: 'Subscribe'}>).values[0];
  };
  const reconnect = async () => { const at = sent.length; sockets.at(-1)!.onclose?.({code: 1006}); return subscribed(at); };
  let id = await subscribed(sent.length - 1);
  const s11 = {tag: 'Some' as const, value: codec.State2.encode({x: 1, y: 1})};
  await deliver(live({sub: id, complete: true, you: {tag: 'Some', value: 1}, incarnation: a, revision: 2n, joined: [[1, caller]]}));
  await deliver(live({sub: id, incarnation: a, revision: 3n, joined: [[2, other]], states: [[2, s11]]}));
  await deliver(live({sub: id, incarnation: a, revision: 4n, joined: [[3, other]]}));
  await deliver(live({sub: id, incarnation: a, revision: 5n, left: [3]}));
  assert.deepEqual([life.incarnation, life.revision, life.self], [aHex, 5n, 1]);
  assert.deepEqual(changes, ['A@2 renewed +1 - ~ [1:-]', 'A@3 +2 - ~2 [1:- 2:1,1]', 'A@4 +3 - ~ [1:- 2:1,1 3:-]', 'A@5 + -3 ~ [1:- 2:1,1]']);

  // The same incarnation across a reconnect: during the gap the relay removed admission 1
  // (6) and admitted this client as 4 (7). The image spans two Parts; peer 2 continues with
  // its state, and peer 1, absent at the snapshot, leaves there.
  changes.length = 0;
  id = await reconnect();
  assert.deepEqual([life.self, life.incarnation, life.revision, roster(life.peers())], [undefined, aHex, 5n, '1:- 2:1,1']);
  await deliver(live({sub: id, complete: true, you: {tag: 'Some', value: 4}, incarnation: a, revision: 7n, snapshot: false,
    joined: [[2, other], [4, caller]], states: [[2, s11]]}));
  assert.equal(life.revision, undefined, 'an image is partial until its snapshot');
  assert.deepEqual(roster(life.peers()), '1:- 2:1,1 4:-');
  await deliver(live({sub: id, incarnation: a, revision: 7n}));
  assert.deepEqual([life.self, life.incarnation, life.revision], [4, aHex, 7n]);
  assert.deepEqual(life.peers().get(2)!.caller, other);
  assert.deepEqual(changes, ['A@- +4 - ~2 [1:- 2:1,1 4:-]', 'A@7 + -1 ~ [2:1,1 4:-]']);

  // A new incarnation: the room emptied during the gap and a different admission holds
  // peer 2 again. The roster is replaced, never merged; revision 3 is two admissions.
  changes.length = 0;
  id = await reconnect();
  await deliver(live({sub: id, complete: true, you: {tag: 'Some', value: 1}, incarnation: b, revision: 3n,
    joined: [[1, caller], [2, caller]]}));
  assert.deepEqual([life.self, life.incarnation, life.revision], [1, bHex, 3n]);
  assert.deepEqual(changes, ['B@3 renewed +1,2 -2,4 ~ [1:- 2:-]']);
  life.leave();
} finally {
  await roomClient.close();
}
