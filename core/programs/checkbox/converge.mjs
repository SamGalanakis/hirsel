import assert from 'node:assert/strict';
import {mkdtemp, rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join, resolve} from 'node:path';
import {setTimeout as pause} from 'node:timers/promises';
import {pathToFileURL} from 'node:url';
import {loadRuntime} from '../runner/tests/client.mjs';
import {canvasMatches, openBrowser} from './browser-laws.mjs';

export function seeded(seed) {
  let state = seed >>> 0;
  return () => {
    state = (Math.imul(state, 1664525) + 1013904223) >>> 0;
    return state;
  };
}

function mutate(name, file, text) {
  if (file !== 'client') return text;
  const edits = {
    resend_new_p: ['pending.attempts++;', 'if (pending.attempts > 0) pending.p++; pending.attempts++;'],
    skip_resubscribe_after_reset: ['this.publish(); await this.subscribeAll();', 'this.publish();'],
    clear_on_first_batch: ['complete.includes(sub.id)) changed.set', '(complete.includes(sub.id) || !sub.complete)) changed.set'],
  };
  assert(edits[name], `unknown client mutant ${name}`);
  const [before, after] = edits[name];
  assert.equal(text.split(before).length, 2, name);
  return text.replace(before, after);
}

class Board {
  bits = new Uint8Array(125000);
  version = -1n;
  replacements = 0;
  change({invalidated, replaced, rows}) {
    if (invalidated) this.bits.fill(0);
    if (replaced) this.replacements++;
    for (const {old, new: row} of rows) {
      const offset = (row ?? old).key * 64;
      assert(offset < this.bits.length);
      if (!row) this.bits.fill(0, offset, offset + 64);
      else {
        const bytes = row.value.value;
        assert.equal(bytes.length, 64);
        this.bits.set(bytes.subarray(0, this.bits.length - offset), offset);
      }
    }
  }
}

async function waitFor(check, failure) {
  while (!check()) {
    if (failure.error) throw failure.error;
    await pause(10);
  }
  if (failure.error) throw failure.error;
}

// Close after the server has durably answered the selected call, before the SDK
// can receive that answer. Keep the SDK's own storage, timers and reconnect.
function transport(Frame, failure, {faultAt, resetProbe = false} = {}) {
  let outstanding;
  let dropped;
  let retransmit = false;
  let socket;
  let subscription;
  let resetting = false;
  let heldSubscribe;
  let interposed = false;
  let resetSeen = false;
  let durable = -1n;
  const abort = error => { failure.error ??= new Error(error); };
  return {
    get durable() { return durable; },
    get recovered() { return Boolean(dropped && retransmit); },
    get resetBatch() { return interposed; },
    requestReset() {
      assert(resetProbe && subscription && !resetting);
      resetting = true;
      assert.equal(subscription.tag, 'Subscribe');
      const [sub, slot, args] = subscription.values;
      socket.send(Frame.encode({tag: 'Subscribe', values: [sub, slot, args,
        {tag: 'Some', value: Uint8Array.of(1)}]}));
    },
    releaseReplacement() {
      assert(heldSubscribe, 'Reset must request a replacement subscription before accepting unrelated updates');
      socket.send(heldSubscribe);
      heldSubscribe = undefined;
      resetting = false;
    },
    onBatch() {
      if (resetSeen) interposed = true;
    },
    connect(url, protocol) {
      const wire = new WebSocket(url, protocol);
      socket = wire;
      const adapter = {
        binaryType: 'arraybuffer', onopen: null, onmessage: null, onclose: null, onerror: null,
        send(bytes) {
          const frame = Frame.decode(bytes);
          if (frame.tag === 'Call') {
            const [stream, p, , , arg] = frame.values;
            if (dropped && !retransmit) {
              if (stream.t !== dropped.stream.t || stream.n !== dropped.stream.n ||
                  p !== dropped.p || !Buffer.from(arg).equals(dropped.arg)) {
                abort(`reconnect changed durable request (${dropped.stream.t}, ${dropped.stream.n}, ${dropped.p}) to (${stream.t}, ${stream.n}, ${p})`);
                wire.close();
                return;
              }
              retransmit = true;
            }
            outstanding = {stream, p, arg: Buffer.from(arg)};
          } else if (frame.tag === 'Subscribe') {
            subscription = frame;
            if (resetting && resetSeen) {
              heldSubscribe = bytes;
              return;
            }
          }
          wire.send(bytes);
        },
        close() { wire.close(); },
      };
      wire.binaryType = 'arraybuffer';
      wire.onopen = event => adapter.onopen?.(event);
      wire.onclose = event => adapter.onclose?.(event);
      wire.onerror = event => adapter.onerror?.(event);
      wire.onmessage = event => {
        const frame = Frame.decode(new Uint8Array(event.data));
        if (frame.tag === 'Ended') abort(`board subscription ended: ${frame.values[1].value?.tag}`);
        if (frame.tag === 'Batch' || frame.tag === 'Reset') {
          const version = frame.values[0];
          const outcomes = frame.values.at(-1);
          if (outstanding && outcomes.some(row => row.stream.t === outstanding.stream.t && row.stream.n === outstanding.stream.n &&
              row.p === outstanding.p && row.outcome.tag === 'Returned')) {
            if (!dropped && outstanding.p === faultAt) {
              dropped = outstanding;
              wire.close();
              return;
            }
            if (version > durable) durable = version;
          }
          if (resetting && frame.tag === 'Reset' && !resetSeen) {
            resetSeen = true;
            adapter.onmessage?.(event);
            adapter.onmessage?.({data: Frame.encode({tag: 'Batch', values: [version, [], [], []]})});
            return;
          }
        }
        adapter.onmessage?.(event);
      };
      return adapter;
    },
  };
}

export async function converge(origin, generated, {
  clients: count = 64, toggles = 1000, pace = 10, seed = 14059, mutant,
} = {}) {
  assert(Number.isInteger(count) && count >= 2);
  assert(Number.isInteger(toggles) && toggles >= 3);
  assert(Number.isFinite(pace) && pace > 0);
  const url = new URL('/hirsel', origin);
  url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:';
  const imported = file => import(pathToFileURL(resolve(generated, file)).href);
  const {Frame} = await imported('frame.js');
  const {want, identity, bind} = await imported('entries.js');
  const runtime = await loadRuntime(mutant && ((file, text) => mutate(mutant, file, text)));
  const state = await mkdtemp(join(tmpdir(), 'checkbox-converge-'));
  const failure = {};
  const live = [];
  const oracle = new Uint8Array(125000);
  let browser;
  async function client(id, options) {
    const board = new Board();
    const wire = transport(Frame, failure, options);
    const storage = new runtime.NodeStorage(join(state, String(id)));
    const sdk = await runtime.Client.open({
      url: url.href, frame: Frame, wants: want, identity, storage, credential: crypto.getRandomValues(new Uint8Array(16)), socket: wire.connect,
      onBatch: version => { board.version = version; wire.onBatch(); },
      onState: state => {
        if (['StorageUnavailable', 'Retired', 'Incompatible', 'Fenced'].includes(state)) {
          failure.error ??= new Error(`client ${id}: ${state}`);
        }
      },
    });
    live.push(sdk);
    const api = bind(sdk);
    api.subscribeDecoded.main.board(undefined, (_, change) => board.change(change));
    await waitFor(() => board.replacements > 0, failure);
    return {sdk, wire, board, toggle: api.call.main.toggle, storage, positions: new Set()};
  }

  async function toggle(agent, bit, p) {
    const outcome = await agent.toggle(bit);
    assert.equal(outcome.tag, 'Returned', outcome.value?.tag);
    assert.equal(outcome.value.tag, 'Ok');
    const {directory, streams} = await agent.storage.read();
    assert.equal(directory.length, 1);
    const record = streams[directory[0]];
    assert.equal(record.terminal.p, p);
    assert.equal(record.next, p + 1n);
    assert(!agent.positions.has(p), `duplicate final API outcome for p=${p}`);
    agent.positions.add(p);
    oracle[bit >> 3] ^= 1 << (bit & 7);
  }

  try {
    browser = await openBrowser(origin);
    const streams = await Promise.all(Array.from({length: count}, async (_, id) => {
      const random = seeded(seed + id);
      random();
      const faultAt = BigInt(1 + random() % (toggles - 2));
      const agent = await client(id, {faultAt, resetProbe: id === 1});
      return {...agent, random};
    }));
    const [first, probe] = streams;
    await toggle(first, first.random() % 1000000, 0n);
    await waitFor(() => probe.board.version >= first.wire.durable, failure);
    assert.deepEqual(probe.board.bits, oracle);
    probe.wire.requestReset();
    await waitFor(() => probe.wire.resetBatch, failure);
    assert(!probe.board.bits.some(Boolean), 'Reset must clear the rendered image before complete');
    probe.wire.releaseReplacement();
    await waitFor(() => probe.board.replacements === 2, failure);
    assert.deepEqual(probe.board.bits, oracle, 'fresh completion must restore the image');

    const started = performance.now();
    const work = Promise.all(streams.map(async (agent, id) => {
      let next = started;
      for (let i = id === 0 ? 1 : 0; i < toggles; i++) {
        if (next > performance.now()) await pause(next - performance.now());
        next = performance.now() + 1000 / pace;
        await toggle(agent, agent.random() % 1000000, BigInt(i));
      }
      assert.equal(agent.positions.size, toggles);
      assert(agent.wire.recovered, `client ${id} never retried its selected durable position`);
    }));
    let finished = false;
    void work.finally(() => { finished = true; }).catch(() => {});
    await Promise.race([work, waitFor(() => finished, failure)]);
    const reached = streams.reduce((version, agent) => agent.wire.durable > version ? agent.wire.durable : version, -1n);
    await waitFor(() => streams.every(agent => agent.board.version >= reached), failure);
    for (const [index, agent] of streams.entries()) {
      assert.deepEqual(agent.board.bits, oracle, `client ${index} diverged from the returned-call XOR fold`);
    }
    const fresh = await client('fresh', {});
    assert.deepEqual(fresh.board.bits, oracle, 'fresh server snapshot diverged from the XOR fold');
    await browser.check(canvasMatches, {bitmap: Buffer.from(oracle).toString('base64')});
    console.log(`PASS converge: ${count} clients, ${count * toggles} final outcomes, ${count} recovered sends, version ${reached}, seed ${seed}`);
  } finally {
    await Promise.allSettled(live.map(sdk => sdk.close()));
    await browser?.close();
    await runtime.dispose();
    await rm(state, {force: true, recursive: true});
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const [origin, generated, clients = '64', toggles = '1000', pace = '10', seed = '14059', mutant] = process.argv.slice(2);
  await converge(origin, generated, {clients: +clients, toggles: +toggles, pace: +pace, seed: +seed, mutant});
}
