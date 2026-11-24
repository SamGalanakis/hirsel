export type Option<T> = {tag: 'None'} | {tag: 'Some'; value: T};
export interface Want { identity: Uint8Array; args_fp: bigint; result_fp: bigint }
export interface Room { requested: bigint; available: bigint }
export type Fault = {tag: 'Raised'; value: number} | {tag: 'OutOfBytes'; value: Room}
  | {tag: 'OutOfTime'; value: bigint} | {tag: 'Gone'} | {tag: 'Conflict'; value: string};
export type Refusal = {tag: 'NoEntry' | 'Overloaded' | 'Deadline' | 'NotAllowed' | 'RequestMismatch' | 'HistoryUnavailable'}
  | {tag: 'Malformed'; value: {kind: number; at: bigint}} | {tag: 'Budget'; value: Room};
type WireOutcome<R> = {tag: 'Returned'; value: R} | {tag: 'Failed'; value: Fault}
  | {tag: 'Refused'; value: Refusal} | {tag: 'Abandoned' | 'InDoubt'};
export type Outcome<R> = WireOutcome<R> | {tag: 'LocallyUnknown' | 'RequestMismatch'};
export interface SlotRow { slot: number; key: Uint8Array; value: Option<Uint8Array> }
export interface Group { sub: number; value: Uint8Array; rows: SlotRow[] }
export interface Done {stream: number; p: bigint; outcome: Outcome<Uint8Array>}
export type Entry = {tag: 'Ready'; value: number} | {tag: 'Resolve'; values: [number, Uint8Array]} | {tag: 'Gone'};
export interface Caller { issuer: number; subject: Uint8Array }
export type Frame = {tag: 'Hello'; values: [Option<Uint8Array>, Want[]]}
  | {tag: 'Call'; values: [number, bigint, number, number, Uint8Array]}
  | {tag: 'Subscribe'; values: [number, number, Uint8Array, Option<Uint8Array>]}
  | {tag: 'Unsubscribe'; value: number}
  | {tag: 'Welcome'; values: [Caller, number, bigint, Uint8Array, string, bigint]}
  | {tag: 'Placed'; value: Entry}
  | {tag: 'Batch'; values: [bigint, number[], Group[], Done[]]}
  | {tag: 'Ended'; values: [number, WireOutcome<Uint8Array>]}
  | {tag: 'Reset'; values: [bigint, Done[]]};
export interface Codec { encode(frame: Frame): Uint8Array; decode(bytes: Uint8Array): Frame }
export type SubscriptionEnd = Extract<Frame, {tag: 'Ended'}>['values'][1];
export type Access<T> = {tag: 'Pending' | 'Ended'} | {tag: 'Ready'; value: T};
export interface SubscriptionControl { unsubscribe(): void }
export type Frozen<T> = T extends Uint8Array ? Readonly<T>
  : T extends ReadonlyMap<infer K, infer V> ? ReadonlyMap<K, V>
  : T extends object ? {readonly [K in keyof T]: Frozen<T[K]>}
  : T;
export type RowChange<Slots extends object> = {
  [S in keyof Slots]: {readonly slot: S; readonly key: string; readonly old?: Frozen<Slots[S]>; readonly new?: Frozen<Slots[S]>}
}[keyof Slots];
export interface Change<V, Slots extends object> {
  readonly invalidated?: true;
  readonly replaced: boolean;
  readonly value?: {readonly old?: Frozen<V>; readonly new: Frozen<V>};
  readonly rows: readonly RowChange<Slots>[];
}
export interface Subscription<V = Uint8Array, Slots extends object = Record<number, SlotRow>,
  Keys extends Record<keyof Slots, unknown> = Record<keyof Slots, Uint8Array>> extends SubscriptionControl {
  value(): Access<Frozen<V>>;
  rows<S extends keyof Slots>(slot: S): Access<readonly Frozen<Slots[S]>[]>;
  map<S extends keyof Slots>(slot: S): Access<ReadonlyMap<string, Frozen<Slots[S]>>>;
  get<S extends keyof Slots>(slot: S, key: Keys[S]): Access<Frozen<Slots[S]> | undefined>;
  current(): boolean;
}
export interface SubscriptionCodec<V, Slots extends object,
  Keys extends Record<keyof Slots, unknown> = Record<keyof Slots, Uint8Array>> {
  value(bytes: Uint8Array): V;
  slot(position: number): keyof Slots;
  row(row: SlotRow): Slots[keyof Slots];
  key<S extends keyof Slots>(slot: S, value: Keys[S]): Uint8Array;
  keyPrefixBytes?: number;
}
export interface Transport {
  call<R>(want: Want, args: Uint8Array, decode: (bytes: Uint8Array) => R, prediction?: unknown): Promise<Outcome<R>>;
  subscribe(want: Want, args: Uint8Array, onChange: (sub: Subscription, change: Change<Uint8Array, Record<number, SlotRow>>) => void,
    onEnd?: (reason: SubscriptionEnd) => void): Subscription;
  subscribe<V, Slots extends object, Keys extends Record<keyof Slots, unknown>>(want: Want, args: Uint8Array,
    onChange: (sub: Subscription<V, Slots, Keys>, change: Change<V, Slots>) => void,
    onEnd: ((reason: SubscriptionEnd) => void) | undefined,
    decode: SubscriptionCodec<V, Slots, Keys>): Subscription<V, Slots, Keys>;
}
export interface HttpRequest { path: string; body?: string }
export interface HttpReply { status: number; body: string; committed: boolean; replayed: boolean }
export interface Pending {
  generation: bigint; stream: number; p: bigint; want: Want; args: Uint8Array;
  slot: number; attempts: number; possiblyAdmitted: boolean; http?: HttpRequest;
}
export interface StreamRecord {
  caller: Caller | null; next: bigint; pending?: Pending; terminal?: {p: bigint; outcome: Outcome<Uint8Array>}; retired: boolean;
}
export const RECOVERY_VERSION = 3;
export interface Installation {
  version: typeof RECOVERY_VERSION; generation: bigint;
  directory: number[]; nextStream: number; streams: Record<string, StreamRecord>;
}
export interface Lease { release(): Promise<void>; lost: Promise<void> }
export interface Storage {
  lock(name: string, wait: boolean): Promise<Lease | null>;
  read(): Promise<Installation | null>;
  write(state: Installation): Promise<void>;
}
export type State = 'Connecting' | 'Ready' | 'Backoff' | 'Incompatible' | 'Unbound' | 'Fenced' | 'Retired' | 'StorageUnavailable' | 'Closed';
export interface Socket {
  binaryType: string;
  onopen: ((event?: unknown) => void) | null;
  onmessage: ((event: {data: unknown}) => void) | null;
  onclose: ((event: {code: number; reason?: string}) => void) | null;
  onerror: ((event?: unknown) => void) | null;
  send(data: Uint8Array): void; close(code?: number, reason?: string): void;
}
export interface Clock {
  now(): number; random(): number;
  set(fn: () => void, ms: number): unknown; clear(id: unknown): void;
}
export interface Identity {digest: Uint8Array; target: string; n_place: bigint}
export interface Options {
  url: string; storage: Storage; frame: Codec; wants: Want[]; identity?: Identity; credential?: Uint8Array;
  http?: { url: string; fetch?: typeof fetch };
  onHttpRecovered?: (reply: HttpReply) => void;
  socket?: (url: string, protocol: string) => Socket; clock?: Clock; deadlineMs?: number;
  onState?: (state: State) => void;
  onDiagnostic?: (diagnostic: string) => void;
  onRecovered?: (outcome: Outcome<Uint8Array>) => void;
  onBatch?: (version: bigint) => void;
  onPrediction?: (predictions: ReadonlyMap<bigint, unknown>) => void;
}
const clock: Clock = {
  now: () => Date.now(), random: () => Math.random(),
  set: (fn, ms) => setTimeout(fn, ms), clear: id => clearTimeout(id as ReturnType<typeof setTimeout>),
};
const equal = (a: Uint8Array, b: Uint8Array) => a.length === b.length && a.every((v, i) => v === b[i]);
export function validCaller(c: Caller): boolean {
  return !!c && Number.isInteger(c.issuer) && c.issuer >= 0 && c.issuer <= 255
    && c.subject instanceof Uint8Array && c.subject.length <= 64;
}
const sameCaller = (a: Caller | null, b: Caller | null) =>
  a === null || b === null ? a === b : a.issuer === b.issuer && equal(a.subject, b.subject);
const sameWant = (a: Want, b: Want) => equal(a.identity, b.identity) && a.args_fp === b.args_fp && a.result_fp === b.result_fp;
const copyWant = (w: Want): Want => ({identity: w.identity.slice(), args_fp: w.args_fp, result_fp: w.result_fp});
const key = (b: Uint8Array) => Array.from(b, n => n.toString(16).padStart(2, '0')).join('');
const fresh = (generation: bigint): Installation => ({version: RECOVERY_VERSION, generation, directory: [], nextStream: 0, streams: {}});
export class ClientError extends Error {
  constructor(public readonly code: string) { super(code); this.name = 'ClientError'; }
}
type ImageValue = {raw: Uint8Array; decoded: unknown};
interface SlotImage {
  prefix: string;
  entries: Map<string, ImageValue>;
  map?: ReadonlyMap<string, unknown>;
  rows?: readonly unknown[];
}
interface Image { value: ImageValue; slots: Map<PropertyKey, SlotImage> }
type ImageSlots = Record<PropertyKey, unknown>;
type ImageChange = Change<unknown, ImageSlots>;
interface SlotEdits { prefix: string; entries: Map<string, ImageValue | undefined> }
interface Sub {
  want: Want; args: Uint8Array; id?: number; active: boolean;
  codec: SubscriptionCodec<unknown, ImageSlots, ImageSlots>;
  image?: Image;
  notify: (change: ImageChange) => void;
  onEnd?: (reason: SubscriptionEnd) => void;
}

const rawSubscription: SubscriptionCodec<Uint8Array, Record<number, SlotRow>> = {
  value: bytes => bytes.slice(),
  slot: slot => slot,
  row: row => ({...row, key: row.key.slice(), value: row.value.tag === 'Some'
    ? {tag: 'Some', value: row.value.value.slice()} : {tag: 'None'}}),
  key: (_slot, bytes) => bytes,
};

function freezeDecoded(value: unknown): unknown {
  if (value !== null && typeof value === 'object'
      && (Array.isArray(value) || Object.getPrototypeOf(value) === Object.prototype
        || Object.getPrototypeOf(value) === null)) {
    for (const child of Object.values(value)) freezeDecoded(child);
    Object.freeze(value);
  }
  return value;
}

function stageImage(sub: Sub, group: Group, replaced: boolean) {
  const {codec, image} = sub;
  const value = (replaced || group.value.length > 0) && (!image || !equal(image.value.raw, group.value))
    ? {raw: group.value, decoded: freezeDecoded(codec.value(group.value))} : undefined;
  const slots = new Map<PropertyKey, SlotEdits>();
  for (const row of group.rows) {
    const slot = codec.slot(row.slot);
    const hex = key(row.key);
    const before = image?.slots.get(slot)?.entries.get(hex);
    const raw = row.value.tag === 'Some' ? row.value.value : undefined;
    const after = raw && before && equal(before.raw, raw) ? before
      : raw ? {raw, decoded: freezeDecoded(codec.row(row))} : undefined;
    if (raw === undefined) codec.row(row);
    let edits = slots.get(slot);
    if (!edits) {
      edits = {prefix: '', entries: new Map()};
      slots.set(slot, edits);
    }
    if (after) edits.prefix = hex.slice(0, 2 * (codec.keyPrefixBytes ?? 0));
    if (before !== after || (replaced && after)) edits.entries.set(hex, after);
    else edits.entries.delete(hex);
  }
  if (replaced && image) {
    for (const [slot, prior] of image.slots) {
      let edits = slots.get(slot);
      if (!edits) {
        edits = {prefix: prior.prefix, entries: new Map()};
        slots.set(slot, edits);
      }
      for (const hex of prior.entries.keys()) {
        if (!edits.entries.has(hex)) edits.entries.set(hex, undefined);
      }
      for (const [hex, after] of edits.entries) {
        if (prior.entries.get(hex) === after) edits.entries.delete(hex);
      }
    }
  }
  return {sub, id: group.sub, replaced, value, slots};
}

function commitImage({sub, replaced, value, slots}: ReturnType<typeof stageImage>): ImageChange {
  const image = sub.image ?? {value: value!, slots: new Map<PropertyKey, SlotImage>()};
  const rows: RowChange<ImageSlots>[] = [];
  for (const [slot, edits] of slots) {
    if (edits.entries.size === 0) continue;
    const target = image.slots.get(slot) ?? {prefix: edits.prefix, entries: new Map<string, ImageValue>()};
    for (const [hex, after] of edits.entries) {
      const before = target.entries.get(hex);
      if (after) target.entries.set(hex, after);
      else target.entries.delete(hex);
      rows.push(Object.freeze({slot, key: hex,
        ...(before && {old: before.decoded}), ...(after && {new: after.decoded})}) as RowChange<ImageSlots>);
    }
    target.prefix = edits.prefix || target.prefix;
    target.map = target.rows = undefined;
    image.slots.set(slot, target);
  }
  const changedValue = value && Object.freeze({
    ...(sub.image && {old: sub.image.value.decoded}), new: value.decoded,
  });
  if (value) image.value = value;
  sub.image = image;
  return Object.freeze({replaced, ...(changedValue && {value: changedValue}), rows: Object.freeze(rows)});
}

const emptyRows: readonly never[] = Object.freeze([]);
const emptyMap: ReadonlyMap<string, never> = new Map<string, never>();
const invalidation: ImageChange = Object.freeze({invalidated: true, replaced: false, rows: emptyRows});

interface QueuedCall {
  want: Want; args: Uint8Array; slot: number; prediction?: unknown; http?: HttpRequest;
  resolve: (outcome: Outcome<Uint8Array>) => void; reject: (error: unknown) => void;
}

export class Client implements Transport {
  state: State = 'Connecting';
  readonly generation: bigint;
  readonly stream: number;
  readonly predictions = new Map<bigint, unknown>();
  private credential?: Uint8Array;
  private record: StreamRecord;
  private streamLease: Lease;
  private socket?: Socket;
  private epoch = 0;
  private work: Promise<void> = Promise.resolve();
  private wants: Want[];
  private placed = 0;
  private welcomed = false;
  private welcomedAt = 0;
  private backoff = 100;
  private timer?: unknown;
  private stableTimer?: unknown;
  private clock: Clock;
  private sent = false;
  private nextSub = 0;
  private subs = new Set<Sub>();
  private version = 0n;
  private notifications?: Array<() => void>;
  private calls = new Set<QueuedCall>();
  private activeCall?: Pick<QueuedCall, 'resolve' | 'reject'>;
  private readyResolve!: () => void;
  private readyReject!: (error: unknown) => void;
  private ready: Promise<void>;

  private constructor(private options: Options, state: Installation, stream: number, lease: Lease) {
    this.generation = state.generation; this.stream = stream; this.streamLease = lease;
    this.credential = options.credential?.slice(); this.record = structuredClone(state.streams[stream]);
    this.wants = options.wants.map(copyWant);
    const pending = this.record.pending;
    if (pending && !pending.http && !this.wants.some(w => sameWant(w, pending.want))) this.wants.push(copyWant(pending.want));
    this.clock = options.clock ?? clock;
    this.ready = new Promise((resolve, reject) => {this.readyResolve = resolve; this.readyReject = reject;});
    // A connection may become terminal before an app chooses to await whenReady().
    void this.ready.catch(() => {});
    void lease.lost.then(() => this.enqueue(async () => {
      if (this.state !== 'Closed') await this.stop('Fenced', 'LocallyUnknown');
    }));
  }

  static async open(options: Options): Promise<Client> {
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
        const generation = state ? state.generation + 1n : randomGeneration();
        state = fresh(generation);
        await options.storage.write(state);
        options.onRecovered?.({tag: 'LocallyUnknown'});
      }
      let stream: number | undefined;
      for (const n of state.directory) {
        if (state.streams[n].retired) continue;
        lease = await options.storage.lock(`stream-${state.generation}-${n}`, false);
        if (lease) { stream = n; break; }
      }
      if (stream === undefined) {
        stream = state.nextStream;
        if (stream > 0xffffffff) throw new ClientError('StreamsExhausted');
        lease = await options.storage.lock(`stream-${state.generation}-${stream}`, false);
        if (!lease) throw new ClientError('StreamLeaseUnavailable');
        state.nextStream++; state.directory.push(stream);
        state.streams[stream] = {caller: null, next: 0n, retired: false};
        await options.storage.write(state);
      }
      const client = new Client(options, state, stream, lease!);
      await lock.release();
      client.connect();
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
    if (state !== 'Ready' && state !== 'Connecting' && state !== 'Backoff') {
      for (const call of this.calls) call.reject(new ClientError(state));
      this.calls.clear();
    }
    this.options.onState?.(state);
  }
  private async unavailable(error: unknown): Promise<void> {
    if (this.state === 'Closed') return;
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
        await this.options.storage.write(fresh((state?.generation ?? this.generation) + 1n));
        await this.stop('Fenced', 'LocallyUnknown'); return;
      }
      const stream = state.streams[this.stream];
      if (state.generation !== this.generation || !stream || !sameCaller(stream.caller, this.record.caller)) {
        await this.stop('Fenced', 'LocallyUnknown'); return;
      }
      return await fn(state);
    } finally { await lock.release(); }
  }
  private connect(): void {
    if (['Closed', 'Fenced', 'Retired', 'Incompatible', 'Unbound', 'StorageUnavailable'].includes(this.state)) return;
    this.setState('Connecting'); this.welcomed = false; this.placed = 0; this.sent = false;
    this.clearSubscriptions();
    const epoch = ++this.epoch;
    let socket: Socket;
    try { socket = (this.options.socket ?? ((url, protocol) => new WebSocket(url, protocol) as unknown as Socket))(this.options.url, 'hirsel.3'); }
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
      if (epoch === this.epoch) await this.startNextCall();
    }); };
    const lost = (finish: () => Promise<void>) => {
      if (epoch !== this.epoch) return;
      const lossEpoch = ++this.epoch;
      void this.enqueue(async () => {
        if (lossEpoch === this.epoch) await finish();
      });
      this.clearSubscriptions();
    };
    socket.onclose = event => lost(async () => {
      // Before Welcome, 1008 is the place's binding refusing this Hello's credential.
      if (event.code === 1008 && !this.welcomed) await this.unbound();
      else if (event.code === 1009) await this.incompatible(1009,
        !this.welcomed && event.reason === 'hirsel.3' ? 'Reload' : 'ReplyTooLarge');
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
    this.cancelTimers(); this.disconnect(); this.setState('Backoff');
    const delay = this.clock.random() * this.backoff;
    this.backoff = Math.min(10000, this.backoff * 2);
    this.timer = this.clock.set(() => { this.timer = undefined; void this.enqueue(async () => this.connect()); }, delay);
  }
  private async incompatible(code: number, problem = 'Incompatible'): Promise<void> {
    this.cancelTimers(); this.disconnect(); this.setState('Incompatible');
    const diagnostic = problem === 'Reload' ? 'Reload required before reconnecting to hirsel.3'
      : problem === 'ReplyTooLarge' ? 'Stored reply exceeds the server message limit (1009)'
      : `Incompatible (${code}); reload bindings or reconnect explicitly`;
    this.options.onDiagnostic?.(diagnostic);
    if (problem === 'ReplyTooLarge') {
      this.activeCall?.reject(new ClientError(problem));
      this.activeCall = undefined;
      if (this.record.pending) this.dropPrediction(this.record.pending.p);
    }
    this.readyReject(new ClientError(problem));
  }
  private async unbound(): Promise<void> {
    this.cancelTimers(); this.disconnect(); this.setState('Unbound');
    this.options.onDiagnostic?.(this.credential
      ? 'Unbound: the place refused this credential (Close 1008 before Welcome)'
      : 'Unbound: the place binds callers and needs a credential; pass `credential` to Client.open (Close 1008 before Welcome)');
    this.readyReject(new ClientError('Unbound'));
  }
  reconnect(): void {
    if (this.state !== 'Incompatible' && this.state !== 'Unbound' && this.state !== 'Backoff') throw new ClientError(this.state);
    this.cancelTimers(); this.setState('Connecting');
    this.ready = new Promise((resolve, reject) => {this.readyResolve = resolve; this.readyReject = reject;});
    void this.ready.catch(() => {});
    void this.enqueue(async () => this.connect());
  }
  private async receive(frame: Frame, epoch: number): Promise<void> {
    if (frame.tag === 'Welcome') {
      if (this.welcomed) { await this.incompatible(1008); return; }
      const [caller, , , digest, target, n_place] = frame.values;
      if (!validCaller(caller)) { await this.incompatible(1007); return; }
      this.compareIdentity({digest, target, n_place});
      const previous = this.record.caller;
      if (previous && !sameCaller(previous, caller) && (previous.issuer !== 1 || caller.issuer !== 1)) {
        await this.fence('LocallyUnknown'); return;
      }
      if (!sameCaller(previous, caller)) {
        await this.installation(async state => {
          const record = previous ? {caller, next: 0n, retired: false} : {...this.record, caller};
          state.streams[this.stream] = record;
          await this.options.storage.write(state);
          if (this.record.pending) this.finish({tag: 'LocallyUnknown'});
          this.record = structuredClone(record);
        });
        if (epoch !== this.epoch || this.state !== 'Connecting') return;
      }
      this.welcomed = true; this.welcomedAt = this.clock.now();
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
      const staged: ReturnType<typeof stageImage>[] = [];
      try {
        for (const sub of this.subs) {
          if (sub.id === undefined) continue;
          const replace = completing.has(sub.id);
          const group = byId.get(sub.id);
          if (!group && !replace) continue;
          if (!group || (!replace && !sub.image)) { await this.incompatible(1008); return; }
          staged.push(stageImage(sub, group, replace));
        }
      } catch { await this.incompatible(1007); return; }
      this.notifications = [];
      await this.outcomes(outcomes);
      if (epoch !== this.epoch) { this.publish(); return; }
      this.version = version;
      for (const edit of staged) {
        const {sub, id} = edit;
        if (sub.id !== id) continue;
        const change = commitImage(edit);
        if (change.value || change.rows.length > 0) {
          this.notify(() => { if (sub.id === id) sub.notify(change); });
        }
      }
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
      this.publish(); await this.subscribeAll();
    } else if (frame.tag === 'Ended') {
      const [id, reason] = frame.values;
      const sub = [...this.subs].find(s => s.id === id);
      if (sub) {
        this.removeSubscription(sub);
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
    // Recovery sends precede subscription restoration and any new API call.
    await this.sendPending(); await this.subscribeAll(); this.readyResolve();
  }
  call<R>(want: Want, args: Uint8Array, decode: (bytes: Uint8Array) => R, prediction?: unknown): Promise<Outcome<R>> {
    return this.queueCall(want, args, prediction)
      .then(outcome => outcome.tag === 'Returned' ? {tag: 'Returned', value: decode(outcome.value)} : outcome);
  }
  callHttp(want: Want, request: HttpRequest): Promise<Outcome<HttpReply>> {
    if (!this.options.http || !validHttpRequest(request)) return Promise.reject(new ClientError('InvalidHttpRequest'));
    return this.queueCall(want, new Uint8Array(), undefined, {...request})
      .then(outcome => outcome.tag === 'Returned'
        ? {tag: 'Returned', value: JSON.parse(new TextDecoder().decode(outcome.value)) as HttpReply} : outcome);
  }
  private queueCall(want: Want, args: Uint8Array, prediction?: unknown, http?: HttpRequest): Promise<Outcome<Uint8Array>> {
    const copied = copyWant(want), arg = args.slice();
    return new Promise((resolve, reject) => {
      void this.enqueue(async () => {
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
        this.calls.add({want: copied, args: arg, slot, prediction, http, resolve, reject});
        await this.startNextCall();
      }).catch(reject);
    });
  }
  private async startNextCall(): Promise<void> {
    if (this.state !== 'Ready' || this.record.pending) return;
    const call = this.calls.values().next().value;
    if (!call) return;
    this.calls.delete(call);
    try {
      await this.installation(async state => {
        const record = state.streams[this.stream];
        const pending: Pending = {generation: this.generation, stream: this.stream, p: record.next,
          want: call.want, args: call.args, slot: call.slot, attempts: 0, possiblyAdmitted: false, ...(call.http && {http: call.http})};
        record.pending = pending;
        await this.options.storage.write(state);
        this.record = structuredClone(record); this.activeCall = {resolve: call.resolve, reject: call.reject};
        if (call.prediction !== undefined) {
          this.predictions.set(pending.p, call.prediction);
          this.options.onPrediction?.(this.predictions);
        }
      });
      if (this.state !== 'Ready') throw new ClientError(this.state);
      await this.sendPending();
    } catch (error) {
      call.reject(error);
      throw error;
    }
  }
  private async sendPending(): Promise<void> {
    if (this.state !== 'Ready' || !this.record.pending || this.sent) return;
    const http = await this.installation(async state => {
      const record = state.streams[this.stream], pending = record.pending!;
      // Persist possible admission before send. A crash here is conservative:
      // a later definitive refusal cannot authorize replacing these bytes.
      pending.attempts++; pending.possiblyAdmitted = pending.attempts > 1 || pending.possiblyAdmitted;
      if (!pending.http) pending.slot = this.wants.findIndex(w => sameWant(w, pending.want));
      await this.options.storage.write(state);
      this.record = structuredClone(record);
      this.sent = true;
      if (pending.http) return {request: pending.http, p: pending.p, caller: record.caller!};
      try { this.socket!.send(this.options.frame.encode({tag: 'Call', values: [this.stream, pending.p, pending.slot, this.options.deadlineMs ?? 30000, pending.args]})); }
      catch { this.transient(); }
    });
    if (http) {
      const epoch = this.epoch;
      void this.sendHttp(http.request, http.p, http.caller).then(
        reply => this.enqueue(async () => {
          if (epoch === this.epoch && this.state === 'Ready') await this.httpOutcome(http.p, reply);
        }),
        () => this.enqueue(async () => {
          if (epoch === this.epoch && this.state === 'Ready') this.retryPending();
        }),
      ).catch(() => {});
    }
  }
  private async sendHttp(request: HttpRequest, p: bigint, caller: Caller): Promise<HttpReply> {
    const options = this.options.http;
    if (!options) throw new ClientError('HttpUnavailable');
    const headers = new Headers();
    if (this.credential) headers.set('Authorization', Array.from(this.credential, b => String.fromCharCode(b)).join(''));
    if (request.body !== undefined) headers.set('Content-Type', 'application/json');
    headers.set('Idempotency-Key', `"${this.stream}.${p}"`);
    headers.set('Hirsel-Expected-Caller', btoa(String.fromCharCode(caller.issuer, caller.subject.length, ...caller.subject)));
    const send = options.fetch ?? globalThis.fetch;
    const result = await send(new URL(request.path, options.url), {
      method: 'POST', headers, body: request.body, redirect: 'error',
    });
    return {status: result.status, body: await result.text(),
      committed: result.headers.get('Idempotent-Outcome') === 'committed',
      replayed: result.headers.get('Idempotent-Replayed') === 'true'};
  }
  private async httpOutcome(p: bigint, reply: HttpReply): Promise<void> {
    const pending = this.record.pending;
    if (pending?.p !== p || !pending.http) return;
    if (!reply.committed && reply.status === 403 && reply.body === '{"refused":"fenced"}') {
      await this.fence('LocallyUnknown'); return;
    }
    const recovered = !this.activeCall;
    const outcome = {tag: 'Returned' as const, value: new TextEncoder().encode(JSON.stringify(reply))};
    if (reply.committed) {
      await this.installation(async state => {
        const record = state.streams[this.stream];
        record.terminal = {p, outcome};
        delete record.pending;
        if (p === 0xffffffffffffffffn) record.retired = true; else record.next = p + 1n;
        await this.options.storage.write(state);
        this.record = structuredClone(record);
        if (recovered) {
          this.sent = false;
          this.options.onHttpRecovered?.(reply);
        } else this.finish(outcome);
        if (record.retired) await this.stop('Retired');
      });
      await this.startNextCall();
    } else {
      if (recovered) {
        this.sent = false;
        this.options.onHttpRecovered?.(reply);
      } else this.finish(outcome);
      if (reply.status === 409 || reply.status === 503 || reply.status === 504) this.retryPending();
    }
  }
  private retryPending(): void {
    this.sent = false;
    if (this.timer !== undefined) this.clock.clear(this.timer);
    this.timer = this.clock.set(() => {
      this.timer = undefined; void this.enqueue(async () => this.sendPending());
    }, this.clock.random() * this.backoff);
    this.backoff = Math.min(10000, this.backoff * 2);
  }
  private async outcomes(rows: Done[]): Promise<void> {
    for (const row of rows) {
      const pending = this.record.pending;
      if (!pending || pending.http || row.stream !== this.stream || row.p !== pending.p) continue;
      this.dropPrediction(pending.p);
      const outcome = row.outcome;
      if (outcome.tag === 'Refused' && outcome.value.tag === 'RequestMismatch') { await this.fence('RequestMismatch'); continue; }
      if (outcome.tag === 'Refused' && outcome.value.tag === 'HistoryUnavailable') { await this.retire('LocallyUnknown'); continue; }
      if (outcome.tag === 'Returned' || outcome.tag === 'Failed') {
        await this.installation(async state => {
          const record = state.streams[this.stream];
          record.terminal = {p: pending.p, outcome: structuredClone(outcome)};
          delete record.pending;
          if (pending.p === 0xffffffffffffffffn) record.retired = true; else record.next = pending.p + 1n;
          await this.options.storage.write(state); this.record = structuredClone(record);
          this.finish(outcome);
          if (record.retired) await this.stop('Retired');
        });
      } else if (outcome.tag === 'Refused' && ['NoEntry', 'Malformed', 'NotAllowed'].includes(outcome.value.tag)) {
        if (pending.possiblyAdmitted || pending.attempts !== 1) {
          await this.retire('LocallyUnknown');
        } else await this.installation(async state => {
          const record = state.streams[this.stream];
          record.terminal = {p: pending.p, outcome: structuredClone(outcome)};
          delete record.pending;
          await this.options.storage.write(state); this.record = structuredClone(record); this.finish(outcome);
        });
      } else if (outcome.tag === 'Abandoned' || (outcome.tag === 'Refused' && ['Overloaded', 'Deadline', 'Budget'].includes(outcome.value.tag))) {
        this.retryPending();
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
  private dropPrediction(p: bigint): void {
    if (this.predictions.delete(p)) this.notify(() => this.options.onPrediction?.(this.predictions));
  }
  private finish(outcome: Outcome<Uint8Array>): void {
    this.sent = false;
    const p = this.record.pending?.p ?? this.record.terminal?.p;
    if (p !== undefined) this.dropPrediction(p);
    const call = this.activeCall; this.activeCall = undefined;
    this.notify(() => { if (call) call.resolve(outcome); else this.options.onRecovered?.(outcome); });
  }
  private async stop(state: 'Fenced' | 'Retired' | 'Closed', outcome?: 'LocallyUnknown' | 'RequestMismatch'): Promise<void> {
    this.cancelTimers(); this.disconnect(); this.setState(state);
    if (outcome && this.record.pending) this.finish({tag: outcome});
    this.readyReject(new ClientError(state));
  }
  private async retire(outcome: 'LocallyUnknown'): Promise<void> {
    await this.installation(async state => {
      state.streams[this.stream].retired = true;
      await this.options.storage.write(state); await this.stop('Retired', outcome);
    });
  }
  private async fence(outcome: 'LocallyUnknown' | 'RequestMismatch'): Promise<void> {
    await this.installation(async state => {
      state.streams[this.stream].retired = true;
      await this.options.storage.write(state);
      await this.options.storage.write(fresh(state.generation + 1n));
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
    const access = <T>(read: (value: Image) => T): Access<T> => {
      if (!sub.active) return {tag: 'Ended'};
      if (!sub.image) return {tag: 'Pending'};
      return {tag: 'Ready', value: read(sub.image)};
    };
    const handle: Subscription<V, Slots, Keys> = {
      value: () => access(image => image.value.decoded as Frozen<V>),
      rows: slot => access(image => {
        const current = image.slots.get(slot);
        if (!current) return emptyRows;
        current.rows ??= Object.freeze(Array.from(current.entries.values(), entry => entry.decoded));
        return current.rows as readonly Frozen<Slots[typeof slot]>[];
      }),
      map: slot => access(image => {
        const current = image.slots.get(slot);
        if (!current) return emptyMap;
        current.map ??= new Map(Array.from(current.entries, ([hex, entry]) => [hex, entry.decoded]));
        return current.map as ReadonlyMap<string, Frozen<Slots[typeof slot]>>;
      }),
      get: (slot, typedKey) => access(image => {
        const current = image.slots.get(slot);
        if (!current) return undefined;
        const hex = current.prefix + key(codec.key(slot, typedKey));
        return current.entries.get(hex)?.decoded as Frozen<Slots[typeof slot]> | undefined;
      }),
      current: () => true,
      unsubscribe: () => this.unsubscribe(sub),
    };
    this.subs.add(sub);
    void this.enqueue(async () => this.subscribeAll());
    return handle;
  }
  private clearSubscriptions(): void {
    const cleared: Sub[] = [];
    for (const sub of this.subs) {
      if (sub.image) cleared.push(sub);
      sub.id = undefined; sub.image = undefined;
    }
    for (const sub of cleared) sub.notify(invalidation);
  }
  private removeSubscription(sub: Sub): void {
    sub.active = false; sub.id = undefined; sub.image = undefined; this.subs.delete(sub);
  }
  private unsubscribe(sub: Sub): void {
    const id = sub.id;
    this.removeSubscription(sub);
    void this.enqueue(async () => {
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
    await this.enqueue(async () => { await this.stop('Closed', 'LocallyUnknown'); await this.streamLease.release(); });
  }
}

export function validHttpRequest(request: HttpRequest): boolean {
  return !!request && typeof request.path === 'string'
    && /^\/\.hirsel\/op\/[a-z0-9][a-z0-9_.-]{0,63}(\?[^#\s]*)?$/.test(request.path)
    && (request.body === undefined || typeof request.body === 'string');
}

export class CorruptState extends Error {}
function randomGeneration(): bigint {
  const bytes = crypto.getRandomValues(new Uint32Array(4));
  return bytes.reduce((n, b) => (n << 32n) | BigInt(b), 0n);
}
export function intact(s: Installation): boolean {
  return s.version === RECOVERY_VERSION && s.directory.every(n => {
    if (!Object.hasOwn(s.streams, n)) return false;
    const record = s.streams[n];
    return record.caller === null
      ? record.next === 0n && !record.pending && !record.terminal
      : validCaller(record.caller);
  });
}
