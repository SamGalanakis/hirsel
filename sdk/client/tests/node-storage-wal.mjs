import assert from 'node:assert/strict';
import {execFile} from 'node:child_process';
import {appendFile, mkdtemp, readFile, readdir, rm, stat} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {test} from 'node:test';
import {promisify} from 'node:util';
import {NodeStorage} from '../dist/node.js';

const wal = 'state.wal';
const checkpointEvery = 128;
const queues = [1n, 2n].map(n => n.toString(16).padStart(32, '0'));
const nodeModule = new URL('../dist/node.js', import.meta.url).href;
const run = promisify(execFile);

function outbox(position) {
  return {
    version: 4, installation: 2n, generation: 1n, directory: queues,
    streams: Object.fromEntries(queues.map((key, slot) => [key, {
      caller: {issuer: 1, subject: Uint8Array.of(7)}, id: {t: 0n, n: BigInt(slot + 1)},
      minter: 2n, next: position, retired: false, sent: true,
      pending: {
        p: position, want: {identity: Uint8Array.of(1), args_fp: 1n, result_fp: 2n},
        args: Uint8Array.of(slot, Number(position)), slot, inflight: true, untouched: true, remintsLeft: 1,
      },
    }])),
  };
}

async function directory(t) {
  const path = await mkdtemp(join(tmpdir(), 'hirsel-node-wal-'));
  t.after(() => rm(path, {recursive: true, force: true}));
  return path;
}

async function write(storage, state) {
  const lease = await storage.lock('install', true);
  try { await storage.write(state); }
  finally { await lease.release(); }
}

async function child(dir, body) {
  return run(process.execPath, ['--input-type=module', '--eval', `
    import fs from 'node:fs/promises';
    import {syncBuiltinESMExports} from 'node:module';
    import {NodeStorage} from ${JSON.stringify(nodeModule)};
    const dir = process.argv[1];
    const storage = new NodeStorage(dir);
    ${body}
  `, dir]);
}

test('recovers the committed outbox from WAL frames and a compacted checkpoint', async t => {
  const dir = await directory(t);
  const storage = new NodeStorage(dir);
  await write(storage, outbox(0n));
  const original = await stat(join(dir, wal));
  for (let p = 1; p <= checkpointEvery; p++) await write(storage, outbox(BigInt(p)));
  assert.equal((await stat(join(dir, wal))).ino, original.ino);
  assert.deepEqual(await new NodeStorage(dir).read(), outbox(BigInt(checkpointEvery)));

  const next = outbox(BigInt(checkpointEvery));
  next.directory = [queues[1]];
  delete next.streams[queues[0]];
  await write(storage, next);
  const compacted = await stat(join(dir, wal));
  assert.notEqual(compacted.ino, original.ino);
  assert(compacted.size < original.size * checkpointEvery);
  assert.deepEqual(await new NodeStorage(dir).read(), next);
});

test('ignores a partial tail frame and can commit again after restart', async t => {
  const dir = await directory(t);
  const source = await directory(t);
  const storage = new NodeStorage(dir);
  const sample = new NodeStorage(source);
  await write(sample, outbox(0n));
  await write(sample, outbox(1n));
  const prefix = (await stat(join(source, wal))).size;
  await write(sample, outbox(2n));
  const frame = (await readFile(join(source, wal))).subarray(prefix);
  await write(storage, outbox(0n));
  await write(storage, outbox(1n));
  for (const [bytes, prior] of [[1, 1n], [frame.length - 1, 3n]]) {
    await appendFile(join(dir, wal), frame.subarray(0, bytes));
    const reopened = new NodeStorage(dir);
    assert.deepEqual(await reopened.read(), outbox(prior));
    await write(reopened, outbox(prior + 2n));
    assert.deepEqual(await new NodeStorage(dir).read(), outbox(prior + 2n));
  }
});

test('recovers the last acknowledged batch on either side of checkpoint rename', async t => {
  for (const renamed of [false, true]) {
    const dir = await directory(t);
    const storage = new NodeStorage(dir);
    const last = BigInt(checkpointEvery);
    for (let p = 0n; p <= last; p++) await write(storage, outbox(p));
    const original = await stat(join(dir, wal));
    await assert.rejects(child(dir, `
      await storage.lock('install', true);
      const state = await storage.read();
      const rename = fs.rename;
      fs.rename = async (...paths) => {
        if (${renamed}) await rename(...paths);
        process.exit(77);
      };
      syncBuiltinESMExports();
      await storage.write(state);
    `), {code: 77});
    assert.equal((await stat(join(dir, wal))).ino === original.ino, !renamed);
    assert.equal((await readdir(dir)).some(name => name.startsWith('.state-')), !renamed);
    const reopened = new NodeStorage(dir);
    assert.deepEqual(await reopened.read(), outbox(last));
    if (renamed) {
      const checkpoint = await stat(join(dir, wal));
      await assert.rejects(child(dir, `
        await storage.lock('install', true);
        const state = await storage.read();
        const open = fs.open;
        fs.open = async (path, ...args) => {
          const handle = await open(path, ...args);
          if (path === dir) handle.sync = async () => { throw new Error('directory sync failed'); };
          return handle;
        };
        syncBuiltinESMExports();
        await storage.write(state);
      `), /directory sync failed/);
      assert.equal((await stat(join(dir, wal))).size, checkpoint.size);
    }
    await write(reopened, outbox(last + 1n));
    assert.deepEqual(await new NodeStorage(dir).read(), outbox(last + 1n));
  }
});
