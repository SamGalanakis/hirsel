import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {Client, type Socket} from '@hirsel/client';
import {NodeStorage} from '@hirsel/client/node';
import {Frame} from './generated/frame.js';
import {bind, identity, tables, want} from './generated/entries.js';

const produced = await readFile(new URL('./payload.bin', import.meta.url));
const replyEnd = 4 + produced.readUInt32LE();
const payload = produced.subarray(4, replyEnd);
const batch = produced.subarray(replyEnd);
const caller = {issuer: 2, subject: new Uint8Array(16).fill(1)};
let diagnostic: (error: Error) => void = console.error;

function openSocket(): Socket {
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
        emit({tag: 'Welcome', values: [caller, 1, 262144n, identity.digest, identity.target, identity.n_place]});
        emit({tag: 'Placed', value: {tag: 'Ready', value: 0}});
        emit({tag: 'Placed', value: {tag: 'Ready', value: 1}});
      } else if (frame.tag === 'Subscribe') {
        const [id, slot, args] = frame.values;
        assert.equal(id, 0);
        assert.equal(slot, 1);
        assert.deepEqual(args, new Uint8Array());
        queueMicrotask(() => socket.onmessage?.({data: batch}));
      } else {
        assert.equal(frame.tag, 'Call');
        const [stream, p, slot, , args] = frame.values;
        assert.equal(slot, 0);
        assert.deepEqual(args, new Uint8Array());
        emit({tag: 'Batch', values: [1n, [], [], [{stream, p, outcome: {tag: 'Returned', value: payload}}]]});
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
  const expected: Awaited<ReturnType<typeof api.call.packed_reply>> = {
    tag: 'Returned', value: {tag: 'Value', values: [{tag: 'Some', value: {tag: 'None'}}, 9007199254740993n]},
  };
  assert.deepEqual(await api.call.packed_reply(undefined), expected);
  assert.deepEqual(tables.map(({name, id, owner, kind}) => [name, id, owner, kind]),
    [['items', 1, 0, 1], ['by_id', 2, 1, 2]]);
  type ItemRow = {slot: 'items'; key: bigint; value: {tag: 'None'} | {tag: 'Some'; value: {title: string; done: boolean}}};
  const rows = await new Promise<ItemRow[]>((resolve, reject) => {
    diagnostic = reject;
    const sub = api.subscribeDecoded.packed_rows(undefined, current => {
      assert.deepEqual(current.value(), {tag: 'Ready', value: undefined});
      const rows = current.rows('items');
      if (rows.tag === 'Ready') resolve(rows.value);
      else reject(new Error('completion did not publish the rows'));
    }, reject);
    assert.deepEqual(sub.value(), {tag: 'Pending'});
  });
  assert.deepEqual(rows, [{slot: 'items', key: 9007199254740993n,
    value: {tag: 'Some', value: {title: 'crème', done: true}}}]);
} finally {
  await client.close();
}
