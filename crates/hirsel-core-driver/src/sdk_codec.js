// Shared by generated browser and Node bindings. Descriptors follow std.codec.
export class Malformed extends Error {
  constructor(message = 'Malformed') { super(message); this.name = 'Malformed'; }
}
const utf8 = new TextDecoder('utf-8', {fatal: true, ignoreBOM: true});
const text = new TextEncoder();
function stringBytes(value) {
  if (typeof value !== 'string') throw new TypeError('expected String');
  const bytes = text.encode(value);
  if (utf8.decode(bytes) !== value) throw new TypeError('unpaired surrogate');
  return bytes;
}
class Reader {
  constructor(bytes) {
    if (!(bytes instanceof Uint8Array)) throw new TypeError('expected Uint8Array');
    this.bytes = bytes; this.at = 0; this.inverted = false;
    this.view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  }
  take(n) {
    if (n > this.bytes.length - this.at) throw new Malformed('truncated');
    const at = this.at; this.at += n; return at;
  }
  count() { return this.view.getUint32(this.take(4), true); }
  byte() { return this.bytes[this.take(1)] ^ (this.inverted ? 255 : 0); }
  done() { if (this.at !== this.bytes.length) throw new Malformed('trailing bytes'); }
}
class Writer {
  constructor() { this.parts = []; this.size = 0; this.inverted = false; }
  put(bytes) {
    this.parts.push(this.inverted ? Uint8Array.from(bytes, byte => byte ^ 255) : bytes);
    this.size += bytes.length;
  }
  count(n) {
    if (!Number.isInteger(n) || n < 0 || n > 0xffffffff) throw new RangeError('u32 count');
    const b = new Uint8Array(4); new DataView(b.buffer).setUint32(0, n, true); this.put(b);
  }
  byte(n) { this.put(Uint8Array.of(n)); }
  finish() {
    const out = new Uint8Array(this.size); let at = 0;
    for (const part of this.parts) { out.set(part, at); at += part.length; }
    return out;
  }
}
const integer = {3:[1,false],4:[2,false],5:[4,false],6:[8,false],7:[1,true],8:[2,true],9:[4,true],10:[8,true]};
function readInteger(op, r, key) {
  const [width, signed] = integer[op], at = r.take(width);
  let n = width === 8 ? r.view.getBigUint64(at, !key) : BigInt(width === 4 ? r.view.getUint32(at, !key) : width === 2 ? r.view.getUint16(at, !key) : r.view.getUint8(at));
  const bits = BigInt(width * 8);
  if (r.inverted) n ^= (1n << bits) - 1n;
  if (signed) { if (key) n ^= 1n << (bits - 1n); n = BigInt.asIntN(Number(bits), n); }
  return width === 8 ? n : Number(n);
}
function writeInteger(op, value, w, key) {
  const [width, signed] = integer[op];
  if (width === 8 ? typeof value !== 'bigint' : typeof value !== 'number' || !Number.isInteger(value)) throw new TypeError(width === 8 ? 'expected bigint' : 'expected integer');
  const bits = BigInt(width * 8), n = BigInt(value), min = signed ? -(1n << (bits - 1n)) : 0n, max = (1n << (bits - (signed ? 1n : 0n))) - 1n;
  if (n < min || n > max) throw new RangeError('integer out of range');
  let raw = BigInt.asUintN(Number(bits), n);
  if (key && signed) raw ^= 1n << (bits - 1n);
  const bytes = new Uint8Array(width), view = new DataView(bytes.buffer);
  if (width === 8) view.setBigUint64(0, raw, !key);
  else if (width === 4) view.setUint32(0, Number(raw), !key);
  else if (width === 2) view.setUint16(0, Number(raw), !key);
  else view.setUint8(0, Number(raw));
  w.put(bytes);
}
function readBytes(r, key) {
  if (!key) { const n = r.count(); return r.bytes.slice(r.take(n), r.at); }
  const bytes = [];
  for (;;) {
    const b = r.byte();
    if (b !== 0) bytes.push(b);
    else { const next = r.byte(); if (next === 0) return Uint8Array.from(bytes); if (next !== 255) throw new Malformed('key escape'); bytes.push(0); }
  }
}
function writeBytes(bytes, w, key) {
  if (!(bytes instanceof Uint8Array)) throw new TypeError('expected Uint8Array');
  if (!key) { w.count(bytes.length); w.put(bytes); return; }
  for (const b of bytes) { w.byte(b); if (b === 0) w.byte(255); } w.byte(0); w.byte(0);
}
function decodeString(bytes) { try { return utf8.decode(bytes); } catch { throw new Malformed('invalid UTF-8'); } }
function keyString(bytes) {
  let key = ''; for (const byte of bytes) key += byte.toString(16).padStart(2,'0'); return key;
}
function read(t, defs, r, key) {
  const op = t[0];
  if (integer[op]) return readInteger(op, r, key);
  if (op === 0) return undefined;
  if (op === 20) throw new Malformed('Never');
  if (op === 1) { const b = r.byte(); if (b > 1) throw new Malformed('bool'); return b === 1; }
  if (op === 2) { const c = r.count(); if (c > 0x10ffff || c >= 0xd800 && c <= 0xdfff) throw new Malformed('char'); return String.fromCodePoint(c); }
  if (op === 11 || op === 12) {
    const at = r.take(op === 11 ? 4 : 8), value = op === 11 ? r.view.getFloat32(at,true) : r.view.getFloat64(at,true);
    if (Number.isNaN(value) && (op === 11 ? r.view.getUint32(at,true) !== 0x7fc00000 : r.view.getBigUint64(at,true) !== 0x7ff8000000000000n)) throw new Malformed('noncanonical NaN');
    return value;
  }
  if (op === 13) return decodeString(readBytes(r,key));
  if (op === 14) {
    if (t[1][0] === 3) return readBytes(r,key);
    const n = r.count(), out = []; for (let i=0;i<n;i++) out.push(read(t[1],defs,r,key)); return out;
  }
  if (op === 15) {
    const n = r.count(), out = new Map(), seen = new Set();
    for (let i=0;i<n;i++) {
      const start = r.at, k = read(t[1],defs,r,key), identity = keyString(r.bytes.subarray(start,r.at));
      if (seen.has(identity)) throw new Malformed('duplicate map key'); seen.add(identity);
      out.set(k,read(t[2],defs,r,key));
    } return out;
  }
  if (op === 16 || op === 17) {
    const tag = r.count(); if (tag > 1) throw new Malformed('tag');
    if (op === 16) return tag === 0 ? {tag:'None'} : {tag:'Some',value:read(t[1],defs,r,key)};
    return {tag:tag === 0 ? 'Ok' : 'Error',value:read(t[tag + 1],defs,r,key)};
  }
  if (op === 21 && key) {
    r.inverted = !r.inverted;
    try { return {value: read(t[1],defs,r,key)}; }
    finally { r.inverted = !r.inverted; }
  }
  if (op === 18) return t[1].map(part => read(part,defs,r,key));
  const d = defs[t[1]];
  if (d.kind === 2) return read(d.body,defs,r,key);
  if (d.kind === 0) {
    const out = {}; for (const field of d.body) if (!field.removed) Object.defineProperty(out,field.name,{value:read(field.type,defs,r,key),enumerable:true,writable:true,configurable:true}); return out;
  }
  const tag = r.count(), v = d.body[tag]; if (!v || v.removed) throw new Malformed('variant');
  const values = v.fields.map(part => read(part,defs,r,key));
  return values.length === 0 ? {tag:v.name} : values.length === 1 ? {tag:v.name,value:values[0]} : {tag:v.name,values};
}
function write(t, defs, value, w, key) {
  const op = t[0];
  if (integer[op]) { writeInteger(op,value,w,key); return; }
  if (op === 0) { if (value !== undefined) throw new TypeError('expected unit'); return; }
  if (op === 20) throw new TypeError('Never has no values');
  if (op === 1) { if (typeof value !== 'boolean') throw new TypeError('expected bool'); w.byte(value ? 1 : 0); return; }
  if (op === 2) {
    const bytes = stringBytes(value); const points = Array.from(value);
    if (points.length !== 1 || bytes.length === 0) throw new TypeError('expected char'); w.count(value.codePointAt(0)); return;
  }
  if (op === 11 || op === 12) {
    if (typeof value !== 'number') throw new TypeError('expected float');
    const b = new Uint8Array(op === 11 ? 4 : 8), view = new DataView(b.buffer);
    if (op === 11) { if (Number.isNaN(value)) view.setUint32(0,0x7fc00000,true); else view.setFloat32(0,value,true); }
    else { if (Number.isNaN(value)) view.setBigUint64(0,0x7ff8000000000000n,true); else view.setFloat64(0,value,true); } w.put(b); return;
  }
  if (op === 13) { writeBytes(stringBytes(value),w,key); return; }
  if (op === 14) {
    if (t[1][0] === 3) { writeBytes(value,w,key); return; }
    if (!Array.isArray(value)) throw new TypeError('expected List');
    w.count(value.length); for (const item of value) write(t[1],defs,item,w,key); return;
  }
  if (op === 15) {
    if (!(value instanceof Map)) throw new TypeError('expected Map');
    const seen = new Set(); w.count(value.size);
    for (const [k,v] of value) {
      const kw = new Writer(); write(t[1],defs,k,kw,key); const encoded = kw.finish(), identity = keyString(encoded);
      if (seen.has(identity)) throw new TypeError('duplicate map key'); seen.add(identity);
      w.put(encoded); write(t[2],defs,v,w,key);
    } return;
  }
  if (op === 16 || op === 17) {
    const names = op === 16 ? ['None','Some'] : ['Ok','Error'], tag = names.indexOf(value?.tag);
    if (tag < 0) throw new TypeError('unknown tag'); w.count(tag);
    if (op === 17 || tag === 1) write(t[op === 16 ? 1 : tag + 1],defs,value.value,w,key); return;
  }
  if (op === 21 && key) {
    w.inverted = !w.inverted;
    try { write(t[1],defs,value.value,w,key); }
    finally { w.inverted = !w.inverted; }
    return;
  }
  if (op === 18) {
    if (!Array.isArray(value) || value.length !== t[1].length) throw new TypeError('tuple arity');
    t[1].forEach((part,i) => write(part,defs,value[i],w,key)); return;
  }
  const d = defs[t[1]];
  if (d.kind === 2) { write(d.body,defs,value,w,key); return; }
  if (d.kind === 0) { for (const field of d.body) if (!field.removed) write(field.type,defs,value[field.name],w,key); return; }
  const tag = d.body.findIndex(v => !v.removed && v.name === value?.tag);
  if (tag < 0) throw new TypeError('unknown variant'); w.count(tag);
  const fields = d.body[tag].fields;
  const values = fields.length === 1 ? [value.value] : fields.length === 0 ? [] : value.values;
  if (!Array.isArray(values) || values.length !== fields.length) throw new TypeError('variant arity');
  fields.forEach((part,i) => write(part,defs,values[i],w,key));
}
export function makeCodec(root, definitions = [], key = false) {
  return Object.freeze({
    encode(value) { const w = new Writer(); write(root,definitions,value,w,key); return w.finish(); },
    decode(bytes) { const r = new Reader(bytes); const value = read(root,definitions,r,key); r.done(); return value; }
  });
}
export const values = Object.freeze(Object.fromEntries(['unit','bool','char','u8','u16','u32','u64','i8','i16','i32','i64','f32','f64','string'].map((name,op) => [name,makeCodec([op])]).concat([['bytes',makeCodec([14,[3]])]])));
export const keys = Object.freeze(Object.fromEntries(['bool','u8','u16','u32','u64','i8','i16','i32','i64','string'].map(name => [name,makeCodec([{bool:1,u8:3,u16:4,u32:5,u64:6,i8:7,i16:8,i32:9,i64:10,string:13}[name]],[],true)]).concat([['bytes',makeCodec([14,[3]],[],true)]])));
