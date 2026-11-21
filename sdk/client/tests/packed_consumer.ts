import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {Client, type Socket} from '@hirsel/client';
import {NodeStorage} from '@hirsel/client/node';
import {Frame} from './generated/frame.js';
import {bind, identity, want} from './generated/entries.js';

const payload = await readFile(new URL('./payload.bin', import.meta.url));
const caller = {issuer: 2, subject: new Uint8Array(16).fill(1)};

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
  storage: new NodeStorage('state'), onDiagnostic: console.error,
});
try {
  await client.whenReady();
  const api = bind(client);
  // The fixture returns Value(Some(None), 2^53 + 1). The wire call wraps it in Returned.
  const expected: Awaited<ReturnType<typeof api.call.packed_reply>> = {
    tag: 'Returned', value: {tag: 'Value', values: [{tag: 'Some', value: {tag: 'None'}}, 9007199254740993n]},
  };
  assert.deepEqual(await api.call.packed_reply(undefined), expected);
} finally {
  await client.close();
}
