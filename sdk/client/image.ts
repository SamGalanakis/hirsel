import type {Change, Group, RowChange, SlotRow, SubscriptionCodec} from './client.js';

export const equal = (a: Uint8Array, b: Uint8Array) => a.length === b.length && a.every((v, i) => v === b[i]);
export const key = (b: Uint8Array) => Array.from(b, n => n.toString(16).padStart(2, '0')).join('');

type ImageValue = {raw: Uint8Array; decoded: unknown};
interface SlotImage {
  entries: Map<string, ImageValue>;
  map?: ReadonlyMap<string, unknown>;
  rows?: readonly unknown[];
}
interface Image { value: ImageValue; slots: Map<PropertyKey, SlotImage> }
export type ImageSlots = Record<PropertyKey, unknown>;
export type ImageChange = Change<unknown, ImageSlots>;
type SlotEdits = Map<string, ImageValue | undefined>;
export interface ImageEdits { value?: ImageValue; slots: Map<PropertyKey, SlotEdits> }
export interface ImageState { server?: Image; image?: Image }

export const rawSubscription: SubscriptionCodec<Uint8Array, Record<number, SlotRow>> = {
  value: bytes => bytes.slice(),
  slot: slot => slot,
  row: row => ({...row, key: row.key.slice(), value: row.value.tag === 'Some'
    ? {tag: 'Some', value: row.value.value.slice()} : {tag: 'None'}}),
  key: (_slot, bytes) => bytes,
  encodeValue: bytes => bytes,
  encodeRow: (slot, key, row) => {
    if (row.slot !== slot || !equal(row.key, key) || row.value.tag !== 'Some') throw new TypeError('Invalid Draft row');
    const raw = row.value.value.slice();
    return {raw, decoded: rawSubscription.row({slot, key, value: {tag: 'Some', value: raw}})};
  },
};

export function freezeDecoded(value: unknown): unknown {
  if (value !== null && typeof value === 'object'
      && (Array.isArray(value) || Object.getPrototypeOf(value) === Object.prototype
        || Object.getPrototypeOf(value) === null)) {
    for (const child of Object.values(value)) freezeDecoded(child);
    Object.freeze(value);
  }
  return value;
}

export function stageImage(codec: SubscriptionCodec<unknown, ImageSlots, ImageSlots>,
  image: Image | undefined, group: Group, replaced: boolean) {
  const value = (replaced || group.value.length > 0) && (!image || !equal(image.value.raw, group.value))
    ? {raw: group.value, decoded: freezeDecoded(codec.value(group.value))} : undefined;
  const slots = new Map<PropertyKey, SlotEdits>();
  const prefix = codec.keyPrefixBytes ?? 0;
  for (const row of group.rows) {
    const slot = codec.slot(row.slot);
    if (row.key.length < prefix) throw new TypeError('Truncated row key');
    const hex = key(row.key.subarray(prefix));
    const before = image?.slots.get(slot)?.entries.get(hex);
    const raw = row.value.tag === 'Some' ? row.value.value : undefined;
    const after = raw && before && equal(before.raw, raw) ? before
      : raw ? {raw, decoded: freezeDecoded(codec.row(row))} : undefined;
    if (raw === undefined) codec.row(row);
    const edits = slotEdits(slots, slot);
    if (before !== after || (replaced && after)) edits.set(hex, after);
    else edits.delete(hex);
  }
  if (replaced && image) {
    for (const [slot, prior] of image.slots) {
      const edits = slotEdits(slots, slot);
      for (const hex of prior.entries.keys()) {
        if (!edits.has(hex)) edits.set(hex, undefined);
      }
      for (const [hex, after] of edits) {
        if (prior.entries.get(hex) === after) edits.delete(hex);
      }
    }
  }
  return {id: group.sub, replaced, value, slots};
}

export function applyImage(image: Image, {value, slots}: ImageEdits, rows?: RowChange<ImageSlots>[]): void {
  for (const [slot, edits] of slots) {
    if (edits.size === 0) continue;
    const target = image.slots.get(slot) ?? {entries: new Map<string, ImageValue>()};
    for (const [hex, after] of edits) {
      const before = rows && target.entries.get(hex);
      if (after) target.entries.set(hex, after);
      else target.entries.delete(hex);
      rows?.push(Object.freeze({slot, key: hex,
        ...(before && {old: before.decoded}), ...(after && {new: after.decoded})}) as RowChange<ImageSlots>);
    }
    target.map = target.rows = undefined;
    image.slots.set(slot, target);
  }
  if (value) image.value = value;
}

const sameImageValue = (a?: ImageValue, b?: ImageValue) =>
  a === b || (a !== undefined && b !== undefined && equal(a.raw, b.raw));

export function slotEdits(slots: ImageEdits['slots'], slot: PropertyKey): SlotEdits {
  let edits = slots.get(slot);
  if (!edits) slots.set(slot, edits = new Map());
  return edits;
}

export function commitVisible(sub: ImageState, serverEdit?: ReturnType<typeof stageImage>,
  prior?: ImageEdits, next?: ImageEdits): ImageChange | undefined {
  const {image, server} = sub;
  if (!server) return;
  const value = next?.value ?? server.value;
  const edits: ImageEdits = {slots: new Map()};
  // Only server edits and old or new optimistic writes can change the visible image.
  const slots = new Set([...serverEdit?.slots.keys() ?? [], ...prior?.slots.keys() ?? [], ...next?.slots.keys() ?? []]);
  for (const slot of slots) {
    const base = server.slots.get(slot);
    const oldSlot = image?.slots.get(slot);
    const nextEntries = next?.slots.get(slot);
    const keys = new Set([...prior?.slots.get(slot)?.keys() ?? [],
      ...serverEdit?.slots.get(slot)?.keys() ?? [], ...nextEntries?.keys() ?? []]);
    for (const hex of keys) {
      const after = nextEntries?.has(hex) ? nextEntries.get(hex) : base?.entries.get(hex);
      if (!sameImageValue(oldSlot?.entries.get(hex), after)) slotEdits(edits.slots, slot).set(hex, after);
    }
  }
  const changedValue = !sameImageValue(image?.value, value);
  const rows: RowChange<ImageSlots>[] = [];
  sub.image = image ?? {value, slots: new Map()};
  const changeValue = changedValue && Object.freeze({...(image && {old: image.value.decoded}), new: value.decoded});
  if (changedValue) edits.value = value;
  applyImage(sub.image, edits, rows);
  if (!changedValue && !rows.length) return;
  return Object.freeze({replaced: serverEdit?.replaced ?? false,
    ...(changeValue && {value: changeValue}), rows: Object.freeze(rows)});
}
