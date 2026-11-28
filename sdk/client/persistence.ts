import type {Outcome, Want} from './client.js';

export interface Caller { readonly issuer: number; readonly subject: Uint8Array }
export interface HttpRequest { readonly path: string; readonly body?: string }
export interface StreamId { readonly t: bigint; readonly n: bigint }
export interface Pending {
  p: bigint; readonly want: Want; readonly args: Uint8Array; slot: number; readonly http?: HttpRequest;
  inflight: boolean; untouched: boolean; remintsLeft: 0 | 1;
}
export interface StreamRecord {
  readonly caller: Caller | null; next: bigint; pending?: Pending;
  terminal?: {readonly p: bigint; readonly outcome: Outcome<Uint8Array>}; retired: boolean;
  readonly id?: StreamId; minter: bigint | null; sent: boolean;
}
export const RECOVERY_VERSION = 4;
export interface Installation {
  readonly version: typeof RECOVERY_VERSION; readonly installation: bigint; readonly generation: bigint;
  directory: string[]; streams: Record<string, StreamRecord>;
}
export interface Lease { release(): Promise<void>; lost: Promise<void> }
// The client replaces stream records before editing them. Built-in storage can share
// those records between its committed snapshot and the client's working state.
export const replaceRecords = Symbol('replaceRecords');
// The installation shell, directory, stream records and pending records are mutable working
// state between read and write. Nested caller, want, args, http, stream id and terminal outcome
// values are immutable once stored.
// Hold the installation lease across a read/modify/write sequence.
export interface Storage {
  lock(name: string, wait: boolean): Promise<Lease | null>;
  read(records?: typeof replaceRecords): Promise<Installation | null>;
  write(state: Installation, records?: typeof replaceRecords): Promise<void>;
}

export function copyInstallation(state: Installation, records?: typeof replaceRecords): Installation {
  const streams = {...state.streams};
  if (records !== replaceRecords) {
    for (const name of Object.keys(streams)) {
      const record = streams[name];
      streams[name] = {...record};
      if (record.pending) streams[name].pending = {...record.pending};
    }
  }
  return {...state, directory: state.directory.slice(), streams};
}

export const fresh = (installation = randomGeneration()): Installation => ({
  version: RECOVERY_VERSION, installation, generation: randomGeneration(), directory: [], streams: {},
});
export const emptyStream = (caller: Caller | null = null): StreamRecord => ({caller, next: 0n, retired: false, minter: null, sent: false});
export const maxU64 = 0xffffffffffffffffn;
export const hex = (value: bigint, width: number) => value.toString(16).padStart(width, '0');
export const streamHex = (id: StreamId) => hex(id.t, 16) + hex(id.n, 16);
export const streamLock = (state: Installation, stream: string) => `stream-${state.generation}-${stream}`;
export const u64 = (n: unknown): n is bigint => typeof n === 'bigint' && n >= 0n && n <= maxU64;

export function validCaller(c: Caller): boolean {
  return !!c && Number.isInteger(c.issuer) && c.issuer >= 0 && c.issuer <= 255
    && c.subject instanceof Uint8Array && c.subject.length <= 64;
}
export function validHttpRequest(request: HttpRequest): boolean {
  return !!request && typeof request.path === 'string'
    && /^\/\.hirsel\/op\/[a-z0-9][a-z0-9_.-]{0,63}(\?[^#\s]*)?$/.test(request.path)
    && (request.body === undefined || typeof request.body === 'string');
}

export class CorruptState extends Error {}
export function randomGeneration(): bigint {
  const bytes = crypto.getRandomValues(new Uint32Array(4));
  return bytes.reduce((n, b) => (n << 32n) | BigInt(b), 0n);
}
export function intact(s: Installation): boolean {
  return s.version === RECOVERY_VERSION && typeof s.installation === 'bigint' &&
    typeof s.generation === 'bigint' && s.directory.every(n => {
    if (!Object.hasOwn(s.streams, n)) return false;
    const record = s.streams[n];
    return record.caller === null
      ? record.next === 0n && !record.pending && !record.terminal
      : validCaller(record.caller);
  });
}

export function encodeState(value: Installation | StatePatch): string {
  return JSON.stringify(value, (_, value) => typeof value === 'bigint' ? {$bigint: value.toString()}
    : value instanceof Uint8Array ? {$bytes: [...value]} : value);
}
// Parse JSON on the native fast path, then visit only the containers that can hold tagged values.
function reviveState(value: unknown): unknown {
  if (value === null || typeof value !== 'object') return value;
  if (Array.isArray(value)) {
    for (let i = 0; i < value.length; i++) {
      if (value[i] !== null && typeof value[i] === 'object') value[i] = reviveState(value[i]);
    }
    return value;
  }
  const object = value as Record<string, unknown>, keys = Object.keys(object);
  if (keys.length === 1) {
    if (keys[0] === '$bigint') {
      const text = object.$bigint;
      if (typeof text !== 'string' || !/^(0|[1-9][0-9]*)$/.test(text)) throw new CorruptState();
      return BigInt(text);
    }
    if (keys[0] === '$bytes') {
      const bytes = object.$bytes;
      if (!Array.isArray(bytes) || !bytes.every(byte)) throw new CorruptState();
      return Uint8Array.from(bytes);
    }
  }
  for (const key of keys) {
    const child = object[key];
    if (child !== null && typeof child === 'object') object[key] = reviveState(child);
  }
  return object;
}
function parseRecoveryValue(text: string): unknown {
  try {
    return reviveState(JSON.parse(text));
  } catch (error) { if (error instanceof CorruptState) throw error; throw new CorruptState('unreadable client state'); }
}

export function decodeState(text: string): Installation {
  const state = parseRecoveryValue(text) as Installation;
  if (!state || state.version !== RECOVERY_VERSION || !token(state.generation) || !token(state.installation)
    || !validDirectory(state.directory) || !recordMap(state.streams)) throw new CorruptState();
  // Missing directory members are retained for generation fencing in Client.open().
  const names = new Set(state.directory);
  for (const [name, record] of Object.entries(state.streams)) {
    if (!names.has(name) || !validRecord(record, name)) throw new CorruptState();
  }
  return state;
}

interface StatePatch {
  directory?: string[];
  streams: Record<string, StreamRecord | null>;
}

function sameFields<T extends object>(before: T, after: T, nested?: keyof T): boolean {
  if (before === after) return true;
  const keys = Object.keys(before) as Array<keyof T>;
  return keys.length === Object.keys(after).length
    && keys.every(key => Object.hasOwn(after, key) && (key === nested || before[key] === after[key]));
}

export function encodePatch(before: Installation, after: Installation): string {
  const patch: StatePatch = {streams: {}};
  if (before.directory.length !== after.directory.length ||
      before.directory.some((name, index) => name !== after.directory[index])) patch.directory = after.directory;
  for (const name of Object.keys(before.streams)) {
    if (!Object.hasOwn(after.streams, name)) patch.streams[name] = null;
  }
  for (const [name, record] of Object.entries(after.streams)) {
    const old = before.streams[name];
    if (!old || !sameFields(old, record, 'pending') ||
        (old.pending && record.pending ? !sameFields(old.pending, record.pending) : old.pending !== record.pending)) {
      patch.streams[name] = record;
    }
  }
  return encodeState(patch);
}

export function applyPatch(state: Installation, text: string): void {
  const patch = parseRecoveryValue(text) as StatePatch;
  if (!patch || !recordMap(patch.streams)) throw new CorruptState();
  if (patch.directory !== undefined) {
    if (!validDirectory(patch.directory)) throw new CorruptState();
    state.directory = patch.directory;
  }
  const names = new Set(state.directory);
  for (const [name, record] of Object.entries(patch.streams)) {
    if (!streamKey(name)) throw new CorruptState();
    if (record === null) delete state.streams[name];
    else {
      if (!names.has(name) || !validRecord(record, name)) throw new CorruptState();
      state.streams[name] = record;
    }
  }
  if (patch.directory && Object.keys(state.streams).some(name => !names.has(name))) throw new CorruptState();
}

const recordMap = (value: unknown) => !!value && typeof value === 'object' && !Array.isArray(value);
const validDirectory = (value: unknown): value is string[] =>
  Array.isArray(value) && value.every(streamKey) && new Set(value).size === value.length;

const byte = (n: unknown) => Number.isInteger(n) && (n as number) >= 0 && (n as number) <= 255;
const u32 = (n: unknown) => Number.isInteger(n) && (n as number) >= 0 && (n as number) <= 0xffffffff;
const token = (n: unknown) => typeof n === 'bigint' && n >= 0n && n < 1n << 128n;
const streamKey = (name: unknown) => typeof name === 'string' && /^(?:pending-)?[a-f0-9]{32}$/.test(name);
function want(w: Want): boolean {
  return !!w && w.identity instanceof Uint8Array && u64(w.args_fp) && u64(w.result_fp);
}
function pending(p: Pending, r: StreamRecord): boolean {
  return p.p === r.next && want(p.want)
    && p.args instanceof Uint8Array && u32(p.slot) && r.caller !== null
    && typeof p.inflight === 'boolean' && (!p.inflight || r.sent)
    && typeof p.untouched === 'boolean' && (p.remintsLeft === 0 || p.remintsLeft === 1)
    && (p.http === undefined || validHttpRequest(p.http));
}
function validRecord(r: StreamRecord, name: string): boolean {
  if (!r || (r.caller !== null && !validCaller(r.caller)) || !u64(r.next) ||
      typeof r.retired !== 'boolean' || typeof r.sent !== 'boolean' ||
      (r.minter !== null && !token(r.minter))) return false;
  if (r.id === undefined) {
    return name.startsWith('pending-') && r.next === 0n && !r.pending && !r.terminal && !r.sent && r.minter === null;
  }
  if (!r.id || !u64(r.id.t) || !u64(r.id.n) || streamHex(r.id) !== name || r.caller === null ||
      (!r.sent && r.minter === null) || (r.pending && !pending(r.pending, r))) return false;
  if (r.terminal && (!u64(r.terminal.p) || !r.terminal.outcome || !['Returned', 'Failed', 'Refused'].includes(r.terminal.outcome.tag))) return false;
  return true;
}

export const decimalU64 = (value: string | null): bigint | undefined => {
  if (!value || !/^(0|[1-9][0-9]{0,19})$/.test(value)) return undefined;
  const parsed = BigInt(value);
  return u64(parsed) ? parsed : undefined;
};

// Leases `stream` and moves `record` there in memory; the caller writes the installation once.
export async function claimStream(storage: Storage, state: Installation, previous: string | undefined,
  stream: string, record: StreamRecord, keepOld: boolean): Promise<Lease> {
  if (Object.hasOwn(state.streams, stream)) throw new Error('stream id collision');
  const lease = await storage.lock(streamLock(state, stream), false);
  if (!lease) throw new Error('stream lease unavailable');
  if (keepOld || previous === undefined) state.directory.push(stream);
  else {
    state.directory[state.directory.indexOf(previous)] = stream;
    delete state.streams[previous];
  }
  state.streams[stream] = record;
  return lease;
}
export async function replaceStream(storage: Storage, state: Installation, previous: string,
  stream: string, record: StreamRecord, keepOld: boolean): Promise<Lease> {
  const lease = await claimStream(storage, state, previous, stream, record, keepOld);
  try { await storage.write(state, replaceRecords); }
  catch (error) {
    await lease.release();
    throw error;
  }
  return lease;
}

export const sameCaller = (a: Caller | null, b: Caller | null) =>
  a === null || b === null ? a === b : a.issuer === b.issuer && a.subject.length === b.subject.length
    && a.subject.every((byte, i) => byte === b.subject[i]);
// Leases the first free stream, and every other free stream of its Caller that still holds a
// call, so each recovered call resumes on its own lane; else a new empty stream. Empty when
// none can be leased.
export async function leaseStreams(storage: Storage, state: Installation): Promise<Array<[string, Lease]>> {
  const leased: Array<[string, Lease]> = [];
  try {
    let caller: Caller | null = null;
    for (const n of state.directory) {
      const record = state.streams[n];
      if (record.retired || (leased.length > 0 && !(record.pending && sameCaller(record.caller, caller)))) continue;
      const lease = await storage.lock(streamLock(state, n), false);
      if (!lease) continue;
      if (leased.length === 0) caller = record.caller;
      leased.push([n, lease]);
    }
    if (leased.length === 0) {
      let stream: string;
      do { stream = 'pending-' + hex(randomGeneration(), 32); } while (Object.hasOwn(state.streams, stream));
      const lease = await storage.lock(streamLock(state, stream), false);
      if (!lease) return leased;
      leased.push([stream, lease]);
      state.directory.push(stream);
      state.streams[stream] = emptyStream();
      await storage.write(state, replaceRecords);
    }
    return leased;
  } catch (error) {
    for (const [, lease] of leased) await lease.release();
    throw error;
  }
}
// Rewrites a stream this process has just leased. A granted body runs once per delivery and
// nothing is stored for it, so an attempt that may have been delivered is reported LocallyUnknown
// (`onLost`) and never re-sent; any other such attempt is resent
// under its key but is no longer untouched. A stream minted by another installation is imported.
export function recover(state: Installation, stream: string, granted: boolean, onLost: () => void): boolean {
  const before = state.streams[stream], pending = before.pending;
  const imported = before.id !== undefined && before.minter !== state.installation;
  if (!pending?.inflight && !imported) return false;
  const record = {...before};
  if (pending?.inflight && granted) {
    delete record.pending;
    onLost();
  } else if (pending) record.pending = {...pending, inflight: false, untouched: false};
  if (imported) {
    record.minter = null;
    record.sent = true;
  }
  state.streams[stream] = record;
  return true;
}
