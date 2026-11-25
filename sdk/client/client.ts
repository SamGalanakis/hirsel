import {defaultClock, listenForBrowserWake, type Clock, type Socket} from './browser.js';
import {CorruptState, emptyStream, fresh, hex, intact, maxU64, randomGeneration, streamHex, decimalU64, replaceStream,
  streamLock, u64, validCaller, validHttpRequest, type Caller, type HttpRequest, type Installation,
  type Lease, type Pending, type Storage, type StreamId, type StreamRecord} from './persistence.js';
export {CorruptState, RECOVERY_VERSION, intact, validCaller, validHttpRequest} from './persistence.js';
export type {Clock, Socket} from './browser.js';
export type {Caller, HttpRequest, Installation, Lease, Pending, Storage, StreamId, StreamRecord} from './persistence.js';
import {applyImage, commitVisible, equal, invalidation, rawSubscription, runOverlay,
  stageImage, subscriptionHandle, type CallOptions, type Change, type Draft, type ImageEdits, type ImageSlots,
  type Layer, type Overlay, type Sub, type Subscription, type SubscriptionCodec} from './image.js';
import {RoomMember, type Decoded, type LiveLink, type LiveRoom, type RoomCodec, type RoomHandlers} from './room.js';
export type {LiveRoom, PeerId, RoomCodec, RoomHandlers, RoomPeer, RoomPeersChange, ValueCodec} from './room.js';
export type {Access, CallOptions, Change, Draft, Frozen, RowChange, Subscription, SubscriptionCodec, SubscriptionControl} from './image.js';

import type {Codec, Done, Entry, Fault, Frame, Group, Option, Outcome, Part, Refusal, Room, SlotRow, SubscriptionEnd, Want,
  WireOutcome} from './wire.js';
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
export interface HttpReply { status: number; body: string; committed: boolean; replayed: boolean }
interface HttpAttempt extends HttpReply { refusal?: string; at?: bigint; retention?: bigint }
export type State = 'Connecting' | 'Ready' | 'Backoff' | 'Incompatible' | 'Unbound' | 'CredentialNeeded' | 'Fenced' | 'Retired' | 'StorageUnavailable' | 'Closed';
export interface Identity {digest: Uint8Array; target: string; n_place: bigint}
// The generated bindings' `kind`, one per `want`. Only a transact call is stored, so only its
// reply moves the stream position; a granted call's body is never re-sent once it may have run.
export type EntryKind = 'compute' | 'transact' | 'query' | 'granted' | 'room';
export interface Options {
  url: string; storage: Storage; frame: Codec; wants: Want[]; kinds: readonly EntryKind[]; identity?: Identity;
  // Called for the first Hello and once per credential recovery, outside the client's
  // serialized work and storage lock. Undefined bytes mean none can be had.
  credential?: () => Promise<Uint8Array | undefined>;
  http?: { url: string; fetch?: typeof fetch };
  onHttpRecovered?: (reply: HttpReply) => void;
  socket?: (url: string, protocol: string) => Socket; clock?: Clock; deadlineMs?: number;
  onState?: (state: State) => void;
  onDiagnostic?: (diagnostic: string) => void;
  onRecovered?: (outcome: Outcome<Uint8Array>) => void;
  onBatch?: (version: bigint) => void;
}
const sameCaller = (a: Caller | null, b: Caller | null) =>
  a === null || b === null ? a === b : a.issuer === b.issuer && equal(a.subject, b.subject);
const sameWant = (a: Want, b: Want) => equal(a.identity, b.identity) && a.args_fp === b.args_fp && a.result_fp === b.result_fp;
const kindOf = (options: Options, want: Want): EntryKind | undefined => options.kinds[options.wants.findIndex(w => sameWant(w, want))];
const copyWant = (w: Want): Want => ({identity: w.identity.slice(), args_fp: w.args_fp, result_fp: w.result_fp});
const futureSlackMs = 300000n;
const credentialUsable = (state: State) => state === 'Ready' || state === 'Connecting' || state === 'Backoff';
const authorization = (credential: Uint8Array | undefined) =>
  credential && Array.from(credential, b => String.fromCharCode(b)).join('');

export class ClientError extends Error {
  constructor(public readonly code: string) { super(code); this.name = 'ClientError'; }
}
interface QueuedCall {
  want: Want; args: Uint8Array; slot: number; overlay?: symbol; http?: HttpRequest;
  resolve: (outcome: Outcome<Uint8Array>) => void; reject: (error: unknown) => void;
}

export class Client implements Transport {
  state: State = 'Connecting';
  readonly generation: bigint;
  private readonly installationId: bigint;
  private credential?: Uint8Array;
  private httpAuthority = new AbortController();
  private readonly credentialWaiters = new Set<() => void>();
  private refreshing = false;
  private refreshed = false;
  private record: StreamRecord;
  private streamLease: Lease;
  private socket?: Socket;
  private epoch = 0;
  private work: Promise<void> = Promise.resolve();
  private wants: Want[];
  private placed = 0;
  private welcomed = false;
  private welcomedAt = 0;
  private keyedAt = 0n;
  private keyedAtClock = 0;
  private retention = 0n;
  private remintOnWelcome = false;
  private httpFlight?: Promise<HttpAttempt>;
  private backoff = 100;
  private timer?: unknown;
  private stableTimer?: unknown;
  private clock: Clock;
  private removeBrowserWake?: () => void;
  private sent = false;
  private nextSub = 0;
  private subs = new Set<Sub>();
  private rooms = new Map<Sub, RoomMember>();
  private live: LiveLink = {
    ready: () => this.state === 'Ready' && this.socket !== undefined, backlog: () => this.socket?.bufferedAmount ?? 0,
    write: frame => { try { this.socket?.send(this.options.frame.encode(frame)); } catch { this.transient(); } },
    clock: {set: (fn, ms) => this.clock.set(fn, ms), clear: id => this.clock.clear(id)},
  };
  private handles = new WeakMap<object, Sub>();
  private overlays = new Map<symbol, Overlay>();
  private earlyOverlays = new Set<symbol>();
  private layer: Layer = new Map();
  private version = 0n;
  private notifications?: Array<() => void>;
  private calls = new Set<QueuedCall>();
  private activeCall?: Pick<QueuedCall, 'resolve' | 'reject' | 'overlay'>;
  private readyResolve!: () => void;
  private readyReject!: (error: unknown) => void;
  private ready: Promise<void>;

  private constructor(private options: Options, state: Installation, private stream: string, lease: Lease) {
    this.generation = state.generation; this.installationId = state.installation; this.streamLease = lease;
    this.record = structuredClone(state.streams[stream]);
    this.wants = options.wants.map(copyWant);
    this.clock = options.clock ?? defaultClock;
    this.ready = new Promise((resolve, reject) => {this.readyResolve = resolve; this.readyReject = reject;});
    void this.ready.catch(() => {});
    this.watchLease(lease);
  }

  private watchLease(lease: Lease): void {
    void lease.lost.then(() => this.enqueue(async () => {
      if (this.streamLease === lease && this.state !== 'Closed') await this.stop('Fenced', 'LocallyUnknown');
    }));
  }

  static async open(options: Options): Promise<Client> {
    if (options.kinds.length !== options.wants.length) throw new ClientError('InvalidBindings');
    const lock = await options.storage.lock('install', true);
    if (!lock) throw new ClientError('StorageUnavailable');
    let lease: Lease | null = null;
    try {
      let state: Installation | null;
      try { state = await options.storage.read(); }
      catch (error) {
        if (!(error instanceof CorruptState)) throw error;
        state = null;
      }
      if (!state || !intact(state)) {
        state = fresh(state?.installation);
        await options.storage.write(state);
        options.onRecovered?.({tag: 'LocallyUnknown'});
      }
      let stream: string | undefined;
      for (const n of state.directory) {
        if (state.streams[n].retired) continue;
        lease = await options.storage.lock(streamLock(state, n), false);
        if (lease) { stream = n; break; }
      }
      if (stream === undefined) {
        do { stream = 'pending-' + hex(randomGeneration(), 32); } while (Object.hasOwn(state.streams, stream));
        lease = await options.storage.lock(streamLock(state, stream), false);
        if (!lease) throw new ClientError('StreamLeaseUnavailable');
        state.directory.push(stream);
        state.streams[stream] = emptyStream();
        await options.storage.write(state);
      }
      const record = state.streams[stream];
      const imported = record.id !== undefined && record.minter !== state.installation;
      if (record.pending?.inflight || imported) {
        // A granted body runs once per delivery and nothing is stored for it, so an attempt
        // that may have been delivered is reported LocallyUnknown and never re-sent.
        if (record.pending?.inflight && !record.pending.http && kindOf(options, record.pending.want) === 'granted') {
          delete record.pending;
          options.onRecovered?.({tag: 'LocallyUnknown'});
        } else if (record.pending) {
          record.pending.inflight = false;
          record.pending.untouched = false;
        }
        if (imported) {
          record.minter = null;
          record.sent = true;
        }
        await options.storage.write(state);
      }
      const client = new Client(options, state, stream, lease!);
      await lock.release();
      client.connect();
      client.removeBrowserWake = listenForBrowserWake(() => client.wakeBackoff());
      return client;
    } catch (error) {
      await lease?.release(); await lock.release(); throw error;
    }
  }

  whenReady(): Promise<void> { return this.ready; }
  private enqueue(fn: () => Promise<void>): Promise<void> {
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
    if (this.state === 'Closed') return;
    this.overlays.clear();
    this.cancelTimers(); this.disconnect(); this.setState('StorageUnavailable');
    this.options.onDiagnostic?.(String(error));
    this.readyReject(new ClientError('StorageUnavailable'));
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
      const stream = state.streams[this.stream];
      if (state.installation !== this.installationId || state.generation !== this.generation ||
          !stream || !sameCaller(stream.caller, this.record.caller)) {
        await this.stop('Fenced', 'LocallyUnknown'); return;
      }
      return await fn(state);
    } finally { await lock.release(); }
  }
  private connect(): void {
    if (['Closed', 'Fenced', 'Retired', 'Incompatible', 'Unbound', 'CredentialNeeded', 'StorageUnavailable'].includes(this.state)) return;
    if (this.options.credential && !this.credential) { this.setState('Connecting'); this.refresh(undefined); return; }
    this.setState('Connecting'); this.welcomed = false; this.placed = 0; this.sent = false;
    this.clearSubscriptions();
    const epoch = ++this.epoch;
    let socket: Socket;
    try { socket = (this.options.socket ?? ((url, protocol) => new WebSocket(url, protocol) as unknown as Socket))(this.options.url, 'hirsel.4'); }
    catch { this.transient(); return; }
    this.socket = socket; socket.binaryType = 'arraybuffer';
    socket.onopen = () => { void this.enqueue(async () => {
      if (epoch !== this.epoch) return;
      await this.installation(async () => {
        const credential: Option<Uint8Array> = this.credential ? {tag: 'Some', value: this.credential} : {tag: 'None'};
        try { socket.send(this.options.frame.encode({tag: 'Hello', values: [credential, this.wants]})); }
        catch { this.transient(); }
      });
    }); };
    socket.onmessage = event => { void this.enqueue(async () => {
      if (epoch !== this.epoch) return;
      let frame: Frame;
      try {
        if (!(event.data instanceof ArrayBuffer) && !(event.data instanceof Uint8Array)) throw new Error('binary Frame required');
        frame = this.options.frame.decode(event.data instanceof Uint8Array ? event.data : new Uint8Array(event.data));
      } catch { await this.incompatible(1007); return; }
      await this.receive(frame, epoch);
      if (epoch === this.epoch) { this.flushRooms(); await this.startNextCall(); }
    }); };
    const lost = (finish: () => Promise<void>) => {
      if (epoch !== this.epoch) return;
      const lossEpoch = ++this.epoch;
      void this.enqueue(async () => {
        if (lossEpoch !== this.epoch) return;
        await this.noteLostAttempt();
        if (lossEpoch === this.epoch) await finish();
      });
      this.clearSubscriptions();
    };
    socket.onclose = event => lost(async () => {
      // 4401 AuthExpired ends an established session; 1008 before Welcome is the binding
      // refusing this Hello's credential, which is how an expiry reached offline shows.
      if (event.code === 4401) await this.credentialLost('Close 4401');
      else if (event.code === 1008 && !this.welcomed) await this.credentialLost('Close 1008 before Welcome');
      else if (event.code === 1009) await this.incompatible(1009,
        !this.welcomed && event.reason === 'hirsel.4' ? 'Reload' : 'ReplyTooLarge');
      else if ([1007, 1008].includes(event.code)) await this.incompatible(event.code);
      else this.transient();
    });
    socket.onerror = () => lost(async () => this.transient());
  }
  private disconnect(): void {
    ++this.epoch; const socket = this.socket; this.socket = undefined;
    this.clearSubscriptions();
    socket?.close();
  }
  private cancelTimers(): void {
    if (this.timer !== undefined) this.clock.clear(this.timer);
    if (this.stableTimer !== undefined) this.clock.clear(this.stableTimer);
    this.timer = undefined; this.stableTimer = undefined;
  }
  private transient(): void {
    this.cancelTimers(); this.disconnect();
    const delay = this.clock.random() * this.backoff;
    this.backoff = Math.min(10000, this.backoff * 2);
    this.timer = this.clock.set(() => this.wakeBackoff(), delay);
    this.setState('Backoff');
  }
  private wakeBackoff(): void {
    if (this.state !== 'Backoff' || this.timer === undefined) return;
    this.clock.clear(this.timer);
    this.timer = undefined;
    const epoch = this.epoch;
    void this.enqueue(async () => {
      if (epoch === this.epoch && this.state === 'Backoff') this.connect();
    });
  }
  private async incompatible(code: number, problem = 'Incompatible'): Promise<void> {
    this.cancelTimers(); this.disconnect(); this.setState('Incompatible');
    const diagnostic = problem === 'Reload' ? 'Reload required before reconnecting to hirsel.4'
      : problem === 'ReplyTooLarge' ? 'Stored reply exceeds the server message limit (1009)'
      : `Incompatible (${code}); reload bindings or reconnect explicitly`;
    this.options.onDiagnostic?.(diagnostic);
    if (problem === 'ReplyTooLarge') {
      if (this.activeCall?.overlay) this.overlays.delete(this.activeCall.overlay);
      this.activeCall?.reject(new ClientError(problem));
      this.activeCall = undefined;
    }
    this.readyReject(new ClientError(problem));
  }
  private async unbound(cause: string): Promise<void> {
    this.cancelTimers(); this.disconnect(); this.credential = undefined; this.setState('Unbound');
    this.options.onDiagnostic?.(this.options.credential
      ? `Unbound: the place refused the credential and one refresh did not recover (${cause})`
      : `Unbound: the place binds callers and needs a credential; pass \`credential\` to Client.open (${cause})`);
    this.readyReject(new ClientError('Unbound'));
  }
  // A refused or expired credential is dropped at once, so no path sends it again. Each
  // recovery attempt gets one refresh; WS and HTTP refusals join the refresh in flight.
  // Pending work stays durable and is replayed only after Welcome names the same Caller.
  private async credentialLost(cause: string): Promise<void> {
    if (this.state === 'CredentialNeeded') return;
    const refused = this.credential;
    if (!this.options.credential || this.refreshed) { await this.unbound(cause); return; }
    this.cancelTimers(); this.disconnect(); this.credential = undefined;
    this.refreshed = true; this.setState('CredentialNeeded');
    this.options.onDiagnostic?.(`CredentialNeeded: the place refused the session credential (${cause}); refreshing once`);
    this.refresh(refused);
  }
  private refresh(refused: Uint8Array | undefined): void {
    if (this.refreshing) return;
    this.refreshing = true;
    const source = this.options.credential!, epoch = this.epoch;
    const settled = (fresh: Uint8Array | undefined) => this.enqueue(async () => {
      this.refreshing = false;
      // A result after close, fencing or a newer attempt is discarded.
      if (epoch !== this.epoch || (this.state !== 'CredentialNeeded' && this.state !== 'Connecting')) return;
      if (!fresh || (refused && equal(fresh, refused))) {
        await this.unbound(fresh ? 'the refresh returned the refused credential' : 'the credential source gave none'); return;
      }
      this.credential = fresh.slice(); this.setState('Connecting'); this.connect();
    });
    void Promise.resolve().then(source).then(settled, () => settled(undefined));
  }
  reconnect(): void {
    if (this.state !== 'Incompatible' && this.state !== 'Unbound' && this.state !== 'Backoff') throw new ClientError(this.state);
    this.cancelTimers(); this.refreshed = false; this.setState('Connecting');
    this.ready = new Promise((resolve, reject) => {this.readyResolve = resolve; this.readyReject = reject;});
    void this.ready.catch(() => {});
    void this.enqueue(async () => this.connect());
  }
  private async receive(frame: Frame, epoch: number): Promise<void> {
    if (frame.tag === 'Welcome') {
      if (this.welcomed) { await this.incompatible(1008); return; }
      const [caller, , , digest, target, n_place, now, retention] = frame.values;
      if (!validCaller(caller) || !u64(now) || !u64(retention)) { await this.incompatible(1007); return; }
      this.compareIdentity({digest, target, n_place});
      const previous = this.record.caller;
      if (previous && !sameCaller(previous, caller) && (previous.issuer !== 1 || caller.issuer !== 1)) {
        await this.fence('LocallyUnknown'); return;
      }
      if (!sameCaller(previous, caller)) {
        await this.installation(async state => {
          const record = previous ? emptyStream(caller) : {...this.record, caller};
          if (previous) {
            const prior = state.streams[this.stream];
            prior.retired = true;
            const stream = 'pending-' + hex(randomGeneration(), 32);
            await this.moveStream(state, stream, record, true);
          } else {
            state.streams[this.stream] = record;
            await this.options.storage.write(state);
          }
          if (previous) this.overlays.clear();
          if (this.record.pending) this.finish({tag: 'LocallyUnknown'});
          this.record = structuredClone(record);
        });
        if (epoch !== this.epoch || this.state !== 'Connecting') return;
      }
      this.welcomed = true; this.welcomedAt = this.clock.now(); this.refreshed = false;
      this.sampleTime(now, retention);
      if (this.wants.length === 0) await this.allPlaced();
      return;
    }
    if (frame.tag === 'Placed') {
      if (!this.welcomed || this.placed >= this.wants.length) { await this.incompatible(1008); return; }
      if (frame.value.tag === 'Gone') { await this.incompatible(1008); return; }
      this.placed++;
      if (this.placed === this.wants.length) await this.allPlaced();
      return;
    }
    if (this.state !== 'Ready') { await this.incompatible(1008); return; }
    if (frame.tag === 'Batch') {
      const [version, complete, groups, outcomes] = frame.values;
      if (version < this.version) { await this.incompatible(1008); return; }
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
      if (epoch !== this.epoch) { this.publish(); return; }
      this.version = version;
      for (const [sub, edit] of staged) {
        if (sub.id !== edit.id) { staged.delete(sub); continue; }
        sub.server ??= {value: edit.value!, slots: new Map()};
        applyImage(sub.server, edit);
      }
      this.recompute(staged);
      this.publish();
      if (epoch === this.epoch) this.options.onBatch?.(version);
    } else if (frame.tag === 'Reset') {
      const [version, outcomes] = frame.values;
      if (version < this.version) { await this.incompatible(1008); return; }
      this.clearSubscriptions();
      this.notifications = [];
      await this.outcomes(outcomes);
      if (epoch !== this.epoch) { this.publish(); return; }
      this.version = version;
      this.recompute();
      this.publish(); await this.subscribeAll();
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
    } else await this.incompatible(1008);
  }
  private compareIdentity(actual: Identity): void {
    const expected = this.options.identity;
    if (actual.digest.length === 0) this.options.onDiagnostic?.('identity unknown');
    else if (expected && expected.n_place !== actual.n_place)
      this.options.onDiagnostic?.(`launch identity mismatch: these bindings were generated with n_place ${expected.n_place} and the place serves ${actual.n_place}; regenerate with hirsel sdk ts --n-place ${actual.n_place}`);
    else if (expected && (!equal(expected.digest, actual.digest) || expected.target !== actual.target))
      this.options.onDiagnostic?.('launch identity mismatch');
  }
  private async allPlaced(): Promise<void> {
    this.setState('Ready'); this.version = 0n;
    this.stableTimer = this.clock.set(() => {
      if (this.state === 'Ready' && this.clock.now() - this.welcomedAt >= 30000) this.backoff = 100;
    }, Math.max(0, 30000 - (this.clock.now() - this.welcomedAt)));
    // A recovered call whose entry the loaded bindings no longer name has no known kind, so
    // neither its re-send nor its position can be decided: report it unknown on a fresh stream.
    const pending = this.record.pending;
    if (pending && !pending.http && kindOf(this.options, pending.want) === undefined) {
      await this.retire('LocallyUnknown'); return;
    }
    if (this.remintOnWelcome) {
      this.remintOnWelcome = false;
      await this.remint(this.keyedAt);
    }
    await this.sendPending(); await this.subscribeAll(); this.readyResolve();
  }
  call<R>(want: Want, args: Uint8Array, decode: (bytes: Uint8Array) => R, options?: CallOptions): Promise<Outcome<R>> {
    return this.queueCall(want, args, options?.optimistic)
      .then(outcome => outcome.tag === 'Returned' ? {tag: 'Returned', value: decode(outcome.value)} : outcome);
  }
  /** @internal Used by generated Subscribe bindings to share session recovery. */
  async httpSubscribeCredential(signal: AbortSignal) {
    while (true) {
      signal.throwIfAborted();
      if (credentialUsable(this.state) && (!this.options.credential || this.credential)) break;
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
      authorization: authorization(this.credential),
      signal: authority.signal,
      refused: (cause: string, retried: boolean) => this.enqueue(async () => {
        if (signal.aborted || authority.signal.aborted) return;
        if (retried) await this.unbound(cause);
        else await this.credentialLost(cause);
      }),
    };
  }

  callHttp(want: Want, request: HttpRequest): Promise<Outcome<HttpReply>> {
    if (!this.options.http || !validHttpRequest(request)) return Promise.reject(new ClientError('InvalidHttpRequest'));
    return this.queueCall(want, new Uint8Array(), undefined, {...request})
      .then(outcome => outcome.tag === 'Returned'
        ? {tag: 'Returned', value: JSON.parse(new TextDecoder().decode(outcome.value)) as HttpReply} : outcome);
  }
  private queueCall(want: Want, args: Uint8Array, optimistic?: CallOptions['optimistic'], http?: HttpRequest): Promise<Outcome<Uint8Array>> {
    const copied = copyWant(want), arg = args.slice();
    // Shown on the calling tick when Ready; the queued step still decides whether the call is sent.
    let overlay = optimistic && this.state === 'Ready' && this.wants.some(w => sameWant(w, copied)) ? this.addOverlay(optimistic) : undefined;
    let queued = false;
    return new Promise((resolve, reject) => {
      void this.enqueue(async () => {
        if (overlay) this.earlyOverlays.delete(overlay);
        if (this.state !== 'Ready') throw new ClientError(this.state);
        const slot = http ? 0 : this.wants.findIndex(w => sameWant(w, copied));
        if (slot < 0) throw new ClientError('UnknownWant');
        if (http && this.record.pending?.http && sameWant(this.record.pending.want, copied)
          && this.record.pending.http.path === http.path && this.record.pending.http.body === http.body) {
          if (this.activeCall) throw new ClientError('AlreadySending');
          this.activeCall = {resolve, reject};
          await this.sendPending();
          return;
        }
        overlay ??= optimistic && this.addOverlay(optimistic, true);
        this.calls.add({want: copied, args: arg, slot, overlay, http, resolve, reject}); queued = true;
        await this.startNextCall();
      }).catch(error => {
        if (!queued && overlay && this.overlays.delete(overlay)) this.recompute();
        reject(error);
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
  private async startNextCall(): Promise<void> {
    if (this.state !== 'Ready' || this.record.pending) return;
    const call = this.calls.values().next().value;
    if (!call) return;
    this.calls.delete(call);
    try {
      await this.installation(async state => {
        let record = state.streams[this.stream];
        const mint = record.id === undefined;
        if (mint) record = {...record, id: this.mint(this.keyedTime()), minter: state.installation};
        const pending: Pending = {p: record.next, want: call.want, args: call.args, slot: call.slot, inflight: false,
          untouched: record.minter === state.installation, remintsLeft: 1, ...(call.http && {http: call.http})};
        record.pending = pending;
        if (mint) await this.moveStream(state, streamHex(record.id!), record);
        else await this.options.storage.write(state);
        this.record = structuredClone(record);
        this.activeCall = {resolve: call.resolve, reject: call.reject, overlay: call.overlay};
      });
      if (this.state !== 'Ready') throw new ClientError(this.state);
      await this.sendPending();
    } catch (error) {
      if (call.overlay && this.overlays.delete(call.overlay)) this.recompute();
      call.reject(error);
      throw error;
    }
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
  private async moveStream(state: Installation, stream: string, record: StreamRecord, keepOld = false): Promise<void> {
    const lease = await replaceStream(this.options.storage, state, this.stream, stream, record, keepOld);
    const previous = this.streamLease;
    this.stream = stream;
    this.streamLease = lease;
    this.watchLease(lease);
    await previous.release();
  }
  private granted(): boolean {
    const pending = this.record.pending;
    return !!pending && !pending.http && kindOf(this.options, pending.want) === 'granted';
  }
  // A granted call's attempt may have run its body, and nothing is stored to dedup a
  // re-send, so the call ends LocallyUnknown and the position stays where it was.
  private async loseGranted(): Promise<void> {
    await this.installation(async state => {
      const record = state.streams[this.stream];
      delete record.pending;
      await this.options.storage.write(state);
      this.record = structuredClone(record);
      this.finish({tag: 'LocallyUnknown'});
    });
  }
  private async noteLostAttempt(): Promise<void> {
    if (!this.record.pending?.inflight) return;
    if (this.granted()) { await this.loseGranted(); return; }
    await this.installation(async state => {
      const record = state.streams[this.stream], pending = record.pending!;
      pending.inflight = false;
      pending.untouched = false;
      await this.options.storage.write(state);
      this.record = structuredClone(record);
    });
  }
  private async remint(at: bigint): Promise<void> {
    await this.installation(async state => {
      const old = state.streams[this.stream], pending = old.pending;
      if (!pending?.untouched || pending.remintsLeft !== 1 || old.minter !== state.installation) return;
      const id = this.mint(at);
      const record: StreamRecord = {
        ...emptyStream(old.caller), id, minter: state.installation,
        pending: {...pending, p: 0n, inflight: false, remintsLeft: 0},
      };
      await this.moveStream(state, streamHex(id), record);
      this.record = structuredClone(record);
      this.sent = false;
    });
  }
  private async sendPending(): Promise<void> {
    if (this.state !== 'Ready' || !this.record.pending || this.sent || this.httpFlight) return;
    await this.installation(async state => {
      let record = state.streams[this.stream];
      const pending = record.pending!;
      if (pending.inflight) pending.untouched = false;
      const now = this.keyedTime();
      const margin = futureSlackMs + BigInt(this.options.deadlineMs ?? 30000) + 1n;
      if (!record.sent && record.minter === state.installation &&
          now - record.id!.t > this.retention / 2n - margin) {
        const id = this.mint(now);
        record = {...record, id, next: 0n};
        pending.p = 0n;
        await this.moveStream(state, streamHex(id), record);
      }
      record.sent = true;
      pending.inflight = true;
      if (!pending.http) pending.slot = this.wants.findIndex(w => sameWant(w, pending.want));
      const epoch = this.epoch;
      await this.options.storage.write(state);
      this.record = structuredClone(record);
      this.sent = true;
      if (epoch !== this.epoch) return;
      if (pending.http) {
        const flight = this.sendHttp(pending.http, record.id!, pending.p, record.caller!);
        this.httpFlight = flight;
        void flight.then(
          reply => this.enqueue(() => this.httpComplete(epoch, pending.p, reply)),
          () => this.enqueue(() => this.httpComplete(epoch, pending.p)),
        ).catch(() => {});
      } else {
        try { this.socket!.send(this.options.frame.encode({tag: 'Call', values: [record.id!, pending.p, pending.slot, this.options.deadlineMs ?? 30000, pending.args]})); }
        catch { this.transient(); }
      }
    });
  }
  private async httpComplete(epoch: number, p: bigint, reply?: HttpAttempt): Promise<void> {
    this.httpFlight = undefined;
    if (epoch !== this.epoch || this.state !== 'Ready') {
      await this.sendPending();
    } else if (reply) await this.httpOutcome(p, reply);
    else await this.retryPending(false);
  }
  private async sendHttp(request: HttpRequest, id: StreamId, p: bigint, caller: Caller): Promise<HttpAttempt> {
    const options = this.options.http;
    if (!options) throw new ClientError('HttpUnavailable');
    const headers = new Headers();
    const auth = authorization(this.credential);
    if (auth !== undefined) headers.set('Authorization', auth);
    if (request.body !== undefined) headers.set('Content-Type', 'application/json');
    const key = `"${streamHex(id)}.${p}"`;
    headers.set('Idempotency-Key', key);
    headers.set('Hirsel-Expected-Caller', btoa(String.fromCharCode(caller.issuer, caller.subject.length, ...caller.subject)));
    const send = options.fetch ?? globalThis.fetch;
    const result = await send(new URL(request.path, options.url), {
      method: 'POST', headers, body: request.body, redirect: 'error',
    });
    const marker = /^([a-z_]+);key=(.+)$/.exec(result.headers.get('Hirsel-Refused') ?? '');
    return {status: result.status, body: await result.text(),
      committed: result.headers.get('Idempotent-Outcome') === 'committed',
      replayed: result.headers.get('Idempotent-Replayed') === 'true',
      refusal: marker?.[2] === key ? marker[1] : undefined,
      at: decimalU64(result.headers.get('Hirsel-Now')),
      retention: decimalU64(result.headers.get('Hirsel-Retention'))};
  }
  private async httpOutcome(p: bigint, attempt: HttpAttempt): Promise<void> {
    const pending = this.record.pending;
    if (pending?.p !== p || !pending.http || !pending.inflight) return;
    const {refusal, at, retention, ...reply} = attempt;
    if (at !== undefined) this.sampleTime(at, retention ?? this.retention);
    // 401: the binding refused or expired the credential before anything was recorded, so the
    // call stays pending, not in flight, for the same Caller after a fresh credential binds.
    if (!reply.committed && reply.status === 401) {
      await this.installation(async state => {
        const record = state.streams[this.stream];
        record.pending!.inflight = false;
        await this.options.storage.write(state);
        this.record = structuredClone(record);
      });
      this.sent = false;
      await this.credentialLost('HTTP 401');
      return;
    }
    if (reply.committed) {
      const outcome = {tag: 'Returned' as const, value: new TextEncoder().encode(JSON.stringify(reply))};
      await this.completeCall(outcome, true, false, reply);
    } else if (reply.status === 403 && reply.body === '{"refused":"fenced"}') {
      await this.fence('LocallyUnknown');
    } else if (reply.status === 422 && refusal === 'request_mismatch') {
      await this.fence('RequestMismatch');
    } else if (reply.status === 410 && refusal === 'history_unavailable') {
      // This marker proves only the delivery it answers. A proxy or a supplied
      // fetch implementation may already have sent this key before that delivery.
      await this.retire('LocallyUnknown');
    } else if (reply.status === 409) {
      await this.retryPending(false);
    } else if ((reply.status === 503 && (refusal === 'overloaded' || refusal === 'budget')) ||
        (reply.status === 504 && refusal === 'deadline')) {
      await this.retryPending(true);
    } else if (reply.status === 404 && refusal === 'no_entry') {
      await this.finishRefusal({tag: 'NoEntry'});
    } else if (reply.status === 403 && refusal === 'not_allowed') {
      await this.finishRefusal({tag: 'NotAllowed'});
    } else if (reply.status === 400 && refusal === 'malformed') {
      const malformed = /^\{"refused":"malformed","kind":([0-7])\}$/.exec(reply.body);
      if (!malformed) await this.retryPending(false);
      else if (malformed[1] === '7') await this.retryPending(false);
      else await this.finishRefusal({tag: 'Malformed', value: {kind: Number(malformed[1]), at: at ?? 0n}});
    } else {
      await this.retryPending(false);
    }
    await this.startNextCall();
  }
  private async retryPending(proven: boolean): Promise<void> {
    if (!proven) await this.noteLostAttempt();
    else await this.installation(async state => {
      const record = state.streams[this.stream];
      record.pending!.inflight = false;
      await this.options.storage.write(state);
      this.record = structuredClone(record);
    });
    if (this.state !== 'Ready') return;
    if (this.timer !== undefined) this.clock.clear(this.timer);
    this.timer = this.clock.set(() => {
      this.timer = undefined;
      void this.enqueue(async () => {
        this.sent = false;
        await this.sendPending();
      });
    }, this.clock.random() * this.backoff);
    this.backoff = Math.min(10000, this.backoff * 2);
  }
  private async completeCall(outcome: Outcome<Uint8Array>, advance: boolean, retire: boolean, http?: HttpReply): Promise<void> {
    await this.installation(async state => {
      const record = state.streams[this.stream], pending = record.pending!;
      record.terminal = {p: pending.p, outcome: structuredClone(outcome)};
      delete record.pending;
      if (advance && pending.p !== maxU64) record.next = pending.p + 1n;
      if (retire || (advance && pending.p === maxU64)) record.retired = true;
      await this.options.storage.write(state);
      this.record = structuredClone(record);
      if (http && !this.activeCall && this.options.onHttpRecovered) {
        this.sent = false;
        this.options.onHttpRecovered(http);
      } else this.finish(outcome);
      if (record.retired) await this.stop('Retired');
    });
  }
  private async finishRefusal(refusal: Refusal): Promise<void> {
    if (this.record.pending!.untouched) await this.completeCall({tag: 'Refused', value: refusal}, false, false);
    else await this.retire('LocallyUnknown');
  }
  private async outcomes(rows: Done[]): Promise<void> {
    for (const row of rows) {
      const pending = this.record.pending, id = this.record.id;
      if (!pending?.inflight || pending.http || row.stream.t !== id?.t || row.stream.n !== id.n || row.p !== pending.p) continue;
      const outcome = row.outcome;
      if (outcome.tag === 'Returned' || outcome.tag === 'Failed') {
        // Only a transact call's reply is stored at p (STORE-SPEC §1.8 P1); a compute or
        // granted answer leaves the position for the next stored call.
        await this.completeCall(outcome, kindOf(this.options, pending.want) === 'transact', false);
      } else if (outcome.tag === 'Abandoned') {
        if (this.granted()) await this.loseGranted();
        else await this.retryPending(false);
      } else if (outcome.tag === 'Refused') {
        const refusal = outcome.value;
        const future = refusal.tag === 'Malformed' && refusal.value.kind === 7;
        if (refusal.tag === 'RequestMismatch') await this.fence('RequestMismatch');
        else if (refusal.tag === 'HistoryUnavailable' || future) {
          if (!pending.untouched) {
            if (future) await this.retryPending(true);
            else await this.retire('LocallyUnknown');
          } else if (pending.remintsLeft === 1 && this.record.minter === this.installationId) {
            if (future) {
              this.sampleTime(refusal.value.at, this.retention);
              await this.remint(refusal.value.at);
              await this.sendPending();
            } else {
              this.remintOnWelcome = true;
              this.transient();
            }
          } else await this.completeCall(outcome, false, true);
        } else if (refusal.tag === 'Overloaded' || refusal.tag === 'Deadline' || refusal.tag === 'Budget') {
          await this.retryPending(true);
        } else await this.finishRefusal(refusal);
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
  private finish(outcome: Outcome<Uint8Array>): void {
    this.sent = false;
    const call = this.activeCall; this.activeCall = undefined;
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
  private async stop(state: 'Fenced' | 'Retired' | 'Closed', outcome?: 'LocallyUnknown' | 'RequestMismatch'): Promise<void> {
    this.overlays.clear();
    this.cancelTimers(); this.disconnect(); this.setState(state);
    if (outcome && this.record.pending) this.finish({tag: outcome});
    this.readyReject(new ClientError(state));
  }
  private async retire(outcome: 'LocallyUnknown'): Promise<void> {
    await this.installation(async state => {
      const record = state.streams[this.stream];
      record.retired = true;
      record.pending!.inflight = false;
      record.pending!.untouched = false;
      await this.options.storage.write(state);
      this.record = structuredClone(record);
      await this.stop('Retired', outcome);
    });
  }
  private async fence(outcome: 'LocallyUnknown' | 'RequestMismatch'): Promise<void> {
    await this.installation(async state => {
      await this.options.storage.write(fresh(state.installation));
      await this.stop('Fenced', outcome);
    });
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
    const id = sub.id;
    this.removeSubscription(sub);
    void this.enqueue(async () => {
      this.recompute();
      if (id !== undefined && this.state === 'Ready') await this.send({tag: 'Unsubscribe', value: id});
    });
  }
  private async send(frame: Frame): Promise<void> {
    await this.installation(async () => {
      try { this.socket?.send(this.options.frame.encode(frame)); } catch { this.transient(); }
    });
  }
  private async subscribeAll(): Promise<void> {
    if (this.state !== 'Ready') return;
    for (const sub of this.subs) {
      if (this.state !== 'Ready') return;
      if (!sub.active || sub.id !== undefined) continue;
      if (this.nextSub > 0xffffffff) { await this.incompatible(1008); return; }
      sub.id = this.nextSub++;
      await this.send({tag: 'Subscribe', values: [sub.id, this.wants.findIndex(w => sameWant(w, sub.want)), sub.args, {tag: 'None'}]});
    }
  }
  async close(): Promise<void> {
    this.removeBrowserWake?.();
    this.removeBrowserWake = undefined;
    await this.enqueue(async () => { await this.stop('Closed', 'LocallyUnknown'); await this.streamLease.release(); });
  }
}
