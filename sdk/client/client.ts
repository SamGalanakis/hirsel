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
export interface Row extends SlotRow { sub: number }
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
export interface Subscription<V = Uint8Array, Slots extends object = Record<number, SlotRow>> extends SubscriptionControl {
  value(): Access<V>;
  rows<S extends keyof Slots>(slot: S): Access<Slots[S][]>;
}
export interface SubscriptionCodec<V, Slots extends object> {
  value(bytes: Uint8Array): V;
  slot(position: number): keyof Slots;
  row(row: SlotRow): Slots[keyof Slots];
}
export interface Transport {
  call<R>(want: Want, args: Uint8Array, decode: (bytes: Uint8Array) => R, prediction?: unknown): Promise<Outcome<R>>;
  subscribe(want: Want, args: Uint8Array, onChange: (sub: Subscription) => void,
    onEnd?: (reason: SubscriptionEnd) => void): Subscription;
  subscribe<V, Slots extends object>(want: Want, args: Uint8Array,
    onChange: (sub: Subscription<V, Slots>) => void, onEnd: ((reason: SubscriptionEnd) => void) | undefined,
    decode: SubscriptionCodec<V, Slots>): Subscription<V, Slots>;
}
export interface Pending {
  generation: bigint; stream: number; p: bigint; want: Want; args: Uint8Array;
  slot: number; attempts: number; possiblyAdmitted: boolean;
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
interface Image {
  value: unknown;
  slots: Map<PropertyKey, Map<string, unknown>>;
}
interface RowEdit { slot: PropertyKey; key: string; row?: unknown }
interface Sub {
  want: Want; args: Uint8Array; id?: number; active: boolean; complete: boolean;
  codec?: SubscriptionCodec<unknown, Record<string | number, unknown>>;
  image?: Image;
  notify: (group: Group, replace: boolean) => void;
  onEnd?: (reason: SubscriptionEnd) => void;
}

const rawSubscription: SubscriptionCodec<Uint8Array, Record<number, SlotRow>> = {
  value: bytes => bytes.slice(),
  slot: slot => slot,
  row: row => structuredClone(row),
};

interface QueuedCall {
  want: Want; args: Uint8Array; slot: number; prediction?: unknown;
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
    if (pending && !this.wants.some(w => sameWant(w, pending.want))) this.wants.push(copyWant(pending.want));
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
    this.options.onDiagnostic?.(String(error));
    this.setState('StorageUnavailable'); this.cancelTimers(); this.disconnect();
    this.readyReject(new ClientError('StorageUnavailable'));
  }
  private async installation<T>(fn: (state: Installation) => Promise<T>): Promise<T | undefined> {
    const lock = await this.options.storage.lock('install', true);
    if (!lock) throw new ClientError('StorageUnavailable');
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
      await this.receive(frame);
      await this.startNextCall();
    }); };
    socket.onclose = event => {
      if (epoch === this.epoch) this.clearSubscriptions();
      void this.enqueue(async () => {
        if (epoch !== this.epoch) return;
        // Before Welcome, 1008 is the place's binding refusing this Hello's credential.
        if (event.code === 1008 && !this.welcomed) await this.unbound();
        else if (event.code === 1009) await this.incompatible(1009,
          !this.welcomed && event.reason === 'hirsel.3' ? 'Reload' : 'ReplyTooLarge');
        else if ([1007, 1008].includes(event.code)) await this.incompatible(event.code);
        else this.transient();
      });
    };
    socket.onerror = () => {
      if (epoch === this.epoch) this.clearSubscriptions();
      void this.enqueue(async () => { if (epoch === this.epoch) this.transient(); });
    };
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
  private async receive(frame: Frame): Promise<void> {
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
        if (this.state !== 'Connecting') return;
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
      const staged: Array<{sub: Sub; group: Group; replace: boolean; value: unknown; rows: RowEdit[]}> = [];
      try {
        for (const sub of this.subs) {
          if (sub.id === undefined) continue;
          const replace = completing.has(sub.id);
          const group = byId.get(sub.id);
          if (!group && !replace) continue;
          if (!group || (!replace && !sub.complete)) { await this.incompatible(1008); return; }
          const codec = sub.codec;
          const value = codec && (replace || group.value.length > 0) ? codec.value(group.value) : sub.image?.value;
          const rows = codec ? group.rows.map(row => {
            const decoded = codec.row(row);
            return {slot: codec.slot(row.slot), key: key(row.key), row: row.value.tag === 'Some' ? decoded : undefined};
          }) : [];
          staged.push({sub, group, replace, value, rows});
        }
      } catch { await this.incompatible(1007); return; }
      this.notifications = [];
      await this.outcomes(outcomes);
      this.version = version;
      for (const {sub, group, replace, value, rows} of staged) {
        if (sub.id !== group.sub) continue;
        sub.complete = true;
        if (!sub.codec) continue;
        const slots = replace ? new Map<PropertyKey, Map<string, unknown>>() : sub.image!.slots;
        for (const edit of rows) {
          let slot = slots.get(edit.slot);
          if (!slot) { slot = new Map(); slots.set(edit.slot, slot); }
          if (edit.row === undefined) slot.delete(edit.key); else slot.set(edit.key, edit.row);
        }
        sub.image = {value, slots};
      }
      this.publish();
      for (const {sub, group, replace} of staged) {
        if (sub.id === group.sub) sub.notify(group, replace);
      }
      this.options.onBatch?.(version);
    } else if (frame.tag === 'Reset') {
      const [version, outcomes] = frame.values;
      if (version < this.version) { await this.incompatible(1008); return; }
      this.clearSubscriptions();
      this.notifications = [];
      await this.outcomes(outcomes); this.version = version;
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
    const copied = copyWant(want), arg = args.slice();
    return new Promise<Outcome<Uint8Array>>((resolve, reject) => {
      void this.enqueue(async () => {
        if (this.state !== 'Ready') throw new ClientError(this.state);
        const slot = this.wants.findIndex(w => sameWant(w, copied));
        if (slot < 0) throw new ClientError('UnknownWant');
        this.calls.add({want: copied, args: arg, slot, prediction, resolve, reject});
        await this.startNextCall();
      }).catch(reject);
    }).then(outcome => outcome.tag === 'Returned' ? {tag: 'Returned', value: decode(outcome.value)} : outcome);
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
          want: call.want, args: call.args, slot: call.slot, attempts: 0, possiblyAdmitted: false};
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
    await this.installation(async state => {
      const record = state.streams[this.stream], pending = record.pending!;
      // Persist possible admission before send. A crash here is conservative:
      // a later definitive refusal cannot authorize replacing these bytes.
      pending.attempts++; pending.possiblyAdmitted = pending.attempts > 1 || pending.possiblyAdmitted;
      pending.slot = this.wants.findIndex(w => sameWant(w, pending.want));
      await this.options.storage.write(state);
      this.record = structuredClone(record);
      this.sent = true;
      try { this.socket!.send(this.options.frame.encode({tag: 'Call', values: [this.stream, pending.p, pending.slot, this.options.deadlineMs ?? 30000, pending.args]})); }
      catch { this.transient(); }
    });
  }
  private async outcomes(rows: Done[]): Promise<void> {
    for (const row of rows) {
      const pending = this.record.pending;
      if (!pending || row.stream !== this.stream || row.p !== pending.p) continue;
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
        this.sent = false;
        if (this.timer !== undefined) this.clock.clear(this.timer);
        this.timer = this.clock.set(() => {
          this.timer = undefined; void this.enqueue(async () => this.sendPending());
        }, this.clock.random() * this.backoff);
        this.backoff = Math.min(10000, this.backoff * 2);
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
  subscribe(want: Want, args: Uint8Array, onChange: (sub: Subscription) => void,
    onEnd?: (reason: SubscriptionEnd) => void): Subscription;
  subscribe<V, Slots extends object>(want: Want, args: Uint8Array,
    onChange: (sub: Subscription<V, Slots>) => void, onEnd: ((reason: SubscriptionEnd) => void) | undefined,
    decode: SubscriptionCodec<V, Slots>): Subscription<V, Slots>;
  subscribe<V, Slots extends object>(want: Want, args: Uint8Array, onChange: (sub: Subscription<V, Slots>) => void,
    onEnd?: (reason: SubscriptionEnd) => void, decode?: SubscriptionCodec<V, Slots>): Subscription<V, Slots> {
    const codec = (decode ?? rawSubscription) as Sub['codec'];
    const sub = this.addSubscription(want, args, () => onChange(handle), onEnd, codec);
    const access = <T>(read: (value: Image) => T): Access<T> => {
      if (!sub.active) return {tag: 'Ended'};
      if (!sub.image) return {tag: 'Pending'};
      return {tag: 'Ready', value: structuredClone(read(sub.image))};
    };
    const handle: Subscription<V, Slots> = {
      value: () => access(image => image.value as V),
      rows: slot => access(image => [...(image.slots.get(slot)?.values() ?? [])] as Slots[typeof slot][]),
      unsubscribe: () => this.unsubscribe(sub),
    };
    return handle;
  }
  subscribeChanges(want: Want, args: Uint8Array, onChanges: (rows: Row[], replace: boolean) => void,
    onEnd?: (reason: SubscriptionEnd) => void): SubscriptionControl {
    const sub = this.addSubscription(want, args,
      (group, replace) => onChanges(group.rows.map(row => ({...row, sub: group.sub})), replace), onEnd);
    return {unsubscribe: () => this.unsubscribe(sub)};
  }
  private addSubscription(want: Want, args: Uint8Array, notify: Sub['notify'], onEnd?: Sub['onEnd'], codec?: Sub['codec']): Sub {
    if (!this.wants.some(w => sameWant(w, want))) throw new ClientError('UnknownWant');
    const sub: Sub = {want: copyWant(want), args: args.slice(), notify, onEnd, codec, complete: false, active: true};
    this.subs.add(sub);
    void this.enqueue(async () => this.subscribeAll());
    return sub;
  }
  private clearSubscriptions(): void {
    for (const sub of this.subs) { sub.id = undefined; sub.image = undefined; sub.complete = false; }
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
      sub.id = this.nextSub++; sub.complete = false;
      await this.send({tag: 'Subscribe', values: [sub.id, this.wants.findIndex(w => sameWant(w, sub.want)), sub.args, {tag: 'None'}]});
    }
  }
  async close(): Promise<void> {
    await this.enqueue(async () => { await this.stop('Closed', 'LocallyUnknown'); await this.streamLease.release(); });
  }
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
