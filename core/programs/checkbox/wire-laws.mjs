// Real socket laws. Run against a fresh place for each selector.
import assert from 'node:assert/strict';
import {randomBytes} from 'node:crypto';
import {request} from 'node:http';
import {mkdtemp, rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {setTimeout as pause} from 'node:timers/promises';
import {join, resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import {loadRuntime} from '../store/tests/client.mjs';

const [name, origin, generated] = process.argv.slice(2);
const url = new URL('/hirsel', origin);
url.protocol = 'ws:';
const imported = file => import(pathToFileURL(resolve(generated, file)).href);
const {Frame} = await imported('frame.js');
const {values, keys} = await imported('codec.js');
const {want, identity, bind} = await imported('entries.js');
const sockets = new Set();

const some = value => ({tag: 'Some', value});

async function raw(credential = some(randomBytes(16)), entries = want) {
  const socket = new WebSocket(url, 'hirsel.2');
  sockets.add(socket);
  socket.binaryType = 'arraybuffer';
  const queue = [];
  let wake;
  const push = event => {
    queue.push(event);
    wake?.();
  };
  const opened = new Promise((ready, reject) => {
    socket.addEventListener('open', ready, {once: true});
    socket.addEventListener('error', reject, {once: true});
  });
  socket.addEventListener('message', event => push(Frame.decode(new Uint8Array(event.data))));
  socket.addEventListener('close', event => push({tag: 'Close', value: event.code}));
  await opened;
  const send = frame => socket.send(Frame.encode(frame));
  const next = async () => {
    while (!queue.length) await new Promise(ready => { wake = ready; });
    wake = undefined;
    return queue.shift();
  };
  send({tag: 'Hello', values: [credential, entries]});
  return {socket, send, next};
}

async function welcome(credential, entries = want) {
  const client = await raw(credential, entries);
  const answer = await client.next();
  assert.equal(answer.tag, 'Welcome');
  const slots = [];
  for (const _ of entries) {
    const placed = await client.next();
    assert.equal(placed.tag, 'Placed');
    assert.equal(placed.value.tag, 'Ready');
    slots.push(placed.value.value);
  }
  return {...client, slots};
}

function call(client, stream, p, bit) {
  client.send({tag: 'Call', values: [stream, p, client.slots[0], 30000, values.u32.encode(bit)]});
}

async function image(client, sub) {
  client.send({tag: 'Subscribe', values: [sub, client.slots[1], values.unit.encode(undefined), none]});
  for (;;) {
    const update = await client.next();
    assert.equal(update.tag, 'Batch');
    const [, complete, rows] = update.values;
    if (!complete.includes(sub)) continue;
    const result = new Uint8Array(125000);
    for (const row of rows) {
      if (row.sub !== sub || row.value.tag !== 'Some') continue;
      const start = keys.u32.decode(row.key.subarray(4)) * 64;
      result.set(values.bytes.decode(row.value.value).subarray(0, result.length - start), start);
    }
    return result;
  }
}

async function toggle_smoke() {
  const runtime = await loadRuntime();
  const state = await mkdtemp(join(tmpdir(), 'checkbox-client-'));
  let client;
  try {
    client = await runtime.Client.open({url: url.href, frame: Frame, wants: want, identity, credential: randomBytes(16),
      storage: new runtime.NodeStorage(state)});
    await client.whenReady();
    const {toggle} = bind(client).call;
    assert.deepEqual(await toggle(3), {tag: 'Returned', value: {tag: 'Ok', value: true}});
    assert.deepEqual(await toggle(1000000), {
      tag: 'Refused', value: {tag: 'Malformed', value: {kind: 6, at: 0n}},
    });
  } finally {
    await client?.close();
    await runtime.dispose();
    await rm(state, {force: true, recursive: true});
  }
}

async function bit_pixels() {
  const client = await welcome();
  const expected = new Uint8Array(125000);
  let p = 0n;
  for (const [index, offset, mask] of [[0, 0, 1], [7, 0, 128], [8, 1, 1],
    [511, 63, 128], [512, 64, 1], [999999, 124999, 128]]) {
    call(client, 0, p, index);
    const batch = await client.next();
    assert.equal(batch.tag, 'Batch');
    assert.deepEqual(batch.values[3], [{stream: 0, p, outcome: {
      tag: 'Returned', value: Uint8Array.of(0, 0, 0, 0, 1),
    }}]);
    expected[offset] ^= mask;
    assert.deepEqual(await image(client, Number(p)), expected);
    p++;
  }
}

async function toggle_no_rerun() {
  const subscribers = await Promise.all(Array.from({length: 8}, () => welcome()));
  const callPeer = await welcome();
  const sub = 1;
  await Promise.all(subscribers.map(async (peer) => {
    peer.send({tag: 'Subscribe', values: [sub, peer.slots[1], values.unit.encode(undefined), none]});
    const initial = await peer.next();
    assert.equal(initial.tag, 'Batch');
    assert.deepEqual(initial.values[1], [sub]);
    assert.deepEqual(initial.values[2], [{sub, key: new Uint8Array(), value: none}]);
  }));
  let runs = subscribers.length;
  for (let i = 0; i < 1000; i++) {
    const start = performance.now();
    const p = BigInt(i);
    const checked = i % 2 === 0;
    call(callPeer, 0, p, 3);
    const response = await callPeer.next();
    assert.equal(response.tag, 'Batch');
    assert.deepEqual(response.values[3], [{stream: 0, p, outcome: {
      tag: 'Returned', value: Uint8Array.of(0, 0, 0, 0, Number(checked)),
    }}]);
    await Promise.all(subscribers.map(async (peer) => {
      const update = await peer.next();
      assert.equal(update.tag, 'Batch');
      assert.equal(update.values[0], response.values[0]);
      const rows = update.values[2];
      const boardRows = rows.filter(row => row.key.length > 0);
      runs += rows.length - boardRows.length;
      const chunk = new Uint8Array(64);
      chunk[0] = checked ? 8 : 0;
      assert.equal(boardRows.length, 1);
      assert.equal(keys.u32.decode(boardRows[0].key.subarray(4)), 0);
      assert.deepEqual(values.bytes.decode(boardRows[0].value.value), chunk);
    }));
    await pause(Math.max(0, 50 - (performance.now() - start)));
  }
  assert.equal(runs, 8, 'each board query should run once, at Subscribe');
  console.log('8 board queries for 8 subscriptions and 1,000 durable toggles');
}

function upgradeStatus() {
  return new Promise((finish, reject) => {
    const upgrade = request(new URL('/hirsel', origin), {headers: {
      Connection: 'Upgrade', Upgrade: 'websocket', 'Sec-WebSocket-Version': '13',
      'Sec-WebSocket-Key': randomBytes(16).toString('base64'), 'Sec-WebSocket-Protocol': 'hirsel.2',
    }}, response => {
      response.resume();
      finish(response.statusCode);
    });
    upgrade.on('upgrade', (response, socket) => { socket.destroy(); finish(response.statusCode); });
    upgrade.on('error', reject);
    upgrade.end();
  });
}

async function owner_limit_503() {
  for (let owner = 0; owner < 10000; owner++) {
    try {
      await welcome(undefined, []);
    } catch (error) {
      throw new Error(`only ${owner} owners welcomed`, {cause: error});
    }
  }
  assert.equal(await upgradeStatus(), 503);
}

const laws = {toggle_smoke, bit_pixels, toggle_no_rerun, owner_limit_503};
try {
  assert(laws[name], `unknown law: ${name}`);
  await laws[name]();
  console.log(`PASS ${name}`);
} finally {
  for (const socket of sockets) socket.close();
}
