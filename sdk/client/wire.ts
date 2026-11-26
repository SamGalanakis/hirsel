import type {Caller, StreamId} from './persistence.js';

// The wire frames and the values they carry (hirsel.4).
export type Option<T> = {tag: 'None'} | {tag: 'Some'; value: T};
export interface Want { identity: Uint8Array; args_fp: bigint; result_fp: bigint }
export interface Room { requested: bigint; available: bigint }
export type Fault = {tag: 'Raised'; value: number} | {tag: 'OutOfBytes'; value: Room}
  | {tag: 'OutOfTime'; value: bigint} | {tag: 'Gone'} | {tag: 'Conflict'; value: string};
export type Refusal = {tag: 'NoEntry' | 'Overloaded' | 'Deadline' | 'NotAllowed' | 'RequestMismatch' | 'HistoryUnavailable'}
  | {tag: 'Malformed'; value: {kind: number; at: bigint}} | {tag: 'Budget'; value: Room}
  | {tag: 'StreamFuture'; value: bigint};
export type WireOutcome<R> = {tag: 'Returned'; value: R} | {tag: 'Failed'; value: Fault}
  | {tag: 'Refused'; value: Refusal} | {tag: 'Abandoned' | 'InDoubt'};
export type Outcome<R> = WireOutcome<R> | {tag: 'LocallyUnknown' | 'RequestMismatch'};
export interface SlotRow { slot: number; key: Uint8Array; value: Option<Uint8Array> }
export interface Group { sub: number; value: Uint8Array; rows: SlotRow[] }
export interface Done {stream: StreamId; p: bigint; outcome: Outcome<Uint8Array>}
export type Entry = {tag: 'Ready'; value: number} | {tag: 'Resolve'; values: [number, Uint8Array]} | {tag: 'Gone'};
export interface Part {
  sub: number; complete: boolean; you: Option<number>;
  incarnation: [bigint, bigint]; revision: bigint; snapshot: boolean;
  joined: [number, Caller][];
  states: [number, Option<Uint8Array>][]; msgs: [number, boolean, Uint8Array][];
  left: number[]; gap: number; lost: number; full: boolean;
}
export type Frame = {tag: 'Hello'; values: [Option<Uint8Array>, Want[]]}
  | {tag: 'Call'; values: [StreamId, bigint, number, number, Uint8Array]}
  | {tag: 'Subscribe'; values: [number, number, Uint8Array, Option<Uint8Array>]}
  | {tag: 'Unsubscribe'; value: number}
  | {tag: 'Welcome'; values: [Caller, number, bigint, Uint8Array, string, bigint, bigint, bigint]}
  | {tag: 'Placed'; value: Entry}
  | {tag: 'Batch'; values: [bigint, number[], Group[], Done[]]}
  | {tag: 'Ended'; values: [number, WireOutcome<Uint8Array>]}
  | {tag: 'Reset'; values: [bigint, Done[]]}
  | {tag: 'Set'; values: [number, Option<Uint8Array>]}
  | {tag: 'Send'; values: [number, Option<number>, Uint8Array]}
  | {tag: 'Live'; value: Part[]};
export interface Codec { encode(frame: Frame): Uint8Array; decode(bytes: Uint8Array): Frame }
export type SubscriptionEnd = Extract<Frame, {tag: 'Ended'}>['values'][1];

type Incoming = {epoch: number; data: unknown[]};
interface Receive {
  schedule: (run: () => Promise<void>) => void;
  frame: (frame: Frame) => Promise<void>;
  invalid: () => Promise<void>;
  unobserved: () => boolean;
}

// Neighboring socket messages can share a completion write while they wait for
// the client queue. An intervening client job closes the group.
export class Inbox {
  private tail?: Incoming;

  seal(): void { this.tail = undefined; }

  receiver(epoch: number, codec: Codec, receive: Receive): ({data}: {data: unknown}) => void {
    return ({data}) => {
      if (this.tail?.epoch === epoch) { this.tail.data.push(data); return; }
      const batch = {epoch, data: [data]};
      receive.schedule(async () => {
        if (this.tail === batch) this.seal();
        await this.drain(batch.data, codec, receive);
      });
      this.tail = batch;
    };
  }

  private async drain(messages: unknown[], codec: Codec, receive: Receive): Promise<void> {
    let version: bigint | undefined, done: Done[] = [];
    const flush = async () => {
      if (version !== undefined) await receive.frame({tag: 'Batch', values: [version, [], [], done]});
      version = undefined; done = [];
    };
    for (const message of messages) {
      let frame: Frame;
      try {
        if (!(message instanceof ArrayBuffer) && !(message instanceof Uint8Array)) throw new Error('binary Frame required');
        frame = codec.decode(message instanceof Uint8Array ? message : new Uint8Array(message));
      } catch { await flush(); await receive.invalid(); return; }
      // Preserve every boundary where an image, a refusal or a callback can
      // observe the version between these calls.
      if (frame.tag === 'Batch' && receive.unobserved() && frame.values[1].length === 0 &&
          frame.values[2].length === 0 && frame.values[3].every(({outcome}) =>
            outcome.tag === 'Returned' || outcome.tag === 'Failed')) {
        version = frame.values[0];
        for (const row of frame.values[3]) done.push(row);
      } else {
        await flush();
        await receive.frame(frame);
      }
    }
    await flush();
  }
}
