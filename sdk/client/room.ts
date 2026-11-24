import type {Clock} from './browser.js';
import type {Caller} from './persistence.js';
import type {Change, Subscription} from './image.js';
import type {Frame, Option, Part, SlotRow, SubscriptionEnd} from './wire.js';

export type PeerId = number;
export interface ValueCodec<T> { encode(value: T): Uint8Array; decode(bytes: Uint8Array): T }
// A room's State and Message codecs and the bounds runner.room registered.
export interface RoomCodec<S, M> {
  state: ValueCodec<S>; message: ValueCodec<M>; peers: number; stateMax: number; messageMax: number;
}
export interface RoomPeer<S> { readonly caller: Caller; readonly state: S | undefined }
// `complete` replaces the roster: a new membership, or the end of one.
export interface RoomPeersChange {
  readonly complete: boolean; readonly joined: readonly PeerId[]; readonly left: readonly PeerId[]; readonly set: readonly PeerId[];
}
export interface RoomHandlers<S, M, V, Slots extends object, Keys extends Record<keyof Slots, unknown>> {
  // The Grant (the room's value) and any declared slot rows, as a subscription's onChange.
  onChange?: (room: LiveRoom<S, M, V, Slots, Keys>, change: Change<V, Slots>) => void;
  onPeers?: (peers: ReadonlyMap<PeerId, RoomPeer<S>>, change: RoomPeersChange) => void;
  onMessage?: (from: PeerId, message: M, direct: boolean) => void;
  onGap?: (dropped: number) => void;
  onLost?: (dropped: number) => void;
  onFull?: () => void;
  onEnd?: (reason: SubscriptionEnd) => void;
}
export interface LiveRoom<S, M, V = Uint8Array, Slots extends object = Record<number, SlotRow>,
  Keys extends Record<keyof Slots, unknown> = Record<keyof Slots, Uint8Array>> extends Subscription<V, Slots, Keys> {
  // This membership's peer id; undefined while waiting, full or reconnecting.
  readonly self: PeerId | undefined;
  readonly full: boolean;
  peers(): ReadonlyMap<PeerId, RoomPeer<S>>;
  set(state: S | undefined): void;
  send(message: M, to?: PeerId): void;
  leave(): void;
}

// What a room borrows from its client's session.
export interface LiveLink {
  ready(): boolean;
  backlog(): number;
  write(frame: Frame): void;
  clock: Pick<Clock, 'set' | 'clear'>;
}
export type Decoded = [states: [number, unknown][], msgs: [number, unknown, boolean][]];

// One Live frame (LIVE_MAX) and the relay's ring E: a Set waits and a Send drops above it.
const liveBufferBytes = 65536;
const recheckMs = 16;

export class RoomMember {
  self?: number;
  full = false;
  readonly peers = new Map<number, RoomPeer<unknown>>();
  // The newest set() value, and whether this membership has not been sent it.
  private stored?: Option<Uint8Array>;
  private owed = false;
  private timer?: unknown;
  constructor(private readonly codec: RoomCodec<unknown, unknown>,
    private readonly handlers: RoomHandlers<unknown, unknown, unknown, object, Record<never, unknown>>,
    private readonly link: LiveLink, private readonly sub: {readonly id?: number; readonly active: boolean}) {}

  // Throws when a state or message does not decode; nothing is applied then.
  decode(part: Part): Decoded {
    return [part.states.map(([peer, raw]) => [peer, raw.tag === 'Some' ? this.codec.state.decode(raw.value) : undefined]),
      part.msgs.map(([from, direct, raw]) => [from, this.codec.message.decode(raw), direct])];
  }
  // Joined, then states, then messages, then left (one Part, in order).
  apply(part: Part, [states, msgs]: Decoded): void {
    const before = part.complete ? new Set(this.peers.keys()) : undefined;
    if (part.complete) this.peers.clear();
    if (part.you.tag === 'Some') { this.self = part.you.value; this.full = false; this.owed = this.stored !== undefined; }
    const joined: number[] = [], set: number[] = [], left: number[] = [];
    for (const [peer, caller] of part.joined) { this.peers.set(peer, {caller, state: undefined}); joined.push(peer); }
    for (const [peer, state] of states) {
      const known = this.peers.get(peer);
      if (known) { this.peers.set(peer, {caller: known.caller, state}); set.push(peer); }
    }
    for (const [from, message, direct] of msgs) {
      if (!this.sub.active) return;
      this.handlers.onMessage?.(from, message, direct);
    }
    for (const peer of part.left) if (this.peers.delete(peer)) left.push(peer);
    if (before) for (const peer of before) if (!this.peers.has(peer)) left.push(peer);
    if (part.full) { this.full = true; this.self = undefined; }
    if (!this.sub.active) return;
    if (part.complete || joined.length || set.length || left.length) {
      this.handlers.onPeers?.(this.peers, {complete: part.complete, joined, left, set});
    }
    if (part.gap > 0) this.handlers.onGap?.(part.gap);
    if (part.lost > 0) this.handlers.onLost?.(part.lost);
    if (part.full) this.handlers.onFull?.();
    this.flush();
  }
  // A membership ends with its session: the roster empties and the stored state is owed
  // to the next one, which the client gets on resubscribe.
  end(): void {
    this.self = undefined; this.full = false;
    this.owed = this.stored !== undefined;
    this.drop();
    if (this.peers.size === 0) return;
    const left = [...this.peers.keys()];
    this.peers.clear();
    this.handlers.onPeers?.(this.peers, {complete: true, joined: [], left, set: []});
  }
  drop(): void {
    if (this.timer !== undefined) { this.link.clock.clear(this.timer); this.timer = undefined; }
  }
  flush(): void {
    if (!this.owed || this.self === undefined || this.sub.id === undefined || !this.link.ready()) return;
    if (this.link.backlog() >= liveBufferBytes) {
      this.timer ??= this.link.clock.set(() => { this.timer = undefined; this.flush(); }, recheckMs);
      return;
    }
    this.owed = false;
    this.link.write({tag: 'Set', values: [this.sub.id, this.stored!]});
  }
  set(state: unknown): void {
    if (!this.sub.active) return;
    const raw = state === undefined ? undefined : this.codec.state.encode(state);
    const max = this.codec.stateMax;
    if (raw && raw.length > max) throw new RangeError(`room state is ${raw.length} bytes; the room allows ${max}`);
    this.stored = raw ? {tag: 'Some', value: raw} : {tag: 'None'};
    this.owed = true;
    this.flush();
  }
  send(message: unknown, to?: PeerId): void {
    if (!this.sub.active) return;
    const raw = this.codec.message.encode(message), max = this.codec.messageMax;
    if (raw.length > max) throw new RangeError(`room message is ${raw.length} bytes; the room allows ${max}`);
    if (this.self === undefined || this.sub.id === undefined || !this.link.ready() || this.link.backlog() > liveBufferBytes) {
      this.handlers.onLost?.(1); return;
    }
    this.link.write({tag: 'Send', values: [this.sub.id, to === undefined ? {tag: 'None'} : {tag: 'Some', value: to}, raw]});
  }
  // The room's handle: the subscription's, plus the membership.
  handle<T extends {unsubscribe(): void}>(base: T): T {
    const member = this;
    return Object.defineProperties(base, {
      self: {get: () => member.self},
      full: {get: () => member.full},
      peers: {value: () => member.peers},
      set: {value: (state: unknown) => member.set(state)},
      send: {value: (message: unknown, to?: PeerId) => member.send(message, to)},
      leave: {value: () => base.unsubscribe()},
    });
  }
}
