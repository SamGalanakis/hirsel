export interface Codec<T> { encode(value: T): Uint8Array; decode(bytes: Uint8Array): T }
export type Option<T> = {tag: 'None'} | {tag: 'Some'; value: T};
export type Result<T, E> = {tag: 'Ok'; value: T} | {tag: 'Error'; value: E};
export class Malformed extends Error {}
export function makeCodec<T>(root: unknown[], definitions?: unknown[], key?: boolean): Codec<T>;
type Scalars = {unit: undefined; bool: boolean; char: string; u8: number; u16: number; u32: number; u64: bigint; i8: number; i16: number; i32: number; i64: bigint; f32: number; f64: number; string: string; bytes: Uint8Array};
export const values: {readonly [K in keyof Scalars]: Codec<Scalars[K]>};
export const keys: {readonly [K in 'bool' | 'u8' | 'u16' | 'u32' | 'u64' | 'i8' | 'i16' | 'i32' | 'i64' | 'string' | 'bytes']: Codec<Scalars[K]>};
