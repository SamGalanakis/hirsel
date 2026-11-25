import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {Client, type Socket, type Subscription} from '@hirsel/client';
import {NodeStorage} from '@hirsel/client/node';
import {Frame} from './generated/frame.js';
import * as codec from './generated/codec.js';
import {bind, identity, tables, want} from './generated/entries.js';
import {reopenWhileCalling, watchItem} from './subscription-cache.ts';

const produced = await readFile(new URL('./payload.bin', import.meta.url));
const replyEnd = 4 + produced.readUInt32LE();
const payload = produced.subarray(4, replyEnd);
const batch = produced.subarray(replyEnd);
const caller = {issuer: 2, subject: new Uint8Array(16).fill(1)};
let diagnostic: (error: Error) => void = console.error;

function openSocket(): Socket {
  let version = 1n;
  const emit = (frame: Frame) => {
    const data = Frame.encode(frame);
    queueMicrotask(() => socket.onmessage?.({data}));
  };
  const socket: Socket = {
    binaryType: '', onopen: null, onmessage: null, onclose: null, onerror: null,
    send(data) {
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
        assert.deepEqual(args, new Uint8Array());
        emit({tag: 'Batch', values: [version, [], [], [{stream, p, outcome: {tag: 'Returned', value: payload}}]]});
      }
    },
    close() {},
  };
  queueMicrotask(() => socket.onopen?.());
  return socket;
}

const client = await Client.open({
  url: 'ws://scripted', frame: Frame, wants: want, identity, socket: openSocket,
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
const live = (part: Partial<codec.Part> & {sub: number}): Frame => ({tag: 'Live', value: [{
  complete: false, you: {tag: 'None'}, incarnation: [1n, 2n], revision: 1n, snapshot: true,
  joined: [], states: [], msgs: [], left: [], gap: 0, lost: 0, full: false, ...part,
}]});
const deliver = async (frame: Frame) => {
  sockets.at(-1)!.onmessage?.({data: Frame.encode(frame)});
  await new Promise(resolve => setTimeout(resolve, 0));
};
const roomClient = await Client.open({
  url: 'ws://scripted', frame: Frame, wants: want, identity, socket: roomSocket, storage: new NodeStorage('room-state'),
  clock: {now: () => performance.now(), random: () => 0, set: (fn, ms) => setTimeout(fn, ms), clear: id => clearTimeout(id as never)},
});
try {
  await roomClient.whenReady();
  const api = bind(roomClient);
  const lost: number[] = [], heard: string[] = [];
  const roster = (peers: ReturnType<typeof room.peers>) =>
    [...peers].map(([id, p]) => `${id}:${p.state ? `${p.state.x},${p.state.y}` : '-'}`).join(' ');
  const room = api.room.packed_consumer.packed_room(undefined, {
    onLost: n => lost.push(n),
    onPeers: (peers, change) => heard.push(`${change.renewed ? 'roster' : 'peers'} ${roster(peers)}`),
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
  assert.equal(heard.at(-1), 'peers 5:-', 'the roster stays as last told across a session end');
  const again = sent.slice(before).find(f => f.tag === 'Subscribe') as Extract<Sent, {tag: 'Subscribe'}>;
  assert(again.values[0] > first);
  await deliver(live({sub: again.values[0], complete: true, you: {tag: 'Some', value: 9}, joined: [[9, caller]]}));
  assert.equal(room.self, 9);
  assert.deepEqual(heard.at(-1), 'peers 9:-', 'the same incarnation\'s image omits the old admission 5');
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
