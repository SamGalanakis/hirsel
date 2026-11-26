import type {HttpReply} from './delivery.js';
import {maxU64, type HttpRequest, type Lease, type Pending, type StreamRecord} from './persistence.js';
import type {Done, Outcome, Want} from './wire.js';

// A caller waiting on one call. `deadlineAt` (client clock) bounds this call alone.
export interface Waiter {
  resolve: (outcome: Outcome<Uint8Array>) => void; reject: (error: unknown) => void;
  overlay?: symbol; deadlineAt?: number;
}
export interface QueuedCall extends Waiter { want: Want; args: Uint8Array; slot: number; http?: HttpRequest }
// One leased store stream. A stream passes a position only after its reply is stored (STORE-SPEC
// §1.8 S7), so a lane carries one call at a time and calls multiplex over the session as lanes.
export interface Lane {
  readonly id: number; stream: string; record: StreamRecord; lease: Lease;
  call?: Waiter; abort?: AbortController; retry?: unknown; remintOnWelcome?: boolean;
}
export interface Completion { lane: Lane; outcome: Outcome<Uint8Array>; advance: boolean; retire?: boolean; http?: HttpReply }

export const deadlineRefused: Outcome<Uint8Array> = {tag: 'Refused', value: {tag: 'Deadline'}};
export const expired = (call: Waiter | undefined, now: number): boolean =>
  call?.deadlineAt !== undefined && now >= call.deadlineAt;
// The lane whose in-flight WebSocket send this row answers.
export const owner = (lanes: Iterable<Lane>, row: Done): Lane | undefined => [...lanes].find(({record: {pending, id}}) =>
  pending?.inflight && !pending.http && row.stream.t === id?.t && row.stream.n === id.n && row.p === pending.p);
// A new call at the stream's next position. It is untouched while only this installation has held its key.
export const pendingFor = (record: StreamRecord, call: QueuedCall, installation: bigint): Pending => ({
  p: record.next, want: call.want, args: call.args, slot: call.slot, inflight: false,
  untouched: record.minter === installation, remintsLeft: 1, ...(call.http && {http: call.http}),
});
// Stores one lane's answer: its terminal, the position it moves to, and whether the stream ends.
export function settle(record: StreamRecord, {outcome, advance, retire}: Completion): void {
  const pending = record.pending!;
  record.terminal = {p: pending.p, outcome: structuredClone(outcome)};
  delete record.pending;
  if (advance && pending.p !== maxU64) record.next = pending.p + 1n;
  if (retire || (advance && pending.p === maxU64)) record.retired = true;
}
