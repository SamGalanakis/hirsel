import type {Outcome, Want} from './client.js';

export interface Caller { issuer: number; subject: Uint8Array }
export interface HttpRequest { path: string; body?: string }
export interface StreamId { t: bigint; n: bigint }
export interface Pending {
  p: bigint; want: Want; args: Uint8Array; slot: number; http?: HttpRequest;
  inflight: boolean; untouched: boolean; remintsLeft: 0 | 1;
}
export interface StreamRecord {
  caller: Caller | null; next: bigint; pending?: Pending; terminal?: {p: bigint; outcome: Outcome<Uint8Array>}; retired: boolean;
  id?: StreamId; minter: bigint | null; sent: boolean;
}
export const RECOVERY_VERSION = 4;
export interface Installation {
  version: typeof RECOVERY_VERSION; installation: bigint; generation: bigint;
  directory: string[]; streams: Record<string, StreamRecord>;
}
export interface Lease { release(): Promise<void>; lost: Promise<void> }
export interface Storage {
  lock(name: string, wait: boolean): Promise<Lease | null>;
  read(): Promise<Installation | null>;
  write(state: Installation): Promise<void>;
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

export function encodeState(state: Installation): string {
  return JSON.stringify(state, (_, value) => typeof value === 'bigint' ? {$bigint: value.toString()}
    : value instanceof Uint8Array ? {$bytes: [...value]} : value);
}
export function decodeState(text: string): Installation {
  try {
    const state = JSON.parse(text, (_, value) => {
      if (value && typeof value === 'object' && Object.keys(value).length === 1) {
        if ('$bigint' in value) {
          if (typeof value.$bigint !== 'string' || !/^(0|[1-9][0-9]*)$/.test(value.$bigint)) throw new CorruptState();
          return BigInt(value.$bigint);
        }
        if ('$bytes' in value) {
          if (!Array.isArray(value.$bytes) || !value.$bytes.every(byte)) throw new CorruptState();
          return Uint8Array.from(value.$bytes);
        }
      }
      return value;
    });
    if (!state || state.version !== RECOVERY_VERSION || !token(state.generation) || !token(state.installation)
      || !Array.isArray(state.directory) || !state.directory.every(streamKey)
      || new Set(state.directory).size !== state.directory.length
      || !state.streams || typeof state.streams !== 'object' || Array.isArray(state.streams)) throw new CorruptState();
    // A missing directory member is deliberately retained for the generation
    // loss path in Client.open(), rather than silently treated as unused.
    for (const [n, record] of Object.entries(state.streams)) {
      if (!state.directory.includes(n) || !validRecord(record as StreamRecord, n)) throw new CorruptState();
    }
    return state;
  } catch (error) { if (error instanceof CorruptState) throw error; throw new CorruptState('unreadable client state'); }
}
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
  try { await storage.write(state); }
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
      await storage.write(state);
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
export function recover(state: Installation, record: StreamRecord, granted: (want: Want) => boolean, onLost: () => void): boolean {
  const imported = record.id !== undefined && record.minter !== state.installation;
  if (!record.pending?.inflight && !imported) return false;
  if (record.pending?.inflight && !record.pending.http && granted(record.pending.want)) {
    delete record.pending;
    onLost();
  } else if (record.pending) {
    record.pending.inflight = false;
    record.pending.untouched = false;
  }
  if (imported) {
    record.minter = null;
    record.sent = true;
  }
  return true;
}
