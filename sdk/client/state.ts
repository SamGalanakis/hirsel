import type {Frame} from './wire.js';

type Welcome = Extract<Frame, {tag: 'Welcome'}>;
type Update = Extract<Frame, {tag: 'Batch' | 'Reset' | 'Live' | 'Ended'}>;
type Credential = {bytes: Uint8Array; source: 'initial' | 'refreshed'};

interface Epoch { readonly epoch: number }
interface Dormant extends Epoch { backoff: number; credential?: Credential }
type Attempt = {tag: 'Idle' | 'Preparing' | 'WebSocket' | 'Retry'} | {tag: 'Http'; p: bigint};
// One lane per leased stream. A lane carries at most one call (STORE §1.8 S7: a stream passes a
// position only after its reply), so calls multiplex over the connection as lanes, at most
// `width` = 1+q of them owing the place an Outcome at once (RUNNER-SPEC §2 V1).
interface Lane { readonly attempt: Attempt; readonly backoff: number }
type Lanes = Readonly<Record<number, Lane>>;
type Stopped = 'Fenced' | 'StorageUnavailable' | 'Closed';
type Reconnectable = (Dormant & {tag: 'Backoff' | 'Incompatible'}) | (Epoch & {tag: 'Unbound'; backoff: number});
type LostCall = 'LocallyUnknown' | 'RequestMismatch';
type Connection =
  | (Dormant & {tag: 'Opening' | 'AwaitWelcome' | 'Binding'})
  | (Dormant & {tag: 'Placing'; remaining: number; welcomedAt: number; width: number})
  | (Dormant & {tag: 'Ready'; welcomedAt: number; version: bigint; width: number; lanes: Lanes});
type Loss = {tag: 'Transient'} | {tag: 'Incompatible'; code: number} | {tag: 'CredentialLost'; cause: string};

export type Phase =
  | Connection
  | Reconnectable
  | (Dormant & {tag: 'Starting'})
  | (Epoch & {tag: 'Initial'; backoff: number})
  | (Epoch & {tag: 'Acquiring'; backoff: number; request: {tag: 'Initial'} | {tag: 'Refresh'; refused?: Uint8Array}})
  | (Dormant & {tag: 'Losing'; loss: Loss})
  | (Epoch & {tag: Stopped});

type Resume = () => void | Promise<void>;

export type Event =
  | {tag: 'Start' | 'Reconnect'}
  | (Epoch & {tag: 'Begin' | 'SocketOpen' | 'LossRecorded' | 'BackoffElapsed'})
  | (Epoch & {tag: 'RetryElapsed' | 'CallFinished' | 'RetryCall'; lane: number})
  | (Epoch & {tag: 'StableElapsed'; at: number})
  | (Epoch & {tag: 'CredentialResult'; bytes?: Uint8Array})
  | (Epoch & {tag: 'SocketLost'; code?: number})
  | (Epoch & {tag: 'Frame'; frame: Frame})
  | (Epoch & {tag: 'WelcomeBound'; at: number; width: number})
  | (Epoch & {tag: 'AdvanceVersion'; version: bigint; apply: Resume})
  | (Epoch & {tag: 'PrepareCall'; lane: number; prepare: Resume})
  | (Epoch & {tag: 'SendCall'; lane: number; delivery: {tag: 'WebSocket'} | {tag: 'Http'; p: bigint}; send: Resume})
  | (Epoch & {tag: 'HttpComplete'; lane: number; p: bigint; complete: Resume})
  | (Epoch & {tag: 'Transient'})
  | (Epoch & {tag: 'Incompatible'; code: number})
  | {tag: 'CredentialLost'; cause: string}
  | {tag: 'Unbound'; cause: string}
  | {tag: 'Stop'; state: Stopped; outcome?: LostCall; diagnostic?: string}
  | (Epoch & {tag: 'Resume'; run: Resume});

export type Action =
  | {tag: 'CancelTimers' | 'ClearSubscriptions' | 'CloseSocket' | 'OpenSocket' | 'SendHello' | 'RecordLoss' | 'ResetReady' | 'ContinueFrame'}
  | {tag: 'Begin' | 'SendPending'}
  | {tag: 'State'; state: 'Connecting' | 'Ready' | 'Backoff' | 'CredentialNeeded' | 'Incompatible' | 'Unbound' | Stopped}
  | {tag: 'RefreshCredential'}
  | {tag: 'Diagnostic'; message: string}
  | {tag: 'BindWelcome'; frame: Welcome}
  | {tag: 'Update'; frame: Update}
  | {tag: 'ArmBackoff'; maxMs: number}
  | {tag: 'ArmRetry'; lane: number; maxMs: number}
  | {tag: 'BecomeReady'; welcomedAt: number}
  | {tag: 'Finish'; outcome: LostCall}
  | {tag: 'RejectReady'; code: string}
  | {tag: 'RejectCalls'; code: string}
  | {tag: 'ClearOverlays'}
  | {tag: 'Run'; run: Resume};

const initialBackoff = 100;
export const initialPhase: Phase = {tag: 'Initial', epoch: 0, backoff: initialBackoff};
export const stableMs = 30000;

export interface StepContext {
  readonly wants: number;
  readonly hasCredentialSource: boolean;
}

interface Transition { readonly phase: Phase; readonly actions: readonly Action[] }

const change = (phase: Phase, ...actions: Action[]): Transition => ({phase, actions});
export const current = (phase: Phase, {epoch}: Epoch): boolean => phase.epoch === epoch;
export const canReconnect = (phase: Phase): phase is Reconnectable =>
  phase.tag === 'Backoff' || phase.tag === 'Incompatible' || phase.tag === 'Unbound';
export const credential = (phase: Phase): Uint8Array | undefined =>
  'credential' in phase ? phase.credential?.bytes : undefined;
const nextBackoff = (maxMs: number) => Math.min(10000, maxMs * 2);
const teardown: readonly Action[] = [{tag: 'CancelTimers'}, {tag: 'CloseSocket'}, {tag: 'ClearSubscriptions'}];

function begin(phase: Dormant, {hasCredentialSource}: StepContext): Transition {
  const epoch = phase.epoch + 1;
  if (hasCredentialSource && !phase.credential) {
    return change({tag: 'Acquiring', epoch, backoff: phase.backoff, request: {tag: 'Initial'}},
      {tag: 'State', state: 'Connecting'}, {tag: 'RefreshCredential'});
  }
  return change({tag: 'Opening', epoch, backoff: phase.backoff, credential: phase.credential},
    {tag: 'State', state: 'Connecting'}, {tag: 'ClearSubscriptions'}, {tag: 'OpenSocket'});
}

function unbound(phase: Dormant | Extract<Phase, {tag: 'Acquiring'}>, cause: string, ctx: StepContext): Transition {
  const message = ctx.hasCredentialSource
    ? `Unbound: the place refused the credential and one refresh did not recover (${cause})`
    : `Unbound: the place binds callers and needs a credential; pass \`credential\` to Client.open (${cause})`;
  return change({tag: 'Unbound', epoch: phase.epoch + 1, backoff: phase.backoff},
    ...teardown, {tag: 'State', state: 'Unbound'}, {tag: 'Diagnostic', message}, {tag: 'RejectReady', code: 'Unbound'});
}

function lose(phase: Dormant, loss: Loss, ctx: StepContext): Transition {
  const {backoff, credential} = phase;
  const epoch = phase.epoch + 1;
  switch (loss.tag) {
    case 'Transient':
      return change({tag: 'Backoff', epoch, backoff: nextBackoff(backoff), credential}, ...teardown,
        {tag: 'ArmBackoff', maxMs: backoff}, {tag: 'State', state: 'Backoff'});
    case 'Incompatible': {
      const {code} = loss;
      const message = code === 1009 ? 'Stored reply exceeds the server message limit (1009)'
        : `Incompatible (${code}); reload bindings or reconnect explicitly`;
      return change({tag: 'Incompatible', epoch, backoff, credential}, ...teardown,
        {tag: 'State', state: 'Incompatible'}, {tag: 'Diagnostic', message},
        ...(code === 1009 ? [{tag: 'RejectCalls', code: 'ReplyTooLarge'} as const] : []),
        {tag: 'RejectReady', code: code === 1009 ? 'ReplyTooLarge' : 'Incompatible'});
    }
    case 'CredentialLost':
      if (!ctx.hasCredentialSource || credential?.source === 'refreshed') return unbound(phase, loss.cause, ctx);
      return change({tag: 'Acquiring', epoch, backoff, request: {tag: 'Refresh', refused: credential?.bytes}},
        ...teardown, {tag: 'State', state: 'CredentialNeeded'},
        {tag: 'Diagnostic', message: `CredentialNeeded: the place refused the session credential (${loss.cause}); refreshing once`},
        {tag: 'RefreshCredential'});
  }
}

function socketLoss(phase: Connection, code?: number): Loss {
  if (code === 4401) return {tag: 'CredentialLost', cause: 'Close 4401'};
  if (code === 1008 && phase.tag !== 'Ready' && phase.tag !== 'Placing')
    return {tag: 'CredentialLost', cause: 'Close 1008 before Welcome'};
  if (code === 1007 || code === 1008 || code === 1009) return {tag: 'Incompatible', code};
  return {tag: 'Transient'};
}

const connected = (phase: Phase): phase is Connection =>
  phase.tag === 'Opening' || phase.tag === 'AwaitWelcome' || phase.tag === 'Binding' ||
  phase.tag === 'Placing' || phase.tag === 'Ready';

function ready(phase: Dormant, welcomedAt: number, width: number): Transition {
  const {epoch, backoff, credential} = phase;
  return change({tag: 'Ready', epoch, backoff, credential, welcomedAt, version: 0n, width, lanes: {}},
    {tag: 'State', state: 'Ready'}, {tag: 'BecomeReady', welcomedAt}, {tag: 'ContinueFrame'});
}

const idleLane: Lane = {attempt: {tag: 'Idle'}, backoff: initialBackoff};
const lane = (phase: Extract<Phase, {tag: 'Ready'}>, id: number): Lane => phase.lanes[id] ?? idleLane;
const withLane = (phase: Extract<Phase, {tag: 'Ready'}>, id: number, attempt: Attempt): Phase =>
  ({...phase, lanes: {...phase.lanes, [id]: {attempt, backoff: lane(phase, id).backoff}}});
// Sends the place still owes an Outcome row: each holds one of the session's 1+q units.
const owed = (phase: Extract<Phase, {tag: 'Ready'}>): number =>
  Object.values(phase.lanes).filter(({attempt}) => owes(attempt)).length;
const owes = (attempt: Attempt) => attempt.tag === 'Preparing' || attempt.tag === 'WebSocket' || attempt.tag === 'Http';
// Whether the place owes this lane's send an Outcome, so its answer comes by the deadline it was sent.
export const laneOwed = (phase: Phase, id: number): boolean => phase.tag === 'Ready' && owes(lane(phase, id).attempt);

export function step(phase: Phase, event: Event, ctx: StepContext): Transition {
  if ('epoch' in event && !current(phase, event)) return change(phase);
  switch (event.tag) {
    case 'Start':
      if (phase.tag === 'Initial') return begin(phase, ctx);
      break;
    case 'Reconnect':
      if (canReconnect(phase)) {
        const bytes = credential(phase);
        return change({tag: 'Starting', epoch: phase.epoch + 1, backoff: phase.backoff,
          credential: bytes && {bytes, source: 'initial'}},
          {tag: 'CancelTimers'}, {tag: 'ResetReady'}, {tag: 'State', state: 'Connecting'}, {tag: 'Begin'});
      }
      break;
    case 'Begin':
      if (phase.tag === 'Starting') return begin(phase, ctx);
      break;
    case 'SocketOpen':
      if (phase.tag === 'Opening') return change({...phase, tag: 'AwaitWelcome'}, {tag: 'SendHello'});
      break;
    case 'CredentialResult':
      if (phase.tag === 'Acquiring') {
        const refused = phase.request.tag === 'Refresh' ? phase.request.refused : undefined;
        const {bytes} = event;
        if (!bytes) return unbound(phase, 'the credential source gave none', ctx);
        if (refused && refused.length === bytes.length && refused.every((byte, i) => byte === bytes[i]))
          return unbound(phase, 'the refresh returned the refused credential', ctx);
        const next = begin({...phase, credential: {bytes: bytes.slice(),
          source: phase.request.tag === 'Refresh' ? 'refreshed' : 'initial'}}, ctx);
        return change(next.phase, {tag: 'State', state: 'Connecting'}, ...next.actions);
      }
      break;
    case 'SocketLost':
      if (connected(phase)) {
        const {credential, backoff} = phase;
        return change({tag: 'Losing', epoch: phase.epoch + 1, credential, backoff, loss: socketLoss(phase, event.code)},
          ...teardown, {tag: 'RecordLoss'});
      }
      break;
    case 'LossRecorded':
      if (phase.tag === 'Losing') return lose(phase, phase.loss, ctx);
      break;
    case 'BackoffElapsed':
      if (phase.tag === 'Backoff') {
        const next = begin(phase, ctx);
        return change(next.phase, {tag: 'CancelTimers'}, ...next.actions);
      }
      break;
    case 'StableElapsed':
      if (phase.tag === 'Ready' && event.at - phase.welcomedAt >= stableMs)
        return change({...phase, backoff: initialBackoff});
      break;
    case 'Transient':
    case 'Incompatible':
      if (connected(phase)) return lose(phase, event, ctx);
      break;
    case 'CredentialLost':
    case 'Unbound':
      if (connected(phase) || phase.tag === 'Backoff')
        return event.tag === 'Unbound' ? unbound(phase, event.cause, ctx) : lose(phase, event, ctx);
      break;
    case 'Frame': {
      if (!connected(phase)) break;
      const {frame} = event;
      if (phase.tag === 'AwaitWelcome' && frame.tag === 'Welcome')
        return change({...phase, tag: 'Binding'}, {tag: 'BindWelcome', frame});
      if (phase.tag === 'Placing' && frame.tag === 'Placed' && frame.value.tag !== 'Gone')
        return phase.remaining === 1 ? ready(phase, phase.welcomedAt, phase.width)
          : change({...phase, remaining: phase.remaining - 1});
      if (phase.tag === 'Ready') {
        switch (frame.tag) {
          case 'Batch':
          case 'Reset':
            if (frame.values[0] < phase.version) break;
            return change(phase, {tag: 'Update', frame}, {tag: 'ContinueFrame'});
          case 'Live':
          case 'Ended':
            return change(phase, {tag: 'Update', frame}, {tag: 'ContinueFrame'});
        }
      }
      return lose(phase, {tag: 'Incompatible', code: 1008}, ctx);
    }
    case 'WelcomeBound':
      if (phase.tag === 'Binding') {
        const accepted = {...phase, credential: phase.credential && {bytes: phase.credential.bytes, source: 'initial' as const}};
        return ctx.wants === 0 ? ready(accepted, event.at, event.width)
          : change({...accepted, tag: 'Placing', remaining: ctx.wants, welcomedAt: event.at, width: event.width});
      }
      break;
    case 'AdvanceVersion':
      if (phase.tag === 'Ready') return change({...phase, version: event.version}, {tag: 'Run', run: event.apply});
      break;
    case 'PrepareCall':
      if (phase.tag === 'Ready' && lane(phase, event.lane).attempt.tag === 'Idle' && owed(phase) < phase.width)
        return change(withLane(phase, event.lane, {tag: 'Preparing'}), {tag: 'Run', run: event.prepare});
      break;
    case 'SendCall':
      if (phase.tag === 'Ready' && lane(phase, event.lane).attempt.tag === 'Preparing')
        return change(withLane(phase, event.lane, event.delivery), {tag: 'Run', run: event.send});
      break;
    case 'HttpComplete': {
      if (phase.tag !== 'Ready') break;
      const {attempt} = lane(phase, event.lane);
      if (attempt.tag === 'Http' && attempt.p === event.p) return change(phase, {tag: 'Run', run: event.complete});
      break;
    }
    case 'CallFinished':
      if (phase.tag === 'Ready') {
        const {[event.lane]: _, ...lanes} = phase.lanes;
        return change({...phase, lanes}, {tag: 'SendPending'});
      }
      break;
    case 'RetryCall':
      if (phase.tag === 'Ready') {
        const {backoff} = lane(phase, event.lane);
        return change({...phase, lanes: {...phase.lanes, [event.lane]: {attempt: {tag: 'Retry'}, backoff: nextBackoff(backoff)}}},
          {tag: 'ArmRetry', lane: event.lane, maxMs: backoff}, {tag: 'SendPending'});
      }
      break;
    case 'RetryElapsed':
      if (phase.tag === 'Ready' && lane(phase, event.lane).attempt.tag === 'Retry')
        return change(withLane(phase, event.lane, {tag: 'Idle'}), {tag: 'SendPending'});
      break;
    case 'Stop':
      if (phase.tag !== 'Closed' || event.state === 'Closed')
        return change({tag: event.state, epoch: phase.epoch + 1}, {tag: 'ClearOverlays'}, ...teardown,
          {tag: 'State', state: event.state},
          ...(event.diagnostic === undefined ? [] : [{tag: 'Diagnostic', message: event.diagnostic} as const]),
          ...(event.outcome === undefined ? [] : [{tag: 'Finish', outcome: event.outcome} as const]),
          {tag: 'RejectReady', code: event.state});
      break;
    case 'Resume':
      if (connected(phase) || phase.tag === 'Losing') return change(phase, {tag: 'Run', run: event.run});
      break;
  }
  return change(phase);
}
