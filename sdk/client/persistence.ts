import {CorruptState, RECOVERY_VERSION, validCaller, validHttpRequest, type Installation, type Want, type Pending, type StreamRecord} from './client.js';

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
    if (!state || state.version !== RECOVERY_VERSION || typeof state.generation !== 'bigint'
      || !Array.isArray(state.directory) || !state.directory.every(u32)
      || new Set(state.directory).size !== state.directory.length
      || !Number.isSafeInteger(state.nextStream) || state.nextStream < 0 || state.nextStream > 0x100000000
      || state.directory.some((n: number) => n >= state.nextStream)
      || !state.streams || typeof state.streams !== 'object' || Array.isArray(state.streams)) throw new CorruptState();
    // A missing directory member is deliberately retained for the generation
    // loss path in Client.open(), rather than silently treated as unused.
    for (const [n, record] of Object.entries(state.streams)) {
      if (!state.directory.includes(Number(n)) || !validRecord(record as StreamRecord, state, Number(n))) throw new CorruptState();
    }
    return state;
  } catch (error) { if (error instanceof CorruptState) throw error; throw new CorruptState('unreadable client state'); }
}
const byte = (n: unknown) => Number.isInteger(n) && (n as number) >= 0 && (n as number) <= 255;
const u32 = (n: unknown) => Number.isInteger(n) && (n as number) >= 0 && (n as number) <= 0xffffffff;
const u64 = (n: unknown) => typeof n === 'bigint' && n >= 0n && n <= 0xffffffffffffffffn;
function want(w: Want): boolean {
  return !!w && w.identity instanceof Uint8Array && u64(w.args_fp) && u64(w.result_fp);
}
function pending(p: Pending, state: Installation, stream: number, next: bigint): boolean {
  return p.generation === state.generation && p.stream === stream && p.p === next && want(p.want)
    && p.args instanceof Uint8Array && u32(p.slot) && Number.isSafeInteger(p.attempts) && p.attempts >= 0
    && typeof p.possiblyAdmitted === 'boolean' && state.streams[stream].caller !== null
    && (p.http === undefined || validHttpRequest(p.http));
}
function validRecord(r: StreamRecord, state: Installation, stream: number): boolean {
  if (!r || (r.caller !== null && !validCaller(r.caller)) || !u64(r.next) || typeof r.retired !== 'boolean' || (r.pending && !pending(r.pending, state, stream, r.next))) return false;
  if (r.terminal && (!u64(r.terminal.p) || !r.terminal.outcome || !['Returned', 'Failed', 'Refused'].includes(r.terminal.outcome.tag))) return false;
  return true;
}
