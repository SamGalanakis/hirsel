import type {HttpReply} from './delivery.js';
import type {Clock} from './browser.js';
import type {CallTiming, EntryKind, Options} from './client.js';
import {equal} from './image.js';
import {maxU64, recover, type HttpRequest, type Installation, type Lease, type Pending, type StreamRecord} from './persistence.js';
import type {Done, Fault, Outcome, Refusal, Want} from './wire.js';

// A caller waiting on one call. `deadlineAt` (client clock) bounds this call alone.
export interface Waiter {
  resolve: (outcome: Outcome<Uint8Array>) => void; reject: (error: unknown) => void;
  overlay?: symbol; deadlineAt?: number;
}
export interface Binding { readonly slot: number; readonly kind: EntryKind }
export type Leased = readonly [stream: string, lease: Lease, binding?: Binding];
export interface QueuedCall extends Waiter {
  want: Want; args: Uint8Array; binding?: Binding; http?: HttpRequest;
  timing?: ReturnType<typeof timeCall>;
}
// One leased store stream. A stream passes a position only after its reply is stored (STORE-SPEC
// §1.8 S7), so a lane carries one call at a time and calls multiplex over the session as lanes.
export interface Lane {
  readonly id: number; stream: string; record: StreamRecord; lease: Lease;
  call?: Waiter; binding?: Binding; abort?: AbortController; retry?: unknown; remintOnWelcome?: boolean;
}
export interface Completion { lane: Lane; outcome: Outcome<Uint8Array>; advance: boolean; retire?: boolean; http?: HttpReply }

export const sameWant = (a: Want, b: Want) => equal(a.identity, b.identity) && a.args_fp === b.args_fp && a.result_fp === b.result_fp;
export const copyWant = (w: Want): Want => ({identity: new Uint8Array(w.identity), args_fp: w.args_fp, result_fp: w.result_fp});
export function resolveBinding(wants: readonly Want[], kinds: readonly EntryKind[], want: Want): Binding | undefined {
  const slot = wants.findIndex(w => sameWant(w, want));
  const kind = kinds[slot];
  if (kind !== undefined) return {slot, kind};
}

export function recoverLanes(state: Installation, leased: readonly Leased[], wants: readonly Want[],
  kinds: readonly EntryKind[], onLost: () => void): {changed: boolean; lanes: Leased[]} {
  let changed = false;
  const lanes = leased.map(([stream, lease]) => {
    const pending = state.streams[stream].pending;
    const binding = pending && !pending.http ? resolveBinding(wants, kinds, pending.want) : undefined;
    changed = recover(state, stream, binding?.kind === 'granted', onLost) || changed;
    return [stream, lease, state.streams[stream].pending ? binding : undefined] as const;
  });
  return {changed, lanes};
}

export function timeCall(clock: Clock, observe: NonNullable<Options['onCallTiming']>) {
  const enqueuedAt = clock.now();
  let admittedAt: number | undefined, settled = false;
  return {
    admit() { admittedAt = clock.now(); },
    settle() {
      if (settled) return;
      settled = true;
      const timing: CallTiming = {enqueuedAt, ...(admittedAt !== undefined && {admittedAt}), outcomeAt: clock.now()};
      void Promise.resolve().then(() => observe(timing)).catch(() => {});
    },
  };
}

export function takeCalls(calls: Iterable<QueuedCall>, lanes: Iterable<Lane>, width: number): QueuedCall[] {
  let room = width;
  for (const lane of lanes) if (lane.record.pending) room--;
  const take: QueuedCall[] = [];
  if (room > 0) for (const call of calls) {
    take.push(call);
    if (take.length === room) break;
  }
  return take;
}

export const deadlineRefused: Outcome<Uint8Array> = {tag: 'Refused', value: {tag: 'Deadline'}};
export const expired = (call: Waiter | undefined, now: number): boolean =>
  call?.deadlineAt !== undefined && now >= call.deadlineAt;
// The lane whose in-flight WebSocket send this row answers.
export const owner = (lanes: Iterable<Lane>, row: Done): Lane | undefined => [...lanes].find(({record: {pending, id}}) =>
  pending?.inflight && !pending.http && row.stream.t === id?.t && row.stream.n === id.n && row.p === pending.p);
// A new call at the stream's next position. It is untouched while only this installation has held its key.
export const pendingFor = (record: StreamRecord, call: QueuedCall, installation: bigint): Pending => ({
  p: record.next, want: call.want, args: call.args, slot: call.binding?.slot ?? 0, inflight: false,
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
