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
  constructor() {
    this.bytes = new Uint8Array(64); this.view = new DataView(this.bytes.buffer);
    this.size = 0; this.inverted = false;
  }
  reserve(n) {
    const at = this.size, end = at + n;
    if (end > this.bytes.length) {
      const bytes = new Uint8Array(Math.max(end, this.bytes.length * 2));
      bytes.set(this.bytes.subarray(0, at));
      this.bytes = bytes; this.view = new DataView(bytes.buffer);
    }
    this.size = end;
    return at;
  }
  put(bytes) {
    const at = this.reserve(bytes.length);
    if (!this.inverted) this.bytes.set(bytes, at);
    else for (let i = 0; i < bytes.length; i++) this.bytes[at + i] = bytes[i] ^ 255;
  }
  count(n) {
    if (!Number.isInteger(n) || n < 0 || n > 0xffffffff) throw new RangeError('u32 count');
    const at = this.reserve(4);
    this.view.setUint32(at, this.inverted ? ~n : n, true);
  }
  byte(n) { const at = this.reserve(1); this.bytes[at] = this.inverted ? n ^ 255 : n; }
  finish() {
    return this.size === this.bytes.length ? this.bytes : this.bytes.slice(0, this.size);
  }
}
function numberInteger(width, min, max, get, set) {
  const signed = min < 0;
  // Signed getters extend the sign bit, so their inversion mask is -1.
  const mask = signed ? -1 : max;
  return {
    reader(key) {
      if (!key) return r => get.call(r.view, r.take(width), true);
      return r => {
        const value = get.call(r.view, r.take(width), false) ^ min ^ (r.inverted ? mask : 0);
        return signed ? value : value >>> 0;
      };
    },
    write(value, w, key) {
      if (!Number.isInteger(value)) throw new TypeError('expected integer');
      if (value < min || value > max) throw new RangeError('integer out of range');
      const raw = key ? value ^ min : value;
      if (width === 1) {
        w.byte(raw);
      } else {
        const at = w.reserve(width);
        set.call(w.view, at, w.inverted ? ~raw : raw, !key);
      }
    }
  };
}
function bigInteger(min, max, get) {
  const mask = min < 0n ? -1n : max;
  return {
    reader(key) {
      if (!key) return r => get.call(r.view, r.take(8), true);
      return r => get.call(r.view, r.take(8), false) ^ min ^ (r.inverted ? mask : 0n);
    },
    write(value, w, key) {
      if (typeof value !== 'bigint') throw new TypeError('expected bigint');
      if (value < min || value > max) throw new RangeError('integer out of range');
      const raw = key ? value ^ min : value, at = w.reserve(8);
      w.view.setBigUint64(at, w.inverted ? ~raw : raw, !key);
    }
  };
}
const dataView = DataView.prototype;
const integers = {
  3: numberInteger(1, 0, 0xff, dataView.getUint8),
  4: numberInteger(2, 0, 0xffff, dataView.getUint16, dataView.setUint16),
  5: numberInteger(4, 0, 0xffffffff, dataView.getUint32, dataView.setUint32),
  6: bigInteger(0n, 0xffffffffffffffffn, dataView.getBigUint64),
  7: numberInteger(1, -0x80, 0x7f, dataView.getInt8),
  8: numberInteger(2, -0x8000, 0x7fff, dataView.getInt16, dataView.setUint16),
  9: numberInteger(4, -0x80000000, 0x7fffffff, dataView.getInt32, dataView.setUint32),
  10: bigInteger(-0x8000000000000000n, 0x7fffffffffffffffn, dataView.getBigInt64)
};
function readBytes(r, key, borrow = false) {
  if (!key) {
    const n = r.count(), bytes = r.bytes.subarray(r.take(n), r.at);
    return borrow ? bytes : new Uint8Array(bytes);
  }
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
function readBool(r) {
  const b = r.byte();
  if (b > 1) throw new Malformed('bool');
  return b === 1;
}
function readChar(r) {
  const c = r.count();
  if (c > 0x10ffff || c >= 0xd800 && c <= 0xdfff) throw new Malformed('char');
  return String.fromCodePoint(c);
}
function readF32(r) {
  const at = r.take(4), value = r.view.getFloat32(at, true);
  if (Number.isNaN(value) && r.view.getUint32(at, true) !== 0x7fc00000) throw new Malformed('noncanonical NaN');
  return value;
}
function readF64(r) {
  const at = r.take(8), value = r.view.getFloat64(at, true);
  if (Number.isNaN(value) && r.view.getBigUint64(at, true) !== 0x7ff8000000000000n) throw new Malformed('noncanonical NaN');
  return value;
}
function readParts(parts, r) {
  const values = [];
  for (const part of parts) values.push(part(r));
  return values;
}
function compileReader(root, definitions, key) {
  const readers = [];

  function definition(index) {
    if (readers[index]) return readers[index];
    const d = definitions[index];
    let read;
    readers[index] = r => read(r);
    if (d.kind === 2) {
      read = compile(d.body);
    } else if (d.kind === 0) {
      const fields = d.body.filter(field => !field.removed)
        .map(field => ({name: field.name, read: compile(field.type)}));
      read = r => {
        const out = {};
        for (const field of fields) {
          Object.defineProperty(out, field.name, {value: field.read(r), enumerable: true, writable: true, configurable: true});
        }
        return out;
      };
    } else {
      const variants = d.body.map(v => {
        if (v.removed) return undefined;
        const tag = v.name, parts = v.fields.map(compile);
        if (parts.length === 0) return () => ({tag});
        if (parts.length === 1) return r => ({tag, value: parts[0](r)});
        return r => ({tag, values: readParts(parts, r)});
      });
      read = r => {
        const variant = variants[r.count()];
        if (!variant) throw new Malformed('variant');
        return variant(r);
      };
    }
    readers[index] = read;
    return read;
  }

  function compile(t) {
    const op = t[0];
    if (integers[op]) return integers[op].reader(key);
    switch (op) {
      case 0: return () => undefined;
      case 1: return readBool;
      case 2: return readChar;
      case 11: return readF32;
      case 12: return readF64;
      case 13: return r => decodeString(readBytes(r, key, true));
      case 14: {
        if (t[1][0] === 3) return r => readBytes(r, key);
        const item = compile(t[1]);
        return r => {
          const n = r.count(), out = [];
          for (let i = 0; i < n; i++) out.push(item(r));
          return out;
        };
      }
      case 15: {
        const readKey = compile(t[1]), readValue = compile(t[2]);
        return r => {
          const n = r.count(), out = new Map(), seen = new Set();
          for (let i = 0; i < n; i++) {
            const start = r.at, k = readKey(r), identity = keyString(r.bytes.subarray(start, r.at));
            if (seen.has(identity)) throw new Malformed('duplicate map key');
            seen.add(identity);
            out.set(k, readValue(r));
          }
          return out;
        };
      }
      case 16: {
        const some = compile(t[1]);
        return r => {
          const tag = r.count();
          if (tag > 1) throw new Malformed('tag');
          return tag === 0 ? {tag: 'None'} : {tag: 'Some', value: some(r)};
        };
      }
      case 17: {
        const ok = compile(t[1]), error = compile(t[2]);
        return r => {
          const tag = r.count();
          if (tag > 1) throw new Malformed('tag');
          return tag === 0 ? {tag: 'Ok', value: ok(r)} : {tag: 'Error', value: error(r)};
        };
      }
      case 18: {
        const parts = t[1].map(compile);
        return r => readParts(parts, r);
      }
      case 20: return () => { throw new Malformed('Never'); };
      case 21: {
        if (!key) return definition(t[1]);
        const inner = compile(t[1]);
        return r => {
          r.inverted = !r.inverted;
          try { return {value: inner(r)}; }
          finally { r.inverted = !r.inverted; }
        };
      }
      default: return definition(t[1]);
    }
  }

  return compile(root);
}
function write(t, defs, value, w, key) {
  const op = t[0];
  if (integers[op]) { integers[op].write(value,w,key); return; }
  if (op === 0) { if (value !== undefined) throw new TypeError('expected unit'); return; }
  if (op === 20) throw new TypeError('Never has no values');
  if (op === 1) { if (typeof value !== 'boolean') throw new TypeError('expected bool'); w.byte(value ? 1 : 0); return; }
  if (op === 2) {
    const bytes = stringBytes(value); const points = Array.from(value);
    if (points.length !== 1 || bytes.length === 0) throw new TypeError('expected char'); w.count(value.codePointAt(0)); return;
  }
  if (op === 11 || op === 12) {
    if (typeof value !== 'number') throw new TypeError('expected float');
    const at = w.reserve(op === 11 ? 4 : 8), view = w.view;
    if (op === 11) { if (Number.isNaN(value)) view.setUint32(at,0x7fc00000,true); else view.setFloat32(at,value,true); }
    else { if (Number.isNaN(value)) view.setBigUint64(at,0x7ff8000000000000n,true); else view.setFloat64(at,value,true); }
    if (w.inverted) for (let i = at; i < w.size; i++) w.bytes[i] ^= 255;
    return;
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
      const start = w.size;
      write(t[1],defs,k,w,key);
      const identity = keyString(w.bytes.subarray(start,w.size));
      if (seen.has(identity)) throw new TypeError('duplicate map key'); seen.add(identity);
      write(t[2],defs,v,w,key);
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
  const read = compileReader(root, definitions, key);
  return Object.freeze({
    encode(value) { const w = new Writer(); write(root,definitions,value,w,key); return w.finish(); },
    decode(bytes) { const r = new Reader(bytes); const value = read(r); r.done(); return value; }
  });
}
export const values = Object.freeze(Object.fromEntries(['unit','bool','char','u8','u16','u32','u64','i8','i16','i32','i64','f32','f64','string'].map((name,op) => [name,makeCodec([op])]).concat([['bytes',makeCodec([14,[3]])]])));
export const keys = Object.freeze(Object.fromEntries(['bool','u8','u16','u32','u64','i8','i16','i32','i64','string'].map(name => [name,makeCodec([{bool:1,u8:3,u16:4,u32:5,u64:6,i8:7,i16:8,i32:9,i64:10,string:13}[name]],[],true)]).concat([['bytes',makeCodec([14,[3]],[],true)]])));
