// The binding's JSON representation and its WebSocket codec share a schema.
const own = Object.hasOwn;

function encodeBase64(bytes) {
  let text = '';
  for (const byte of bytes) text += String.fromCharCode(byte);
  return btoa(text);
}

function convert(type, definitions, value, encode) {
  const [op, left, right] = type;
  switch (op) {
    case 0: return encode ? null : undefined;
    case 6: case 10: return encode ? String(value) : BigInt(value);
    case 11: case 12:
      if (!encode) return typeof value === 'string' ? Number(value) : value;
      return Number.isFinite(value) ? (op === 11 ? Math.fround(value) : value) : String(value);
    case 14:
      if (left[0] === 3) return encode ? encodeBase64(value) : Uint8Array.from(atob(value), c => c.charCodeAt(0));
      return value.map(item => convert(left, definitions, item, encode));
    case 15: {
      const pairs = Array.from(value, ([key, item]) => [
        convert(left, definitions, key, encode), convert(right, definitions, item, encode),
      ]);
      return encode ? pairs : new Map(pairs);
    }
    case 16:
      if (encode) return value.tag === 'None' ? null : convert(left, definitions, value.value, true);
      return value === null ? {tag: 'None'} : {tag: 'Some', value: convert(left, definitions, value, false)};
    case 17:
      if (encode) return value.tag === 'Ok'
        ? {ok: convert(left, definitions, value.value, true)}
        : {error: convert(right, definitions, value.value, true)};
      return own(value, 'ok') ? {tag: 'Ok', value: convert(left, definitions, value.ok, false)}
        : {tag: 'Error', value: convert(right, definitions, value.error, false)};
    case 19: {
      const definition = definitions[left];
      if (definition.kind === 2) return convert(definition.body, definitions, value, encode);
      if (definition.kind === 0) return Object.fromEntries(definition.body
        .filter(field => !field.removed && own(value, field.name))
        .map(field => [field.name, convert(field.type, definitions, value[field.name], encode)]));
      const tag = encode ? value.tag : typeof value === 'string' ? value : Object.keys(value)[0];
      const variant = definition.body.find(v => v.name === tag && !v.removed);
      if (!variant) throw new TypeError('unknown variant');
      if (variant.fields.length === 0) return encode ? tag : {tag};
      if (variant.fields.length === 1) {
        const member = convert(variant.fields[0], definitions, encode ? value.value : value[tag], encode);
        return encode ? {[tag]: member} : {tag, value: member};
      }
      const members = variant.fields.map((member, i) =>
        convert(member, definitions, (encode ? value.values : value[tag])[i], encode));
      return encode ? {[tag]: members} : {tag, values: members};
    }
    default: return value;
  }
}

function json(value) {
  // JSON.stringify writes negative zero as zero. Keep its sign on the wire.
  if (Object.is(value, -0)) return '-0';
  if (Array.isArray(value)) return '[' + value.map(json).join(',') + ']';
  if (value !== null && typeof value === 'object') {
    return '{' + Object.entries(value).map(([name, member]) => JSON.stringify(name) + ':' + json(member)).join(',') + '}';
  }
  return JSON.stringify(value);
}

function encodeComponent(value) {
  return encodeURIComponent(String(value)).replace(/[!'()*]/g, c =>
    '%' + c.charCodeAt(0).toString(16).toUpperCase());
}

function fieldsFor(binding, args) {
  const [root, definitions] = binding.args;
  const assemble = convert(root, definitions, args, true);
  let path = binding.mode === 'stream' ? '/.hirsel/op/' + binding.op : binding.path;
  const query = [];
  const body = [];
  let hasBody = false;
  for (const [field, source] of binding.fields) {
    const value = assemble[field];
    if (source === 'body') {
      hasBody = true;
      if (own(assemble, field)) body.push([field, value]);
    } else if (source === 'path' && binding.mode !== 'stream') {
      if (value === undefined) throw new TypeError('missing path field ' + field);
      path = path.replace('{' + field + '}', encodeComponent(value));
    } else if (value !== null && value !== undefined) {
      for (const item of Array.isArray(value) ? value : [value]) {
        query.push(encodeComponent(field) + '=' + encodeComponent(item));
      }
    }
  }
  if (query.length !== 0) path += '?' + query.join('&');
  return {path, body: hasBody ? json(Object.fromEntries(body)) : undefined};
}

function readQuery(binding, envelope) {
  const [root, definitions] = binding.result;
  const slots = new Map(binding.projections.map(slot => [slot.slot, slot]));
  const groups = envelope.groups.map(group => {
    const rows = group.rows.map(row => {
      const slot = slots.get(row.slot);
      if (!slot) throw new TypeError('unknown row slot');
      const [keyType, keyDefs] = slot.key;
      const key = convert(keyType, keyDefs, row.key, false);
      if (!row.present) return {...row, key};
      const [valueType, valueDefs] = slot.value;
      return {...row, key, value: convert(valueType, valueDefs, row.value, false)};
    });
    return own(group, 'value')
      ? {...group, rows, value: convert(root, definitions, group.value, false)}
      : {...group, rows};
  });
  return {...envelope, version: BigInt(envelope.version), groups};
}

function readResult(binding, reply) {
  if (reply.body.length === 0) return reply;
  const [root, definitions] = binding.result;
  const value = JSON.parse(reply.body);
  if (binding.mode === 'stream') {
    if (!reply.committed) return reply;
    const result = root[0] === 19 ? definitions[root[1]].body : root;
    if (reply.status === binding.okStatus) {
      return {...reply, value: {tag: 'Ok', value: convert(result[1], definitions, value, false)}};
    }
    if (reply.status === binding.errorStatus) {
      return {...reply, value: {tag: 'Error', value: convert(result[2], definitions, value, false)}};
    }
  } else if (reply.status === binding.okStatus) {
    return {...reply, value: binding.query ? readQuery(binding, value) : convert(root, definitions, value, false)};
  }
  return reply;
}

export function makeHttpBinding(binding) {
  return Object.freeze({
    request(args) { return fieldsFor(binding, args); },
    async call(client, options, args) {
      const request = fieldsFor(binding, args);
      if (binding.mode === 'stream') {
        const outcome = await client.callHttp(binding.want, request);
        return outcome.tag === 'Returned' ? readResult(binding, outcome.value) : outcome;
      }
      const headers = new Headers();
      if (options.credential !== undefined) headers.set('Authorization', options.credential);
      if (request.body !== undefined) headers.set('Content-Type', 'application/json');
      if (binding.mode === 'subscribe') headers.set('Accept', 'text/event-stream');
      const fetcher = options.fetch ?? globalThis.fetch;
      const response = await fetcher(new URL(request.path, options.url), {
        method: binding.method, headers, body: request.body, redirect: 'error',
      });
      if (binding.mode === 'subscribe') return response;
      return readResult(binding, {status: response.status, body: await response.text(),
        committed: false, replayed: false});
    },
  });
}
