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
// One change to peers(). `renewed` marks a new roster life (incarnation): every earlier
// peer is in `left` and peer ids start again. `selfChanged` marks a change to this
// membership's peer id. `revision` is the room's after this change, or undefined while
// the roster image is not yet complete.
export interface RoomPeersChange {
  readonly incarnation: string; readonly revision: bigint | undefined; readonly renewed: boolean;
  readonly selfChanged: boolean;
  readonly joined: readonly PeerId[]; readonly left: readonly PeerId[]; readonly set: readonly PeerId[];
}
export interface RoomHandlers<S, M, V, Slots extends object, Keys extends Record<keyof Slots, unknown>> {
  // The Grant (the room's value) and any declared slot rows, as a subscription's onChange.
  onChange?: (room: LiveRoom<S, M, V, Slots, Keys>, change: Change<V, Slots>) => void;
  onPeers?: (peers: ReadonlyMap<PeerId, RoomPeer<S>>, change: RoomPeersChange) => void;
  // Every change to this membership's peer id, including undefined when a session end,
  // a full room or a membership image without an admission clears it. `previous` is the
  // id it replaces.
  onSeat?: (self: PeerId | undefined, previous: PeerId | undefined) => void;
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
  // The roster's life, 32 hex digits of its 16 bytes; (incarnation, peer) names one admission.
  readonly incarnation: string | undefined;
  // The membership revision at which peers() is complete; undefined while an image is partial.
  readonly revision: bigint | undefined;
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

// The 16 incarnation bytes, in order, from the two little-endian words that carry them.
export function incarnationHex([low, high]: [bigint, bigint]): string {
  let hex = '';
  for (const word of [low, high]) for (let i = 0n; i < 64n; i += 8n) hex += ((word >> i) & 0xffn).toString(16).padStart(2, '0');
  return hex;
}

// One Live frame (LIVE_MAX) and the relay's ring E: a Set waits and a Send drops above it.
const liveBufferBytes = 65536;
const recheckMs = 16;

export class RoomMember {
  self?: number;
  full = false;
  incarnation?: string;
  revision?: bigint;
  readonly peers = new Map<number, RoomPeer<unknown>>();
  // Peers a new membership's image has not yet shown; absent at its snapshot means left.
  private unconfirmed?: Set<number>;
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
  // Joined, then states, then messages, then left (one Part, in order). A Part from
  // another incarnation replaces the roster. A `complete` Part on the same incarnation
  // starts a new membership's image: a peer it shows continues, and one it has not shown
  // by the Part with `snapshot` set has left.
  apply(part: Part, [states, msgs]: Decoded): void {
    const joined: number[] = [], set = new Set<number>(), left: number[] = [];
    let renewed = false;
    const before = this.revision, seatBefore = this.self;
    if (part.complete) { this.self = undefined; this.full = false; }
    if (!part.full) {
      const incarnation = incarnationHex(part.incarnation);
      if (incarnation !== this.incarnation) {
        renewed = true;
        left.push(...this.peers.keys());
        this.peers.clear();
        this.unconfirmed = undefined;
        this.incarnation = incarnation;
        this.revision = undefined;
      } else if (part.complete) {
        this.unconfirmed = new Set(this.peers.keys());
        this.revision = undefined;
      }
    }
    if (part.you.tag === 'Some') { this.self = part.you.value; this.full = false; this.owed = this.stored !== undefined; }
    for (const [peer, caller] of part.joined) {
      // A continued peer is shown again with its current state, so the old one is dropped.
      const known = this.peers.get(peer);
      this.unconfirmed?.delete(peer);
      this.peers.set(peer, {caller, state: undefined});
      if (!known) joined.push(peer); else if (known.state !== undefined) set.add(peer);
    }
    for (const [peer, state] of states) {
      const known = this.peers.get(peer);
      if (known) { this.peers.set(peer, {caller: known.caller, state}); set.add(peer); }
    }
    for (const [from, message, direct] of msgs) {
      if (!this.sub.active) return;
      this.handlers.onMessage?.(from, message, direct);
    }
    for (const peer of part.left) if (this.peers.delete(peer)) { left.push(peer); set.delete(peer); }
    if (part.snapshot && !part.full) {
      for (const peer of this.unconfirmed ?? []) { this.peers.delete(peer); left.push(peer); set.delete(peer); }
      this.unconfirmed = undefined;
      this.revision = part.revision;
    }
    if (part.full) { this.full = true; this.self = undefined; }
    if (!this.sub.active) return;
    const selfChanged = seatBefore !== this.self;
    if (selfChanged) this.handlers.onSeat?.(this.self, seatBefore);
    if (selfChanged || renewed || joined.length || set.size || left.length || this.revision !== before) {
      this.handlers.onPeers?.(this.peers, {incarnation: this.incarnation!, revision: this.revision, renewed, selfChanged, joined, left, set: [...set]});
    }
    if (part.gap > 0) this.handlers.onGap?.(part.gap);
    if (part.lost > 0) this.handlers.onLost?.(part.lost);
    if (part.full) this.handlers.onFull?.();
    this.flush();
  }
  // A membership ends with its session. The roster stays as last told until the next
  // membership's image continues or replaces it, and the stored state is owed to that
  // membership, which the client gets on resubscribe.
  end(): void {
    const seatBefore = this.self;
    this.self = undefined; this.full = false;
    this.owed = this.stored !== undefined;
    if (seatBefore !== undefined) this.handlers.onSeat?.(undefined, seatBefore);
    this.drop();
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
      incarnation: {get: () => member.incarnation},
      revision: {get: () => member.revision},
      peers: {value: () => member.peers},
      set: {value: (state: unknown) => member.set(state)},
      send: {value: (message: unknown, to?: PeerId) => member.send(message, to)},
      leave: {value: () => base.unsubscribe()},
    });
  }
}
