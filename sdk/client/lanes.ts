import type {HttpReply} from './delivery.js';
import {maxU64, type HttpRequest, type Installation, type Lease, type Pending, type StreamRecord} from './persistence.js';
import type {Done, Fault, Outcome, Refusal, Want} from './wire.js';

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
function copyFault(fault: Fault): Fault {
  switch (fault.tag) {
    case 'Raised': return {tag: 'Raised', value: fault.value};
    case 'OutOfBytes': return {tag: 'OutOfBytes', value: {...fault.value}};
    case 'OutOfTime': return {tag: 'OutOfTime', value: fault.value};
    case 'Gone': return {tag: 'Gone'};
    case 'Conflict': return {tag: 'Conflict', value: fault.value};
  }
}
function copyRefusal(refusal: Refusal): Refusal {
  switch (refusal.tag) {
    case 'NoEntry': case 'Overloaded': case 'Deadline': case 'NotAllowed':
    case 'RequestMismatch': case 'HistoryUnavailable':
      return {tag: refusal.tag};
    case 'Malformed': return {tag: 'Malformed', value: {...refusal.value}};
    case 'Budget': return {tag: 'Budget', value: {...refusal.value}};
    case 'StreamFuture': return {tag: 'StreamFuture', value: refusal.value};
  }
}
// The terminal stays owned by recovery state while the original outcome goes to the caller.
function copyOutcome(outcome: Outcome<Uint8Array>): Outcome<Uint8Array> {
  switch (outcome.tag) {
    case 'Returned': return {tag: 'Returned', value: new Uint8Array(outcome.value)};
    case 'Failed': return {tag: 'Failed', value: copyFault(outcome.value)};
    case 'Refused': return {tag: 'Refused', value: copyRefusal(outcome.value)};
    case 'Abandoned': case 'InDoubt': case 'LocallyUnknown': case 'RequestMismatch':
      return {tag: outcome.tag};
  }
}
// Stores one lane's answer: its terminal, the position it moves to, and whether the stream ends.
export function settle(state: Installation, {lane, outcome, advance, retire}: Completion): void {
  const {pending, ...record} = state.streams[lane.stream];
  const {p} = pending!;
  record.terminal = {p, outcome: copyOutcome(outcome)};
  if (advance && p !== maxU64) record.next = p + 1n;
  if (retire || (advance && p === maxU64)) record.retired = true;
  state.streams[lane.stream] = record;
}
