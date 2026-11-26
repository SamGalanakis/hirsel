import {defaultClock, listenForBrowserWake, type Clock, type Socket} from './browser.js';
import {claimStream, CorruptState, leaseStreams, recover, sameCaller, emptyStream, fresh, hex, intact, maxU64, randomGeneration, streamHex, replaceStream,
  streamLock, u64, validCaller, validHttpRequest, type Caller, type HttpRequest, type Installation,
  type Lease, type Storage, type StreamId, type StreamRecord} from './persistence.js';
export {CorruptState, RECOVERY_VERSION, intact, validCaller, validHttpRequest} from './persistence.js';
export type {Clock, Socket} from './browser.js';
export type {Caller, HttpRequest, Installation, Lease, Pending, Storage, StreamId, StreamRecord} from './persistence.js';
import {applyImage, commitVisible, equal, invalidation, rawSubscription, runOverlay,
  stageImage, subscriptionHandle, type CallOptions, type Change, type Draft, type ImageEdits, type ImageSlots,
  type Layer, type Overlay, type Sub, type Subscription, type SubscriptionCodec} from './image.js';
import {RoomMember, type Decoded, type LiveLink, type LiveRoom, type RoomCodec, type RoomHandlers} from './room.js';
export type {LiveRoom, PeerId, RoomCodec, RoomHandlers, RoomPeer, RoomPeersChange, ValueCodec} from './room.js';
export type {Access, CallOptions, Change, Draft, Frozen, RowChange, Subscription, SubscriptionCodec, SubscriptionControl} from './image.js';

import {canReconnect, credential, current, initialPhase, laneOwed, stableMs, step, type Action, type Event, type Phase} from './state.js';

import {Inbox, type Codec, type Done, type Entry, type Fault, type Frame, type Group, type Option, type Outcome,
  type Part, type Refusal, type Room, type SlotRow, type SubscriptionEnd, type Want, type WireOutcome} from './wire.js';
export type {Codec, Done, Entry, Fault, Frame, Group, Option, Outcome, Part, Refusal, Room, SlotRow, SubscriptionEnd, Want} from './wire.js';
export interface Transport {
  call<R>(want: Want, args: Uint8Array, decode: (bytes: Uint8Array) => R, options?: CallOptions): Promise<Outcome<R>>;
  subscribe(want: Want, args: Uint8Array, onChange: (sub: Subscription, change: Change<Uint8Array, Record<number, SlotRow>>) => void,
    onEnd?: (reason: SubscriptionEnd) => void): Subscription;
  subscribe<V, Slots extends object, Keys extends Record<keyof Slots, unknown>>(want: Want, args: Uint8Array,
    onChange: (sub: Subscription<V, Slots, Keys>, change: Change<V, Slots>) => void,
    onEnd: ((reason: SubscriptionEnd) => void) | undefined,
    decode: SubscriptionCodec<V, Slots, Keys>): Subscription<V, Slots, Keys>;
  room<S, M, V, Slots extends object, Keys extends Record<keyof Slots, unknown>>(want: Want, args: Uint8Array,
    handlers: RoomHandlers<S, M, V, Slots, Keys>, decode: SubscriptionCodec<V, Slots, Keys>, live: RoomCodec<S, M>): LiveRoom<S, M, V, Slots, Keys>;
}
import {deadlineRefused, expired, owner, pendingFor, settle, type Completion, type Lane, type QueuedCall, type Waiter} from './lanes.js';
import {authorization, httpVerdict, sendHttp, type HttpAttempt, type HttpOptions, type HttpReply} from './delivery.js';
export type {HttpReply} from './delivery.js';
export type State = 'Connecting' | 'Ready' | 'Backoff' | 'Incompatible' | 'Unbound' | 'CredentialNeeded' | 'Fenced' | 'StorageUnavailable' | 'Closed';
export interface Identity {digest: Uint8Array; target: string; n_place: bigint}
// The generated bindings' `kind`, one per `want`. Only a transact call is stored, so only its
// reply moves the stream position; a granted call's body is never re-sent once it may have run.
export type EntryKind = 'compute' | 'transact' | 'query' | 'granted' | 'room';
export interface Options {
  url: string; storage: Storage; frame: Codec; wants: Want[]; kinds: readonly EntryKind[]; identity?: Identity;
  // Called for the first Hello and once per credential recovery, outside the client's
  // serialized work and storage lock. Undefined bytes mean none can be had.
  credential?: () => Promise<Uint8Array | undefined>;
  http?: HttpOptions;
  onHttpRecovered?: (reply: HttpReply) => void;
  socket?: (url: string, protocol: string) => Socket; clock?: Clock; deadlineMs?: number;
  onState?: (state: State) => void;
  onDiagnostic?: (diagnostic: string) => void;
  onRecovered?: (outcome: Outcome<Uint8Array>) => void;
  onBatch?: (version: bigint) => void;
}
const sameWant = (a: Want, b: Want) => equal(a.identity, b.identity) && a.args_fp === b.args_fp && a.result_fp === b.result_fp;
const kindOf = (options: Options, want: Want): EntryKind | undefined => options.kinds[options.wants.findIndex(w => sameWant(w, want))];
const granted = (options: Options) => (want: Want) => kindOf(options, want) === 'granted';
const lost = (options: Options) => () => options.onRecovered?.({tag: 'LocallyUnknown'});
const copyWant = (w: Want): Want => ({identity: w.identity.slice(), args_fp: w.args_fp, result_fp: w.result_fp});
const futureSlackMs = 300000n;
const credentialUsable = (state: State) => state === 'Ready' || state === 'Connecting' || state === 'Backoff';

export class ClientError extends Error {
  constructor(public readonly code: string) { super(code); this.name = 'ClientError'; }
}
export class Client implements Transport {
  state: State = 'Connecting';
  readonly generation: bigint;
  private readonly installationId: bigint;
  private httpAuthority = new AbortController();
  private readonly credentialWaiters = new Set<() => void>();
  private caller: Caller | null;
  private lanes = new Map<number, Lane>();
  private nextLane = 0;
  private phase: Phase = initialPhase;
  private socket?: Socket;
  private readonly inbox = new Inbox();
  private work: Promise<void> = Promise.resolve();
  private pumpQueued = false;
  private wants: Want[];
  private keyedAt = 0n;
  private keyedAtClock = 0;
  private retention = 0n;
  private timer?: unknown;
  private stableTimer?: unknown;
  private clock: Clock;
  private removeBrowserWake?: () => void;
  private nextSub = 0;
  private subs = new Set<Sub>();
  private rooms = new Map<Sub, RoomMember>();
  private live: LiveLink = {
    ready: () => this.phase.tag === 'Ready' && this.socket !== undefined, backlog: () => this.socket?.bufferedAmount ?? 0,
    write: frame => this.writeFrame(frame, this.phase.epoch),
    clock: {set: (fn, ms) => this.clock.set(fn, ms), clear: id => this.clock.clear(id)},
  };
  private handles = new WeakMap<object, Sub>();
  private overlays = new Map<symbol, Overlay>();
  private earlyOverlays = new Set<symbol>();
  private layer: Layer = new Map();
  private notifications?: Array<() => void>;
  private calls = new Set<QueuedCall>();
  private readyResolve!: () => void;
  private readyReject!: (error: unknown) => void;
  private ready: Promise<void>;

  private constructor(private options: Options, state: Installation, leased: ReadonlyArray<readonly [string, Lease]>) {
    this.generation = state.generation; this.installationId = state.installation;
    this.caller = state.streams[leased[0][0]].caller;
    for (const [stream, lease] of leased) this.addLeased(stream, structuredClone(state.streams[stream]), lease);
    this.wants = options.wants.map(copyWant);
    this.clock = options.clock ?? defaultClock;
    this.ready = new Promise((resolve, reject) => {this.readyResolve = resolve; this.readyReject = reject;});
    void this.ready.catch(() => {});
  }

  private addLeased(stream: string, record: StreamRecord, lease: Lease): Lane {
    const lane: Lane = {id: this.nextLane++, stream, record, lease};
    this.lanes.set(lane.id, lane);
    this.watchLease(lane, lease);
    return lane;
  }
  private watchLease(lane: Lane, lease: Lease): void {
    void lease.lost.then(() => this.enqueue(async () => {
      if (this.lanes.get(lane.id) === lane && lane.lease === lease && this.state !== 'Closed') await this.stop('Fenced', 'LocallyUnknown');
    }));
  }

  static async open(options: Options): Promise<Client> {
    if (options.kinds.length !== options.wants.length) throw new ClientError('InvalidBindings');
    const lock = await options.storage.lock('install', true);
    if (!lock) throw new ClientError('StorageUnavailable');
    const leased: Array<[string, Lease]> = [];
    try {
      let state: Installation | null, reset = false;
      try { state = await options.storage.read(); }
      catch (error) {
        if (!(error instanceof CorruptState)) throw error;
        state = null;
      }
      if (!state || !intact(state)) {
        state = fresh(state?.installation);
        reset = true;
      }
      leased.push(...await leaseStreams(options.storage, state));
      if (leased.length === 0) throw new ClientError('StreamLeaseUnavailable');
      let changed = false;
      for (const [n] of leased) changed = recover(state, state.streams[n], granted(options), lost(options)) || changed;
      if (changed) await options.storage.write(state);
      if (reset) options.onRecovered?.({tag: 'LocallyUnknown'});
      const client = new Client(options, state, leased);
      await lock.release();
      await client.run({tag: 'Start'});
      client.removeBrowserWake = listenForBrowserWake(() => client.wakeBackoff());
      return client;
    } catch (error) {
      for (const [, lease] of leased) await lease.release();
      await lock.release(); throw error;
    }
  }

  whenReady(): Promise<void> { return this.ready; }
  private enqueue(fn: () => Promise<void>): Promise<void> {
    this.inbox.seal();
    const next = this.work.then(fn);
    this.work = next.catch(error => error instanceof ClientError ? undefined : this.unavailable(error));
    return next;
  }
  private setState(state: State): void {
    this.state = state;
    if (state === 'CredentialNeeded' || !credentialUsable(state)) this.httpAuthority.abort(new ClientError(state));
    else if (this.httpAuthority.signal.aborted) this.httpAuthority = new AbortController();
    for (const wake of this.credentialWaiters) wake();
    if (state !== 'CredentialNeeded' && !credentialUsable(state)) {
      for (const call of this.calls) {
        if (call.overlay) this.overlays.delete(call.overlay);
        call.reject(new ClientError(state));
      }
      this.calls.clear();
    }
    this.options.onState?.(state);
  }
  private async unavailable(error: unknown): Promise<void> {
    if (this.phase.tag === 'Closed') return;
    await this.run({tag: 'Stop', state: 'StorageUnavailable', diagnostic: String(error)});
  }
  private async installation<T>(fn: (state: Installation) => Promise<T>): Promise<T | undefined> {
    const lock = await this.options.storage.lock('install', true);
    if (!lock) {
      const error = new ClientError('StorageUnavailable');
      await this.unavailable(error);
      throw error;
    }
    try {
      let state: Installation | null;
      try { state = await this.options.storage.read(); }
      catch (e) { if (!(e instanceof CorruptState)) throw e; state = null; }
      if (!state || !intact(state)) {
        await this.options.storage.write(fresh(state?.installation));
        await this.stop('Fenced', 'LocallyUnknown'); return;
      }
      if (state.installation !== this.installationId || state.generation !== this.generation ||
          [...this.lanes.values()].some(({stream}) => !state.streams[stream] || !sameCaller(state.streams[stream].caller, this.caller))) {
        await this.stop('Fenced', 'LocallyUnknown'); return;
      }
      return await fn(state);
    } finally { await lock.release(); }
  }
  private run(event: Event): Promise<void> {
    const {phase, actions} = step(this.phase, event, {
      wants: this.wants.length, hasCredentialSource: this.options.credential !== undefined,
    });
    this.phase = phase;
    return this.effects(phase, actions);
  }
  private post(event: Event): void {
    void this.enqueue(() => this.run(event)).catch(() => {});
  }
  private async effects(scope: Phase, actions: readonly Action[]): Promise<void> {
    const {epoch} = scope;
    for (const action of actions) {
      if (!current(this.phase, scope)) return;
      switch (action.tag) {
        case 'State': this.setState(action.state); break;
        case 'Diagnostic': this.options.onDiagnostic?.(action.message); break;
        case 'ClearOverlays': this.overlays.clear(); break;
        case 'CancelTimers':
          for (const timer of [this.timer, this.stableTimer, ...[...this.lanes.values()].map(lane => lane.retry)])
            if (timer !== undefined) this.clock.clear(timer);
          this.timer = this.stableTimer = undefined;
          for (const lane of this.lanes.values()) lane.retry = undefined;
          break;
        case 'ClearSubscriptions': this.clearSubscriptions(); break;
        case 'CloseSocket': {
          const socket = this.socket;
          this.socket = undefined;
          for (const lane of this.lanes.values()) { lane.abort?.abort(); lane.abort = undefined; }
          socket?.close();
          break;
        }
        case 'OpenSocket': this.openSocket(epoch); break;
        case 'SendHello':
          await this.send({tag: 'Hello', values: [this.helloCredential(), this.wants]}, epoch);
          break;
        case 'RefreshCredential':
          void Promise.resolve().then(this.options.credential!).then(
            bytes => this.post({tag: 'CredentialResult', epoch, bytes}),
            () => this.post({tag: 'CredentialResult', epoch}),
          );
          break;
        case 'RecordLoss':
          void this.enqueue(async () => {
            await this.run({tag: 'Resume', epoch, run: async () => {
              await this.noteLost([...this.lanes.values()]);
              await this.run({tag: 'LossRecorded', epoch});
            }});
          }).catch(() => {});
          break;
        case 'Begin': this.post({tag: 'Begin', epoch}); break;
        case 'ResetReady':
          this.ready = new Promise((resolve, reject) => {this.readyResolve = resolve; this.readyReject = reject;});
          void this.ready.catch(() => {});
          break;
        case 'ArmBackoff':
          this.timer = this.clock.set(() => this.post({tag: 'BackoffElapsed', epoch}), this.clock.random() * action.maxMs);
          break;
        case 'ArmRetry': {
          const lane = this.lanes.get(action.lane);
          if (!lane) break;
          if (lane.retry !== undefined) this.clock.clear(lane.retry);
          lane.retry = this.clock.set(() => this.post({tag: 'RetryElapsed', epoch, lane: lane.id}), this.clock.random() * action.maxMs);
          break;
        }
        case 'BindWelcome': await this.bindWelcome(action.frame, epoch); break;
        case 'BecomeReady':
          this.stableTimer = this.clock.set(() => this.post({tag: 'StableElapsed', epoch, at: this.clock.now()}),
            Math.max(0, stableMs - (this.clock.now() - action.welcomedAt)));
          await this.startSession(epoch);
          break;
        case 'Update': await this.receive(action.frame, epoch); break;
        // A lane freed its unit inside a storage step; the queue moves once that step is done.
        case 'SendPending': this.schedulePump(); break;
        case 'ContinueFrame': this.flushRooms(); this.schedulePump(); break;
        case 'RejectReady': this.readyReject(new ClientError(action.code)); break;
        case 'RejectCalls':
          for (const lane of this.lanes.values()) {
            const call = lane.call;
            if (!call) continue;
            lane.call = undefined;
            if (call.overlay) this.overlays.delete(call.overlay);
            call.reject(new ClientError(action.code));
          }
          break;
        case 'Finish':
          for (const lane of this.lanes.values()) if (lane.record.pending) this.finish(lane, {tag: action.outcome});
          break;
        case 'Run': await action.run(); break;
      }
    }
  }
  private helloCredential(): Option<Uint8Array> {
    const bytes = credential(this.phase);
    return bytes ? {tag: 'Some', value: bytes} : {tag: 'None'};
  }
  private openSocket(epoch: number): void {
    let socket: Socket;
    try { socket = (this.options.socket ?? ((url, protocol) => new WebSocket(url, protocol) as unknown as Socket))(this.options.url, 'hirsel.4'); }
    catch { void this.run({tag: 'Transient', epoch}); return; }
    this.socket = socket; socket.binaryType = 'arraybuffer';
    socket.onopen = () => this.post({tag: 'SocketOpen', epoch});
    socket.onmessage = this.inbox.receiver(epoch, this.options.frame, {
      schedule: run => this.post({tag: 'Resume', epoch, run}),
      frame: frame => this.run({tag: 'Frame', frame, epoch}),
      invalid: () => this.incompatible(1007, epoch),
      unobserved: () => this.phase.tag === 'Ready' && !this.options.onBatch && !this.subs.size && !this.overlays.size,
    });
    socket.onclose = ({code}) => { void this.run({tag: 'SocketLost', epoch, code}); };
    socket.onerror = () => { void this.run({tag: 'SocketLost', epoch}); };
  }
  private wakeBackoff(): void {
    if (this.phase.tag === 'Backoff') this.post({tag: 'BackoffElapsed', epoch: this.phase.epoch});
  }
  private incompatible(code: number, epoch = this.phase.epoch): Promise<void> {
    return this.run({tag: 'Incompatible', code, epoch});
  }
  reconnect(): void {
    if (!canReconnect(this.phase)) throw new ClientError(this.state);
    void this.run({tag: 'Reconnect'});
  }
  private async bindWelcome(frame: Extract<Frame, {tag: 'Welcome'}>, epoch: number): Promise<void> {
    const [caller, q, , digest, target, n_place, now, retention] = frame.values;
    if (!validCaller(caller) || !Number.isInteger(q) || q < 0 || !u64(now) || !u64(retention)) { await this.incompatible(1007, epoch); return; }
    this.compareIdentity({digest, target, n_place});
    const previous = this.caller;
    if (previous && !sameCaller(previous, caller) && (previous.issuer !== 1 || caller.issuer !== 1)) {
      await this.fence('LocallyUnknown', undefined, epoch); return;
    }
    if (!sameCaller(previous, caller)) {
      await this.installation(state => this.run({tag: 'Resume', epoch, run: async () => {
        const lanes = [...this.lanes.values()];
        if (previous) {
          // The new Caller owns none of these streams: retire them all and go on with one fresh stream.
          for (const lane of lanes) state.streams[lane.stream].retired = true;
          const [first, ...rest] = lanes, record = emptyStream(caller);
          await this.moveStream(first, state, 'pending-' + hex(randomGeneration(), 32), record, true);
          // Publish the committed stream even if the transport went away during the write.
          this.overlays.clear();
          for (const lane of lanes) if (lane.record.pending) this.finish(lane, {tag: 'LocallyUnknown'});
          for (const lane of rest) await this.dropLane(lane);
          first.record = structuredClone(record);
        } else {
          for (const lane of lanes) state.streams[lane.stream] = {...lane.record, caller};
          await this.options.storage.write(state);
          for (const lane of lanes) lane.record = structuredClone(state.streams[lane.stream]);
        }
        this.caller = caller;
      }}));
    }
    await this.run({tag: 'Resume', epoch, run: () => {
      this.sampleTime(now, retention);
      return this.run({tag: 'WelcomeBound', epoch, at: this.clock.now(), width: 1 + q});
    }});
  }
  private async receive(frame: Extract<Frame, {tag: 'Batch' | 'Reset' | 'Live' | 'Ended'}>, epoch: number): Promise<void> {
    if (frame.tag === 'Batch') {
      const [version, complete, groups, outcomes] = frame.values;
      const byId = new Map(groups.map(group => [group.sub, group]));
      const completing = new Set(complete);
      const staged = new Map<Sub, ReturnType<typeof stageImage>>();
      try {
        for (const sub of this.subs) {
          if (sub.id === undefined) continue;
          const replace = completing.has(sub.id);
          const group = byId.get(sub.id);
          if (!group && !replace) continue;
          if (!group || (!replace && !sub.server)) { await this.incompatible(1008); return; }
          staged.set(sub, stageImage(sub.codec, sub.server, group, replace));
        }
      } catch { await this.incompatible(1007); return; }
      this.notifications = [];
      await this.outcomes(outcomes);
      await this.run({tag: 'AdvanceVersion', epoch, version, apply: async () => {
        for (const [sub, edit] of staged) {
          if (sub.id !== edit.id) { staged.delete(sub); continue; }
          sub.server ??= {value: edit.value!, slots: new Map()};
          applyImage(sub.server, edit);
        }
        this.recompute(staged);
        this.publish();
        await this.run({tag: 'Resume', epoch, run: () => this.options.onBatch?.(version)});
      }});
      this.publish();
    } else if (frame.tag === 'Reset') {
      const [version, outcomes] = frame.values;
      this.clearSubscriptions();
      this.notifications = [];
      await this.outcomes(outcomes);
      await this.run({tag: 'AdvanceVersion', epoch, version, apply: async () => {
        this.recompute();
        this.publish();
        await this.subscribeAll();
      }});
      this.publish();
    } else if (frame.tag === 'Live') {
      for (const part of frame.value) {
        if (part.sub >= this.nextSub) { await this.incompatible(1008); return; }
        const sub = [...this.subs].find(s => s.id === part.sub);
        if (!sub) continue;
        const room = this.rooms.get(sub);
        if (!room) { await this.incompatible(1008); return; }
        let decoded: Decoded;
        try { decoded = room.decode(part); } catch { await this.incompatible(1007); return; }
        room.apply(part, decoded);
      }
    } else if (frame.tag === 'Ended') {
      const [id, reason] = frame.values;
      const sub = [...this.subs].find(s => s.id === id);
      if (sub) {
        this.removeSubscription(sub);
        this.recompute();
        sub.onEnd?.(reason);
      }
    }
  }
  private compareIdentity(actual: Identity): void {
    const expected = this.options.identity;
    if (actual.digest.length === 0) this.options.onDiagnostic?.('identity unknown');
    else if (expected && expected.n_place !== actual.n_place)
      this.options.onDiagnostic?.(`launch identity mismatch: these bindings were generated with n_place ${expected.n_place} and the place serves ${actual.n_place}; regenerate with hirsel sdk ts --n-place ${actual.n_place}`);
    else if (expected && (!equal(expected.digest, actual.digest) || expected.target !== actual.target))
      this.options.onDiagnostic?.('launch identity mismatch');
  }
  private async startSession(epoch: number): Promise<void> {
    for (const lane of [...this.lanes.values()]) {
      // A recovered call whose entry the loaded bindings no longer name has no known kind, so
      // neither its re-send nor its position can be decided: report it unknown and retire its stream.
      const pending = lane.record.pending;
      if (pending && !pending.http && kindOf(this.options, pending.want) === undefined) await this.retire(lane, 'LocallyUnknown');
      else if (lane.remintOnWelcome) {
        lane.remintOnWelcome = false;
        await this.remint(lane, this.keyedAt);
      }
    }
    await this.pump();
    await this.subscribeAll();
    await this.run({tag: 'Resume', epoch, run: () => this.readyResolve()});
  }
  call<R>(want: Want, args: Uint8Array, decode: (bytes: Uint8Array) => R, options?: CallOptions): Promise<Outcome<R>> {
    return this.queueCall(want, args, options)
      .then(outcome => outcome.tag === 'Returned' ? {tag: 'Returned', value: decode(outcome.value)} : outcome);
  }
  /** @internal Used by generated Subscribe bindings to share session recovery. */
  async httpSubscribeCredential(signal: AbortSignal) {
    while (true) {
      signal.throwIfAborted();
      if (credentialUsable(this.state) && (!this.options.credential || credential(this.phase))) break;
      if (this.state !== 'CredentialNeeded' && !credentialUsable(this.state)) throw new ClientError(this.state);
      await new Promise<void>(resolve => {
        const wake = () => {
          this.credentialWaiters.delete(wake);
          signal.removeEventListener('abort', wake);
          resolve();
        };
        this.credentialWaiters.add(wake);
        signal.addEventListener('abort', wake, {once: true});
      });
    }
    const authority = this.httpAuthority;
    return {
      authorization: authorization(credential(this.phase)),
      signal: authority.signal,
      refused: (cause: string, retried: boolean) => this.enqueue(async () => {
        if (signal.aborted || authority.signal.aborted) return;
        await this.run({tag: retried ? 'Unbound' : 'CredentialLost', cause});
      }),
    };
  }

  callHttp(want: Want, request: HttpRequest): Promise<Outcome<HttpReply>> {
    if (!this.options.http || !validHttpRequest(request)) return Promise.reject(new ClientError('InvalidHttpRequest'));
    return this.queueCall(want, new Uint8Array(), undefined, {...request})
      .then(outcome => outcome.tag === 'Returned'
        ? {tag: 'Returned', value: JSON.parse(new TextDecoder().decode(outcome.value)) as HttpReply} : outcome);
  }
  private queueCall(want: Want, args: Uint8Array, options?: CallOptions, http?: HttpRequest): Promise<Outcome<Uint8Array>> {
    const copied = copyWant(want), arg = args.slice(), {optimistic, signal, deadlineMs} = options ?? {};
    if (signal?.aborted) return Promise.reject(signal.reason);
    // Shown on the calling tick when Ready; the queued step still decides whether the call is sent.
    let overlay = optimistic && this.state === 'Ready' && this.wants.some(w => sameWant(w, copied)) ? this.addOverlay(optimistic) : undefined;
    let queued = false;
    return new Promise((resolve, reject) => {
      let timer: unknown;
      const done = () => {
        if (timer !== undefined) this.clock.clear(timer);
        signal?.removeEventListener('abort', cancel);
      };
      const call: QueuedCall = {want: copied, args: arg, slot: 0, http,
        resolve: outcome => { done(); resolve(outcome); }, reject: error => { done(); reject(error); }};
      const cancel = () => void this.enqueue(() => this.drop(call, {error: signal!.reason})).catch(() => {});
      signal?.addEventListener('abort', cancel, {once: true});
      if (deadlineMs !== undefined) {
        call.deadlineAt = this.clock.now() + deadlineMs;
        timer = this.clock.set(() => void this.enqueue(() => this.drop(call, {})).catch(() => {}), deadlineMs);
      }
      void this.enqueue(async () => {
        if (overlay) this.earlyOverlays.delete(overlay);
        if (this.state !== 'Ready') throw new ClientError(this.state);
        call.slot = http ? 0 : this.wants.findIndex(w => sameWant(w, copied));
        if (call.slot < 0) throw new ClientError('UnknownWant');
        const recovered = http && [...this.lanes.values()].find(({record: {pending}}) => pending?.http && sameWant(pending.want, copied)
          && pending.http.path === http.path && pending.http.body === http.body);
        if (recovered) {
          if (recovered.call) throw new ClientError('AlreadySending');
          recovered.call = call;
          await this.pump();
          return;
        }
        overlay ??= optimistic && this.addOverlay(optimistic, true);
        call.overlay = overlay;
        this.calls.add(call); queued = true;
        this.schedulePump();
      }).catch(error => {
        if (!queued && overlay && this.overlays.delete(overlay)) this.recompute();
        call.reject(error);
      });
    });
  }
  // Overlays apply in call order: a queued one goes before later calls' overlays already shown early.
  private addOverlay(run: NonNullable<CallOptions['optimistic']>, queued = false): symbol {
    const id = Symbol(), later = queued ? [...this.overlays].filter(([k]) => this.earlyOverlays.has(k)) : [];
    for (const [k] of later) this.overlays.delete(k);
    this.overlays.set(id, {run, writes: new Set(), done: false});
    for (const [k, v] of later) this.overlays.set(k, v);
    if (queued) { this.recompute(); return id; }
    // A commit holding notifications has queued only call outcomes so far, so the
    // caller's change can go out now without passing any image change.
    const held = this.notifications; this.notifications = undefined;
    this.earlyOverlays.add(id); this.recompute(); this.notifications = held;
    return id;
  }
  // Ends one call by its own deadline (no `error`) or its signal; other calls and lanes go on.
  // A key nothing was stored under is dropped, so the call is refused and its position stays
  // free. Otherwise the lane keeps recovering under its key and the outcome goes to onRecovered.
  private async drop(call: Waiter, why: {error?: unknown}, state?: Installation): Promise<void> {
    const end = (outcome: Outcome<Uint8Array>) => {
      if (call.overlay && this.overlays.delete(call.overlay)) this.recompute();
      if ('error' in why) call.reject(why.error); else call.resolve(outcome);
    };
    if (this.calls.delete(call as QueuedCall)) { end(deadlineRefused); return; }
    const lane = [...this.lanes.values()].find(lane => lane.call === call);
    if (!lane) return;
    const owed = laneOwed(this.phase, lane.id);
    // The place answers an owed send by the deadline it was sent with, which this call's bounds.
    if (owed && !('error' in why)) return;
    const pending = lane.record.pending;
    if (!owed && pending && !pending.inflight && pending.untouched) {
      await this.edit(lane, record => { delete record.pending; }, state);
      if (lane.call !== call) return;
      lane.call = undefined;
      await this.run({tag: 'CallFinished', epoch: this.phase.epoch, lane: lane.id});
      end(deadlineRefused);
    } else {
      lane.call = undefined;
      end({tag: 'LocallyUnknown'});
    }
  }
  private async pump(): Promise<void> {
    if (this.phase.tag !== 'Ready') return;
    const room = this.phase.width - [...this.lanes.values()].filter(lane => lane.record.pending).length;
    const take: QueuedCall[] = [...this.calls].slice(0, Math.max(0, room));
    if (take.length === 0) { await this.sendLanes(); return; }
    // Pending and in-flight are separate durable writes under one installation lock.
    await this.installation(async state => {
      await this.dispatch(state, take);
      await this.sendLanes(state);
    });
  }
  // One pump runs after the work already queued, so calls and answers arriving together share its writes.
  private schedulePump(): void {
    if (!this.pumpQueued) void this.enqueue(() => { this.pumpQueued = false; return this.pump(); }).catch(() => {});
    this.pumpQueued = true;
  }
  private async dispatch(state: Installation, take: readonly QueuedCall[]): Promise<void> {
    if (this.phase.tag !== 'Ready') return;
    for (const call of take) this.calls.delete(call);
    const claimed: Array<{lane: Lane; stream: string; lease: Lease}> = [];
    try {
      const free = [...this.lanes.values()].filter(lane => !lane.record.pending && !lane.call), used: Lane[] = [];
      for (const call of take) {
        const lane = free.shift() ?? await this.addLane(state);
        let record = state.streams[lane.stream];
        if (record.id === undefined) {
          const id = this.mint(this.keyedTime()), stream = streamHex(id);
          record = {...record, id, minter: state.installation};
          record.pending = pendingFor(record, call, state.installation);
          claimed.push({lane, stream, lease: await claimStream(this.options.storage, state, lane.stream, stream, record, false)});
        } else record.pending = pendingFor(record, call, state.installation);
        lane.call = call;
        used.push(lane);
      }
      await this.options.storage.write(state);
      for (const {lane, stream, lease} of claimed.splice(0)) await this.swapLease(lane, stream, lease);
      for (const lane of used) lane.record = structuredClone(state.streams[lane.stream]);
    } catch (error) {
      for (const {lease} of claimed) await lease.release();
      for (const call of take) {
        if (call.overlay && this.overlays.delete(call.overlay)) this.recompute();
        call.reject(error);
      }
      throw error;
    }
  }
  // A new lane takes a free stream of this Caller from the directory, else a new one.
  private async addLane(state: Installation): Promise<Lane> {
    const ours = new Set([...this.lanes.values()].map(lane => lane.stream));
    for (const n of state.directory) {
      const record = state.streams[n];
      if (ours.has(n) || record.retired || record.pending || !sameCaller(record.caller, this.caller)) continue;
      const lease = await this.options.storage.lock(streamLock(state, n), false);
      if (!lease) continue;
      recover(state, record, granted(this.options), lost(this.options));
      return this.addLeased(n, structuredClone(record), lease);
    }
    let stream: string;
    do { stream = 'pending-' + hex(randomGeneration(), 32); } while (Object.hasOwn(state.streams, stream));
    const record = emptyStream(this.caller);
    return this.addLeased(stream, record, await claimStream(this.options.storage, state, undefined, stream, record, true));
  }
  private async sendLanes(state?: Installation): Promise<void> {
    const {epoch} = this.phase, ready: Lane[] = [];
    for (const lane of [...this.lanes.values()]) {
      if (lane.call && expired(lane.call, this.clock.now())) await this.drop(lane.call, {}, state);
      if (!lane.record.pending) continue;
      await this.run({tag: 'PrepareCall', epoch, lane: lane.id, prepare: () => { ready.push(lane); }});
    }
    if (ready.length === 0) return;
    const prepare = (state: Installation) =>
      this.run({tag: 'Resume', epoch, run: () => this.prepareLanes(state, ready, epoch)});
    if (state) await prepare(state);
    else await this.installation(prepare);
  }
  private async prepareLanes(state: Installation, lanes: readonly Lane[], epoch: number): Promise<void> {
    const now = this.keyedTime(), wire = this.options.deadlineMs ?? 30000, margin = futureSlackMs + BigInt(wire) + 1n;
    for (const lane of lanes) {
      let record = state.streams[lane.stream];
      const pending = record.pending!;
      if (pending.inflight) pending.untouched = false;
      if (!record.sent && record.minter === state.installation &&
          now - record.id!.t > this.retention / 2n - margin) {
        const id = this.mint(now);
        record = {...record, id, next: 0n};
        pending.p = 0n;
        await this.moveStream(lane, state, streamHex(id), record);
      }
      record.sent = true;
      pending.inflight = true;
      if (!pending.http) pending.slot = this.wants.findIndex(w => sameWant(w, pending.want));
    }
    await this.options.storage.write(state);
    for (const lane of lanes) {
      const record = lane.record = structuredClone(state.streams[lane.stream]), pending = record.pending!, id = record.id!;
      const delivery = pending.http ? {tag: 'Http', p: pending.p} as const : {tag: 'WebSocket'} as const;
      // Each send carries its own call's remaining time, so the place refuses or abandons it alone.
      const left = lane.call?.deadlineAt === undefined ? wire : Math.ceil(lane.call.deadlineAt - this.clock.now());
      await this.run({tag: 'SendCall', epoch, lane: lane.id, delivery, send: () => {
        if (pending.http) this.startHttp(lane, pending.http, id, pending.p, record.caller!, epoch);
        else this.writeFrame({tag: 'Call', values: [id, pending.p, pending.slot,
          Math.max(1, Math.min(wire, left)), pending.args]}, epoch);
      }});
    }
  }
  private startHttp(lane: Lane, request: HttpRequest, id: StreamId, p: bigint, caller: Caller, epoch: number): void {
    const abort = new AbortController();
    lane.abort = abort;
    const complete = (reply?: HttpAttempt) => this.post({tag: 'HttpComplete', epoch, lane: lane.id, p, complete: async () => {
      lane.abort = undefined;
      if (reply) await this.httpOutcome(lane, p, reply, epoch);
      else await this.retryPending(lane, false, epoch);
    }});
    void sendHttp(this.options.http!, credential(this.phase), request, id, p, caller, abort.signal).then(complete, () => complete());
  }
  private sampleTime(at: bigint, retention: bigint): void {
    this.keyedAt = at;
    this.keyedAtClock = this.clock.now();
    this.retention = retention;
  }
  private keyedTime(): bigint {
    const at = this.keyedAt + BigInt(Math.max(0, Math.floor(this.clock.now() - this.keyedAtClock)));
    return at > maxU64 ? maxU64 : at;
  }
  private mint(t: bigint): StreamId {
    return {t, n: crypto.getRandomValues(new BigUint64Array(1))[0]};
  }
  private async edit(lane: Lane, fn: (record: StreamRecord) => void, state?: Installation): Promise<boolean> {
    const write = async (installation: Installation) => {
      const record = installation.streams[lane.stream];
      fn(record);
      await this.options.storage.write(installation);
      lane.record = structuredClone(record);
      return true;
    };
    return state ? write(state) : !!await this.installation(write);
  }
  private async moveStream(lane: Lane, state: Installation, stream: string, record: StreamRecord, keepOld = false): Promise<void> {
    await this.swapLease(lane, stream, await replaceStream(this.options.storage, state, lane.stream, stream, record, keepOld));
  }
  private async swapLease(lane: Lane, stream: string, lease: Lease): Promise<void> {
    const previous = lane.lease;
    lane.stream = stream;
    lane.lease = lease;
    this.watchLease(lane, lease);
    await previous.release();
  }
  private granted(lane: Lane): boolean {
    const pending = lane.record.pending;
    return !!pending && !pending.http && kindOf(this.options, pending.want) === 'granted';
  }
  // Each attempt these lanes had in flight may have been delivered. A granted one may have run
  // its body, and nothing is stored to dedup a re-send, so its call ends LocallyUnknown and the
  // position stays where it was; any other is re-sent under its key.
  private async noteLost(lanes: readonly Lane[]): Promise<void> {
    const lost = lanes.filter(lane => lane.record.pending?.inflight);
    if (lost.length === 0) return;
    await this.installation(async state => {
      const granted = lost.filter(lane => this.granted(lane));
      for (const lane of lost) {
        const record = state.streams[lane.stream];
        if (granted.includes(lane)) delete record.pending;
        else Object.assign(record.pending!, {inflight: false, untouched: false});
      }
      await this.options.storage.write(state);
      for (const lane of lost) lane.record = structuredClone(state.streams[lane.stream]);
      for (const lane of granted) this.finish(lane, {tag: 'LocallyUnknown'});
    });
  }
  private async remint(lane: Lane, at: bigint): Promise<void> {
    await this.installation(async state => {
      const old = state.streams[lane.stream], pending = old.pending;
      if (!pending?.untouched || pending.remintsLeft !== 1 || old.minter !== state.installation) return;
      const id = this.mint(at);
      const record: StreamRecord = {
        ...emptyStream(old.caller), id, minter: state.installation,
        pending: {...pending, p: 0n, inflight: false, remintsLeft: 0},
      };
      await this.moveStream(lane, state, streamHex(id), record);
      lane.record = structuredClone(record);
      await this.run({tag: 'CallFinished', epoch: this.phase.epoch, lane: lane.id});
    });
  }
  private async httpOutcome(lane: Lane, p: bigint, attempt: HttpAttempt, epoch: number): Promise<void> {
    const pending = lane.record.pending;
    if (pending?.p !== p || !pending.http || !pending.inflight) return;
    const {refusal, at, retention, ...reply} = attempt;
    if (at !== undefined) this.sampleTime(at, retention ?? this.retention);
    const verdict = httpVerdict(reply, refusal);
    switch (verdict.tag) {
      case 'CredentialLost':
        // Nothing was recorded, so the call stays pending, not in flight, for the same Caller
        // after a fresh credential binds.
        await this.edit(lane, record => { record.pending!.inflight = false; });
        await this.run({tag: 'Resume', epoch, run: () => this.run({tag: 'CredentialLost', cause: 'HTTP 401'})});
        return;
      case 'Committed': {
        const outcome = {tag: 'Returned' as const, value: new TextEncoder().encode(JSON.stringify(reply))};
        await this.completeCalls([{lane, outcome, advance: true, http: reply}]);
        break;
      }
      case 'Fenced': await this.fence('LocallyUnknown'); break;
      case 'RequestMismatch': await this.fence('RequestMismatch', lane); break;
      case 'HistoryUnavailable': await this.retire(lane, 'LocallyUnknown'); break;
      case 'Retry': await this.retryPending(lane, verdict.proven); break;
      case 'Refused': await this.finishRefusal(lane, verdict.refusal); break;
    }
    await this.pump();
  }
  // `proven`: the place answered that this delivery was not run, so the call stays untouched.
  private async retryPending(lane: Lane, proven: boolean, epoch = this.phase.epoch): Promise<void> {
    if (!proven) await this.noteLost([lane]);
    else await this.edit(lane, record => { record.pending!.inflight = false; });
    if (!lane.record.pending) return;
    await this.run({tag: 'RetryCall', epoch, lane: lane.id});
    if (lane.call && expired(lane.call, this.clock.now())) await this.drop(lane.call, {});
  }
  // Settles many lanes' answers in one storage write.
  private async completeCalls(done: readonly Completion[]): Promise<void> {
    await this.installation(async state => {
      for (const completion of done) settle(state.streams[completion.lane.stream], completion);
      await this.options.storage.write(state);
      for (const {lane, outcome, http} of done) {
        lane.record = structuredClone(state.streams[lane.stream]);
        if (http && !lane.call && this.options.onHttpRecovered) {
          await this.run({tag: 'CallFinished', epoch: this.phase.epoch, lane: lane.id});
          this.options.onHttpRecovered(http);
        } else this.finish(lane, outcome);
        if (lane.record.retired) await this.dropLane(lane);
      }
    });
  }
  private async finishRefusal(lane: Lane, refusal: Refusal): Promise<void> {
    if (lane.record.pending!.untouched) await this.completeCalls([{lane, outcome: {tag: 'Refused', value: refusal}, advance: false}]);
    else await this.retire(lane, 'LocallyUnknown');
  }
  private async outcomes(rows: Done[]): Promise<void> {
    // Answers that only settle their call share one storage write; the rest step one by one.
    const plain: Completion[] = [], rest: Array<[Lane, Done]> = [];
    for (const row of rows) {
      const lane = owner(this.lanes.values(), row);
      if (!lane || plain.some(done => done.lane === lane)) continue;
      const {outcome} = row;
      // Only a transact call's reply is stored at p (STORE-SPEC §1.8 P1); a compute or
      // granted answer leaves the position for the next stored call.
      if (outcome.tag === 'Returned' || outcome.tag === 'Failed')
        plain.push({lane, outcome, advance: kindOf(this.options, lane.record.pending!.want) === 'transact'});
      else rest.push([lane, row]);
    }
    if (plain.length > 0) await this.completeCalls(plain);
    for (const [lane, row] of rest) {
      if (owner(this.lanes.values(), row) !== lane) continue;
      const pending = lane.record.pending!, outcome = row.outcome;
      if (outcome.tag === 'Abandoned') await this.retryPending(lane, false);
      else if (outcome.tag === 'Refused') {
        const refusal = outcome.value;
        const future = refusal.tag === 'StreamFuture';
        if (refusal.tag === 'RequestMismatch') await this.fence('RequestMismatch', lane);
        else if (refusal.tag === 'HistoryUnavailable' || future) {
          if (!pending.untouched) {
            if (future) await this.retryPending(lane, true);
            else await this.retire(lane, 'LocallyUnknown');
          } else if (pending.remintsLeft === 1 && lane.record.minter === this.installationId) {
            if (future) {
              this.sampleTime(refusal.value, this.retention);
              await this.remint(lane, refusal.value);
            } else {
              lane.remintOnWelcome = true;
              await this.run({tag: 'Transient', epoch: this.phase.epoch});
            }
          } else await this.completeCalls([{lane, outcome, advance: false, retire: true}]);
        } else if (refusal.tag === 'Overloaded' || refusal.tag === 'Deadline' || refusal.tag === 'Budget') {
          await this.retryPending(lane, true);
        } else await this.finishRefusal(lane, refusal);
      } else await this.incompatible(1008);
    }
  }
  private notify(fn: () => void): void {
    if (this.notifications) this.notifications.push(fn); else fn();
  }
  private publish(): void {
    const notifications = this.notifications ?? []; this.notifications = undefined;
    for (const fn of notifications) fn();
  }
  private finish(lane: Lane, outcome: Outcome<Uint8Array>): void {
    void this.run({tag: 'CallFinished', epoch: this.phase.epoch, lane: lane.id});
    const call = lane.call; lane.call = undefined;
    if (call?.overlay) {
      const overlay = this.overlays.get(call.overlay);
      if (outcome.tag === 'Returned' && overlay) overlay.done = true;
      else this.overlays.delete(call.overlay);
    }
    this.notify(() => { if (call) call.resolve(outcome); else this.options.onRecovered?.(outcome); });
  }
  private recompute(staged?: Map<Sub, ReturnType<typeof stageImage>>): void {
    const next: Layer = new Map();
    const diagnostics: string[] = [];
    for (const [id, overlay] of this.overlays) {
      if (overlay.done && [...overlay.writes].every(sub => !sub.active || sub.server)) {
        this.overlays.delete(id);
        continue;
      }
      try { runOverlay(overlay, next, this.handles, code => new ClientError(code)); }
      catch (error) {
        this.overlays.delete(id);
        diagnostics.push(String(error));
      }
    }
    const changes: Array<() => void> = [];
    const affected = new Set([...staged?.keys() ?? [], ...this.layer.keys(), ...next.keys()]);
    for (const sub of affected) {
      const change = commitVisible(sub, staged?.get(sub), this.layer.get(sub), next.get(sub));
      const id = sub.id;
      if (change) changes.push(() => { if (sub.id === id) sub.notify(change); });
    }
    this.layer = next;
    for (const change of changes) this.notify(change);
    for (const diagnostic of diagnostics) this.notify(() => this.options.onDiagnostic?.(diagnostic));
  }
  private stop(state: 'Fenced' | 'Closed', outcome?: 'LocallyUnknown' | 'RequestMismatch'): Promise<void> {
    return this.run({tag: 'Stop', state, outcome});
  }
  // A stream whose history is gone ends its own call and lane; the client and other lanes go on.
  private async retire(lane: Lane, outcome: 'LocallyUnknown'): Promise<void> {
    if (!await this.edit(lane, record => {
      record.retired = true;
      if (record.pending) Object.assign(record.pending, {inflight: false, untouched: false});
    })) return;
    this.finish(lane, {tag: outcome});
    await this.dropLane(lane);
  }
  private async dropLane(lane: Lane): Promise<void> {
    this.lanes.delete(lane.id);
    if (lane.retry !== undefined) this.clock.clear(lane.retry);
    lane.abort?.abort();
    await lane.lease.release();
  }
  // `lane` sent the mismatching request: its call learns why, and every other call is unknown.
  private async fence(outcome: 'LocallyUnknown' | 'RequestMismatch', lane?: Lane, epoch = this.phase.epoch): Promise<void> {
    await this.installation(state => this.run({tag: 'Resume', epoch, run: async () => {
      await this.options.storage.write(fresh(state.installation));
      if (lane?.record.pending && outcome === 'RequestMismatch') {
        this.finish(lane, {tag: outcome});
        delete lane.record.pending;
      }
      await this.stop('Fenced', 'LocallyUnknown');
    }}));
  }
  subscribe(want: Want, args: Uint8Array, onChange: (sub: Subscription, change: Change<Uint8Array, Record<number, SlotRow>>) => void,
    onEnd?: (reason: SubscriptionEnd) => void): Subscription;
  subscribe<V, Slots extends object, Keys extends Record<keyof Slots, unknown>>(want: Want, args: Uint8Array,
    onChange: (sub: Subscription<V, Slots, Keys>, change: Change<V, Slots>) => void,
    onEnd: ((reason: SubscriptionEnd) => void) | undefined,
    decode: SubscriptionCodec<V, Slots, Keys>): Subscription<V, Slots, Keys>;
  subscribe<V, Slots extends object, Keys extends Record<keyof Slots, unknown>>(want: Want, args: Uint8Array,
    onChange: (sub: Subscription<V, Slots, Keys>, change: Change<V, Slots>) => void,
    onEnd?: (reason: SubscriptionEnd) => void, decode?: SubscriptionCodec<V, Slots, Keys>): Subscription<V, Slots, Keys> {
    if (!this.wants.some(w => sameWant(w, want))) throw new ClientError('UnknownWant');
    const codec = (decode ?? rawSubscription) as unknown as Sub['codec'];
    const sub: Sub = {
      want: copyWant(want), args: args.slice(), active: true, codec, onEnd,
      notify: change => onChange(handle, change as Change<V, Slots>),
    };
    const handle = subscriptionHandle<V, Slots, Keys>(sub, codec, () => this.unsubscribe(sub));
    this.handles.set(handle, sub);
    this.subs.add(sub);
    void this.enqueue(async () => this.subscribeAll());
    return handle;
  }
  private flushRooms(): void {
    for (const room of this.rooms.values()) room.flush();
  }
  room<S, M, V, Slots extends object, Keys extends Record<keyof Slots, unknown>>(want: Want, args: Uint8Array,
    handlers: RoomHandlers<S, M, V, Slots, Keys>, decode: SubscriptionCodec<V, Slots, Keys>, live: RoomCodec<S, M>): LiveRoom<S, M, V, Slots, Keys> {
    if (!this.wants.some(w => sameWant(w, want))) throw new ClientError('UnknownWant');
    const codec = decode as unknown as Sub['codec'];
    let handle: LiveRoom<S, M, V, Slots, Keys> | undefined;
    const sub: Sub = {
      want: copyWant(want), args: args.slice(), active: true, codec, onEnd: handlers.onEnd,
      notify: change => handlers.onChange?.(handle!, change as Change<V, Slots>),
    };
    const room = new RoomMember(live as RoomCodec<unknown, unknown>, handlers as never, this.live, sub);
    handle = room.handle(subscriptionHandle<V, Slots, Keys>(sub, codec, () => this.unsubscribe(sub))) as LiveRoom<S, M, V, Slots, Keys>;
    this.handles.set(handle, sub);
    this.rooms.set(sub, room);
    this.subs.add(sub);
    void this.enqueue(async () => this.subscribeAll());
    return handle;
  }
  private clearSubscriptions(): void {
    for (const room of this.rooms.values()) room.end();
    const cleared: Sub[] = [];
    this.layer.clear();
    for (const sub of this.subs) {
      if (sub.image) cleared.push(sub);
      sub.id = undefined; sub.image = sub.server = undefined;
    }
    for (const sub of cleared) sub.notify(invalidation);
  }
  private removeSubscription(sub: Sub): void {
    sub.active = false; sub.id = undefined; sub.image = sub.server = undefined; this.subs.delete(sub);
    this.layer.delete(sub);
    this.rooms.get(sub)?.drop();
    this.rooms.delete(sub);
  }
  private unsubscribe(sub: Sub): void {
    const id = sub.id, {epoch} = this.phase;
    this.removeSubscription(sub);
    void this.enqueue(async () => {
      this.recompute();
      if (id !== undefined && this.state === 'Ready') await this.send({tag: 'Unsubscribe', value: id}, epoch);
    });
  }
  private writeFrame(frame: Frame, epoch: number): void {
    try { this.socket?.send(this.options.frame.encode(frame)); }
    catch { void this.run({tag: 'Transient', epoch}); }
  }
  private async send(frame: Frame, epoch: number): Promise<void> {
    await this.installation(() => this.run({tag: 'Resume', epoch, run: () => this.writeFrame(frame, epoch)}));
  }
  private async subscribeAll(): Promise<void> {
    for (const sub of this.subs) {
      if (this.phase.tag !== 'Ready') return;
      if (!sub.active || sub.id !== undefined) continue;
      if (this.nextSub > 0xffffffff) { await this.incompatible(1008); return; }
      sub.id = this.nextSub++;
      await this.send({tag: 'Subscribe', values: [sub.id, this.wants.findIndex(w => sameWant(w, sub.want)),
        sub.args, {tag: 'None'}]}, this.phase.epoch);
    }
  }
  async close(): Promise<void> {
    this.removeBrowserWake?.(); this.removeBrowserWake = undefined;
    await this.enqueue(async () => {
      await this.stop('Closed', 'LocallyUnknown');
      for (const lane of this.lanes.values()) await lane.lease.release();
    });
  }
}
