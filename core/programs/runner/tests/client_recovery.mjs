import assert from 'node:assert/strict';
import {execFile, spawn} from 'node:child_process';
import {mkdir} from 'node:fs/promises';
import {join} from 'node:path';
import {createInterface} from 'node:readline';
import {pathToFileURL} from 'node:url';
import {promisify} from 'node:util';

const exec = promisify(execFile);
const returned = value => ({tag: 'Returned', value: {tag: 'Ok', value}});
const answered = value => ({tag: 'Returned', value});
const historyUnavailable = {tag: 'Refused', value: {tag: 'HistoryUnavailable'}};
const unknown = {tag: 'LocallyUnknown'};
const within = (answer, what) => Promise.race([answer, new Promise((_, fail) => {
  setTimeout(() => fail(new Error(`${what}: no answer in 15 s`)), 15000).unref();
})]);
async function committedKey(fault) {
  const response = await fault.committed;
  assert.equal(response.status, 201);
  assert.equal(response.committed, 'committed');
  return response.key;
}

function assertMarkedRetry(fault, key) {
  assert.deepEqual(fault.posts, [key, key]);
  assert.deepEqual(fault.responses[1], {status: 410, marker: `history_unavailable;key=${key}`});
}

const word = value => value.toString(16).padStart(16, '0');
const bump = value => ({path: '/.hirsel/op/bump', body: JSON.stringify({value})});

async function startPlace(executable, mode, directory) {
  const process = spawn(executable, [mode, directory], {stdio: ['pipe', 'pipe', 'pipe']});
  let error = '';
  process.stderr.setEncoding('utf8').on('data', chunk => { error += chunk; });
  const exited = new Promise((resolve, reject) => {
    process.once('error', reject);
    process.once('close', (code, signal) => {
      if (code === 0) resolve();
      else reject(new Error(`Place ended (${code ?? signal}): ${error}`));
    });
  });
  void exited.catch(() => {});
  const output = createInterface({input: process.stdout})[Symbol.asyncIterator]();
  const port = async () => {
    const line = await output.next();
    if (line.done) await exited;
    assert.match(line.value ?? '', /^[1-9][0-9]*$/);
    return line.value;
  };
  const stop = async () => { process.stdin.end(); await exited; };
  try {
    return {ws: `ws://127.0.0.1:${await port()}/hirsel`, http: `http://127.0.0.1:${await port()}`, stop};
  } catch (error) {
    await stop().catch(() => {});
    throw error;
  }
}

function observedSocket(frame, sends, receives) {
  return (url, protocol) => {
    const ws = new WebSocket(url, protocol);
    ws.binaryType = 'arraybuffer';
    const socket = {
      send(data) {
        sends?.(frame.decode(new Uint8Array(data)));
        ws.send(data);
      },
      close: (code, reason) => ws.close(code, reason),
    };
    ws.onopen = event => socket.onopen?.(event);
    ws.onclose = event => socket.onclose?.(event);
    ws.onerror = event => socket.onerror?.(event);
    ws.onmessage = event => {
      const deliver = () => socket.onmessage?.(event);
      if (receives) receives(frame.decode(new Uint8Array(event.data)), deliver);
      else deliver();
    };
    return socket;
  };
}

function outcomes(frame) {
  if (frame.tag === 'Batch') return frame.values[3];
  if (frame.tag === 'Reset') return frame.values[1];
  return [];
}

function faultAfterCommit(mode) {
  const committed = Promise.withResolvers();
  const release = Promise.withResolvers();
  const posts = [], responses = [];
  return {
    committed: committed.promise,
    release: release.resolve,
    posts,
    responses,
    async fetch(url, request) {
      const key = request.headers.get('Idempotency-Key');
      posts.push(key);
      const response = await fetch(url, request);
      responses.push({status: response.status, marker: response.headers.get('Hirsel-Refused')});
      if (posts.length === 1) {
        await response.text();
        committed.resolve({key, status: response.status, committed: response.headers.get('Idempotent-Outcome')});
        await release.promise;
        if (mode === 'lost') throw new TypeError('upstream closed after accepting the POST');
        return new Response(null, {status: 503});
      }
      return response;
    },
  };
}

export async function realRecoveryLaws(runtime, compiler, executable, sdk) {
  const {Frame} = await import(pathToFileURL(join(sdk, 'frame.js')));
  const {bind, want, kind, identity} = await import(pathToFileURL(join(sdk, 'entries.js')));
  const tests = {
    // Only a stored (transact) call consumes its position: the compute and granted calls
    // after the first bump go at p1 and leave it for the second bump (STORE-SPEC §1.8 P1).
    async positions_move_only_on_stored_calls(h) {
      const calls = [];
      const a = await h.client('mixed', {
        socket: observedSocket(Frame, frame => { if (frame.tag === 'Call') calls.push([frame.values[1], frame.values[2]]); }),
      });
      assert.deepEqual(await a.bump(21), returned(21));
      assert.deepEqual(await within(a.double(5), 'compute'), answered(10));
      assert.deepEqual(await within(a.echo(7), 'granted'), answered(7));
      assert.deepEqual(await within(a.bump(22), 'second stored call'), returned(22));
      assert.deepEqual(calls, [[0n, 0], [1n, 1], [1n, 2], [1n, 0]]);
      await h.checkEffects([1, 1, 0]);
    },
    // A granted body may have run once its Call was delivered, and nothing is stored to
    // dedup a re-send, so a lost attempt ends LocallyUnknown and is never sent again.
    async granted_lost_attempt_is_not_resent(h) {
      const calls = [];
      let dropped = false;
      const back = Promise.withResolvers();
      const a = await h.client('granted', {
        onState: state => { if (dropped && state === 'Ready') back.resolve(); },
        socket: (url, protocol) => {
          const socket = observedSocket(Frame, frame => {
            if (frame.tag === 'Call') calls.push([frame.values[1], frame.values[2]]);
          }, (frame, deliver) => {
            if (!dropped && outcomes(frame).some(row => row.p === 1n)) { dropped = true; socket.close(); }
            else deliver();
          })(url, protocol);
          return socket;
        },
      });
      assert.deepEqual(await a.bump(21), returned(21));
      assert.deepEqual(await within(a.echo(7), 'granted'), unknown);
      assert(dropped);
      await within(back.promise, 'reconnect');
      assert.deepEqual(await within(a.bump(22), 'stored call after the loss'), returned(22));
      assert.deepEqual(calls, [[0n, 0], [1n, 2], [1n, 0]]);
      await h.checkEffects([1, 1, 0]);
    },
    async fresh_installations_share_caller(h) {
      const a = await h.client('first');
      const b = await h.client('second');
      assert.deepEqual(await a.bump(21), returned(21));
      assert.deepEqual(await b.bump(22), returned(22));
      const first = await a.storage.read();
      const second = await b.storage.read();
      assert.notEqual(first.installation, second.installation);
      assert.notEqual(first.directory[0], second.directory[0]);
      await h.checkEffects([1, 1, 0]);
    },
    async concurrent_same_caller_bridge(h) {
      const captured = Promise.withResolvers();
      const browser = await h.client('browser', {
        socket: observedSocket(Frame, undefined, (frame, deliver) => {
          if (outcomes(frame).length) captured.resolve(deliver);
          else deliver();
        }),
      });
      const sdkAnswer = browser.bump(21);
      const deliver = await Promise.race([
        captured.promise,
        sdkAnswer.then(result => assert.fail(`SDK settled before wire result: ${JSON.stringify(result)}`)),
      ]);
      try {
        assert.deepEqual(await h.cli(22), returned(22));
      } finally {
        deliver();
      }
      assert.deepEqual(await sdkAnswer, returned(21));
      await h.checkEffects([1, 1, 0]);
    },
    async older_remint_refused_once(h) {
      const calls = [], answers = [];
      const a = await h.client('old', {
        socket: observedSocket(Frame, frame => {
          if (frame.tag === 'Call') calls.push(frame.values.slice(0, 2));
        }, (frame, deliver) => {
          answers.push(...outcomes(frame).map(row => row.outcome.tag === 'Returned' ? 'Returned' : row.outcome));
          deliver();
        }),
      });
      assert.deepEqual(await a.bump(21), returned(21));
      const [[original, position]] = calls;
      assert.equal(position, 0n);
      // A later retained stream is legal within the Place's five-minute future
      // allowance. Both Welcome samples for the SDK are behind its birth.
      const later = BigInt(Date.now()) + 60000n;
      assert.deepEqual(await h.cli(22, word(later) + word(0n)), returned(22));
      assert.deepEqual(await a.bump(23), historyUnavailable);
      assert.equal(a.client.state, 'Ready', 'a retired stream ends only its own call');
      assert.equal(calls.length, 3);
      assert.deepEqual(calls[1], [original, 1n]);
      const [replacement, remintedPosition] = calls[2];
      assert.notDeepEqual(replacement, original);
      assert(replacement.t < later);
      assert.equal(remintedPosition, 0n);
      assert.deepEqual(answers, ['Returned', historyUnavailable, historyUnavailable]);
      await h.checkEffects([1, 1, 0]);
    },
    async marked_410_after_invisible_delivery_through_reopen(h) {
      const fault = faultAfterCommit('lost');
      const a = await h.client('http', {fetch: fault.fetch});
      const answer = a.client.callHttp(want[0], bump(21));
      const key = await committedKey(fault);
      await a.client.close();
      fault.release();
      assert.deepEqual(await answer, unknown);
      await h.checkEffects([1, 0, 0]);
      await h.start();
      await h.evict(key);
      const replayed = Promise.withResolvers();
      const reopened = await h.client('http', {fetch: fault.fetch, onRecovered: replayed.resolve, ready: false});
      assert.deepEqual(await replayed.promise, unknown);
      assert.equal(reopened.client.state, 'Ready');
      assertMarkedRetry(fault, key);
      const fresh = await h.client('http', {fetch: fault.fetch});
      assert.deepEqual(await fresh.client.callHttp(want[0], bump(23)), {
        tag: 'Returned', value: {status: 201, body: '23', committed: true, replayed: false},
      });
      assert.equal(fault.posts.length, 3);
      assert.notEqual(fault.posts[2], key);
      await h.checkEffects([1, 1, 1]);
    },
    async unmarked_503_after_commit(h) {
      const fault = faultAfterCommit('503');
      const retry = Promise.withResolvers();
      const disconnected = Promise.withResolvers();
      // Hold the retry timer across the Place restart. Explicit reconnect
      // will replay the pending POST on the new transport.
      const clock = {
        now: Date.now,
        random: () => 0.5,
        set(fn, ms) {
          if (ms < 1000) { retry.resolve(); return undefined; }
          return setTimeout(fn, ms);
        },
        clear: clearTimeout,
      };
      const a = await h.client('http', {
        fetch: fault.fetch, clock, followPlace: true,
        onState: state => { if (state === 'Backoff') disconnected.resolve(); },
      });
      const answer = a.client.callHttp(want[0], bump(21));
      const key = await committedKey(fault);
      fault.release();
      await Promise.race([
        retry.promise,
        answer.then(result => assert.fail(`unmarked 503 settled the call: ${JSON.stringify(result)}`)),
      ]);
      await h.checkEffects([1, 0, 0]);
      await disconnected.promise;
      await h.start();
      await h.evict(key);
      a.client.reconnect();
      assert.deepEqual(await answer, unknown);
      assert.equal(a.client.state, 'Ready');
      assertMarkedRetry(fault, key);
      await h.checkEffects([1, 1, 0]);
    },
  };

  for (const [name, test] of Object.entries(tests)) {
    const directory = join(name, 'disk');
    await mkdir(directory, {recursive: true});
    const quota = name !== 'fresh_installations_share_caller' && name !== 'concurrent_same_caller_bridge';
    const clients = [];
    let place;
    const stop = async () => {
      if (!place) return;
      const current = place;
      place = undefined;
      await current.stop();
    };
    const h = {
      start: async () => { place = await startPlace(executable, quota ? 'quota-one' : 'serve', directory); },
      stop,
      client: async (label, options = {}) => {
        const {fetch, ready = true, followPlace = false, ...callbacks} = options;
        const transport = followPlace ? {
          socket: (_url, protocol) => new WebSocket(place.ws, protocol),
          http: {url: place.http, fetch: (url, request) => fetch(new URL(url.pathname, place.http), request)},
        } : {http: {url: place.http, fetch}};
        const storage = new runtime.NodeStorage(join(name, label));
        const client = await runtime.Client.open({
          url: place.ws, frame: Frame, wants: want, kinds: kind, identity, storage, ...callbacks,
          ...transport,
        });
        clients.push(client);
        if (ready) await client.whenReady();
        const {call} = bind(client);
        return {
          client, storage, bump: value => call.runner.recovery_bump({value}),
          double: value => call.runner.recovery_double({value}), echo: value => call.runner.recovery_echo({value}),
        };
      },
      cli: async (value, stream) => {
        const {stdout} = await exec(compiler, [
          'call', executable, '--url', place.ws, '--n-place', '1048576',
          ...(stream ? ['--stream', stream] : []), '--', 'runner/recovery_bump', '--value', String(value),
        ]);
        return JSON.parse(stdout);
      },
      evict: async key => {
        assert.match(key, /^"[a-f0-9]{32}\.0"$/);
        const birth = BigInt('0x' + key.slice(1, 17));
        const next = BigInt(Date.now());
        assert(next > birth, 'the evicting call has a later birth');
        assert.deepEqual(await h.cli(22, word(next) + word(0n)), returned(22));
      },
      checkEffects: async expected => {
        await stop();
        const {stdout} = await exec(executable, ['inspect', directory]);
        assert.deepEqual(stdout.trim().split('\n').map(Number), expected);
      },
    };
    try {
      await h.start();
      await test(h);
      console.error(`client ${name}: PASS`);
    } finally {
      try { for (const client of clients) await client.close(); }
      finally { await stop(); }
    }
  }
}
