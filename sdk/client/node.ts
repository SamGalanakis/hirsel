import {hash} from 'node:crypto';
import {constants, type BigIntStats} from 'node:fs';
import {open, mkdir, readlink, rename, stat, symlink, unlink} from 'node:fs/promises';
import {basename, join, resolve} from 'node:path';
import {setTimeout as delay} from 'node:timers/promises';
import {
  applyPatch, copyInstallation, CorruptState, decodeState, encodePatch, encodeState,
  replaceRecords, type Installation, type Lease, type Storage,
} from './persistence.js';

const magic = 0x48535731; // HSW1
const headerBytes = 16;
const compactAfterRecords = 128;
const compactAfterBytes = 1024 * 1024;

interface Journal {
  readonly state: Installation;
  readonly stamp: BigIntStats;
  readonly end: number;
  readonly records: number;
  readonly deltaBytes: number;
  readonly published: boolean;
}

export class NodeStorage implements Storage {
  private readonly directory: string;
  private readonly walPath: string;
  private leaseGenerations = new Map<string, number>();
  private cached?: Journal;

  constructor(directory: string) {
    this.directory = resolve(directory);
    this.walPath = join(this.directory, 'state.wal');
  }

  async lock(name: string, wait: boolean): Promise<Lease | null> {
    const base = join(this.directory, name === 'install' ? 'install.lease' : name + '.stream.lease');
    const owner = join(this.directory, `.owner-${process.pid}-${crypto.randomUUID()}`);
    let generation = this.leaseGenerations.get(base) ?? 0;
    for (;;) {
      const path = generation === 0 ? base : `${base}.${generation}`;
      // symlink publishes the owner identity and claims the path in one exclusive operation.
      try {
        await symlink(owner, path).catch(async error => {
          if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error;
          await mkdir(this.directory, {recursive: true, mode: 0o700});
          await symlink(owner, path);
        });
      } catch (error) {
        if ((error as NodeJS.ErrnoException).code !== 'EEXIST') throw error;
        const other = await readOwner(path);
        if (other === null) continue;
        if (ownerExited(other)) {
          if (await readOwner(path) !== other) continue;
          // Leave the dead claim in place. All contenders advance to the same
          // successor without racing to unlink a new owner's claim.
          this.leaseGenerations.set(base, ++generation);
          continue;
        }
        if (!wait) return null;
        await delay(4);
        continue;
      }
      let end!: () => void;
      const lost = new Promise<void>(resolve => { end = resolve; });
      let release: Promise<void> | undefined;
      return {lost, release: () => release ??= unlink(path).then(end)};
    }
  }

  async read(records?: typeof replaceRecords): Promise<Installation | null> {
    const journal = await this.load();
    return journal && copyInstallation(journal.state, records);
  }

  private async load(): Promise<Journal | null> {
    const stamp = await stat(this.walPath, {bigint: true}).catch(missing);
    const cached = this.cached;
    if (stamp && cached && sameFile(stamp, cached.stamp)) return cached;
    this.cached = undefined;
    if (!stamp) return null;
    const file = await open(this.walPath, 'r');
    try {
      const opened = await file.stat({bigint: true});
      const journal = replay(await file.readFile(), opened);
      if (BigInt(journal.end) === opened.size) this.cached = journal;
      return journal;
    } finally { await file.close(); }
  }

  // Writers hold the installation lease across a read/modify/write sequence.
  async write(state: Installation, records?: typeof replaceRecords): Promise<void> {
    const previous = await this.load().catch(error => {
      if (error instanceof CorruptState) return null;
      throw error;
    });
    if (!previous || previous.state.generation !== state.generation ||
        previous.state.installation !== state.installation || previous.state.version !== state.version) {
      await this.checkpoint(state, records);
      return;
    }
    const frame = encodeFrame(encodePatch(previous.state, state));
    const journal = previous.records >= compactAfterRecords || previous.deltaBytes >= compactAfterBytes
      ? await this.checkpoint(previous.state, replaceRecords) : previous;
    // A process can die after renaming a checkpoint and before syncing its directory.
    // Publish that name before appending the first new mutation to a checkpoint we recovered.
    if (!journal.published) await this.syncDirectory();
    const file = await open(this.walPath, constants.O_WRONLY | constants.O_APPEND);
    try {
      if (BigInt(journal.end) < journal.stamp.size) await file.truncate(journal.end);
      await file.writeFile(frame);
      await file.sync();
      this.cached = {
        state: copyInstallation(state, records), stamp: await file.stat({bigint: true}),
        end: journal.end + frame.length, records: journal.records + 1,
        deltaBytes: journal.deltaBytes + frame.length, published: true,
      };
    } finally { await file.close(); }
  }

  private async checkpoint(state: Installation, records?: typeof replaceRecords): Promise<Journal> {
    const frame = encodeFrame(encodeState(state));
    const temporary = join(this.directory, `.state-${process.pid}-${crypto.randomUUID()}`);
    const file = await open(temporary, 'wx', 0o600).catch(async error => {
      if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error;
      await mkdir(this.directory, {recursive: true, mode: 0o700});
      return open(temporary, 'wx', 0o600);
    });
    try {
      await file.writeFile(frame);
      await file.sync();
      // The checkpoint contains every prior committed frame. Either inode is sufficient
      // for recovery until the rename is durable.
      await rename(temporary, this.walPath);
      await this.syncDirectory();
      return this.cached = {
        state: copyInstallation(state, records), stamp: await file.stat({bigint: true}),
        end: frame.length, records: 0, deltaBytes: 0, published: true,
      };
    } catch (error) {
      await unlink(temporary).catch(() => {});
      throw error;
    } finally {
      await file.close();
    }
  }

  private async syncDirectory(): Promise<void> {
    const directory = await open(this.directory, 'r');
    try { await directory.sync(); } finally { await directory.close(); }
  }
}

function missing(error: NodeJS.ErrnoException): null {
  if (error.code === 'ENOENT') return null;
  throw error;
}

function sameFile(a: BigIntStats, b: BigIntStats): boolean {
  return a.dev === b.dev && a.ino === b.ino && a.size === b.size && a.ctimeNs === b.ctimeNs && a.mtimeNs === b.mtimeNs;
}

function checksum(payload: Buffer): number {
  return hash('sha256', payload, 'buffer').readUInt32BE();
}

function encodeFrame(text: string): Buffer {
  const length = Buffer.byteLength(text);
  const frame = Buffer.allocUnsafe(headerBytes + length);
  frame.write(text, headerBytes, length);
  frame.writeUInt32BE(magic, 0);
  frame.writeUInt32BE(length, 4);
  frame.writeUInt32BE(~length >>> 0, 8);
  frame.writeUInt32BE(checksum(frame.subarray(headerBytes)), 12);
  return frame;
}

function replay(bytes: Buffer, stamp: BigIntStats): Journal {
  let end = 0, checkpointBytes = 0, records = 0;
  let state: Installation | undefined;
  while (bytes.length - end >= headerBytes) {
    const length = bytes.readUInt32BE(end + 4);
    if (bytes.readUInt32BE(end) !== magic || bytes.readUInt32BE(end + 8) !== (~length >>> 0) || length === 0) {
      throw new CorruptState('invalid client WAL header');
    }
    const next = end + headerBytes + length;
    if (next > bytes.length) break;
    const payload = bytes.subarray(end + headerBytes, next);
    if (bytes.readUInt32BE(end + 12) !== checksum(payload)) throw new CorruptState('invalid client WAL checksum');
    const text = payload.toString('utf8');
    if (state) {
      applyPatch(state, text);
      records++;
    } else {
      state = decodeState(text);
      checkpointBytes = next;
    }
    end = next;
  }
  if (!state) throw new CorruptState('missing client WAL checkpoint');
  return {state, stamp, end, records, deltaBytes: end - checkpointBytes, published: records > 0};
}

async function readOwner(path: string): Promise<string | null> {
  try { return await readlink(path); }
  catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ENOENT') return null;
    throw error;
  }
}

function ownerExited(owner: string): boolean {
  const match = /^\.owner-([1-9][0-9]*)-[a-f0-9-]{36}$/.exec(basename(owner));
  const pid = Number(match?.[1]);
  if (!Number.isInteger(pid) || pid > 0x7fffffff) throw new Error('Invalid client lease owner');
  try { process.kill(pid, 0); return false; }
  catch (error) {
    const {code} = error as NodeJS.ErrnoException;
    if (code === 'ESRCH') return true;
    if (code === 'EPERM') return false;
    throw error;
  }
}
