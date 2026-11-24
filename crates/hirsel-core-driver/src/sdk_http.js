import {defaultClock} from './browser.js';
import {applyImage, commitVisible, freezeDecoded, invalidation, stageImage, subscriptionHandle} from './image.js';
import {decimalU64} from './persistence.js';

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
  if (binding.mode === 'subscribe') {
    binding.slotIndex = new Map(binding.projections.map(({slot}, i) => [slot, i]));
    return Object.freeze({
      request(args) { return fieldsFor(binding, args); },
      call(client, options, args, subscription) {
        return subscribeHttp(client, options, binding, fieldsFor(binding, args), subscription);
      },
    });
  }
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
        const fetcher = options.fetch ?? globalThis.fetch;
      const response = await fetcher(new URL(request.path, options.url), {
        method: binding.method, headers, body: request.body, redirect: 'error',
      });
      return readResult(binding, {status: response.status, body: await response.text(),
        committed: false, replayed: false});
    },
  });
}

async function* eventStream(body, signal, retry) {
  const reader = body.getReader();
  const cancel = () => { void reader.cancel().catch(() => {}); };
  signal.addEventListener('abort', cancel, {once: true});
  const decoder = new TextDecoder();
  let buffer = '', name = '', data = [], skipLF = false;
  try {
    while (!signal.aborted) {
      let part;
      try { part = await reader.read(); } catch { return 'lost'; }
      if (part.done || signal.aborted) return 'eof';
      const text = decoder.decode(part.value, {stream: true});
      if (!text) continue;
      buffer += skipLF && text[0] === '\n' ? text.slice(1) : text;
      skipLF = false;
      let start = 0;
      for (const match of buffer.matchAll(/\r\n|\r|\n/g)) {
        const line = buffer.slice(start, match.index);
        start = match.index + match[0].length;
        skipLF = match[0] === '\r' && start === buffer.length;
        if (!line) {
          if (data.length) yield {name: name || 'message', data: data.join('\n')};
          name = ''; data = [];
          continue;
        }
        const colon = line.indexOf(':');
        const field = colon < 0 ? line : line.slice(0, colon);
        const value = colon < 0 ? '' : line.slice(colon + 1).replace(/^ /, '');
        if (field === 'event') name = value;
        else if (field === 'data') data.push(value);
        else if (field === 'retry' && /^[0-9]+$/.test(value)) retry(Math.min(10000, Number(value)));
      }
      buffer = buffer.slice(start);
    }
    return 'eof';
  } finally {
    signal.removeEventListener('abort', cancel);
    await reader.cancel().catch(() => {});
    reader.releaseLock();
  }
}

function pause(clock, delay, signal) {
  if (signal.aborted) return Promise.resolve();
  return new Promise(resolve => {
    const done = () => {
      signal.removeEventListener('abort', done);
      clock.clear(timer);
      resolve();
    };
    const timer = clock.set(done, delay);
    signal.addEventListener('abort', done, {once: true});
  });
}

function sseVersion(value) {
  const version = typeof value === 'string' ? decimalU64(value) : undefined;
  if (version === undefined) throw new TypeError('invalid SSE version');
  return version;
}

function sseGroup(binding, envelope, initialized) {
  const version = sseVersion(envelope.version);
  const replaced = envelope.complete.length === 1 && envelope.complete[0] === 0;
  const group = envelope.groups[0];
  if (envelope.outcomes.length !== 0 || envelope.groups.length !== 1 || group.sub !== 0 ||
      (envelope.complete.length !== 0 && !replaced) || (!initialized && !replaced) ||
      (replaced && !own(group, 'value'))) throw new TypeError('invalid SSE batch');
  const {groups: [decoded]} = readQuery(binding, envelope);
  const rows = decoded.rows.map(row => {
    const slot = binding.slotIndex.get(row.slot);
    if (typeof row.present !== 'boolean' || row.present !== own(row, 'value')) throw new TypeError('invalid SSE row');
    return {slot, key: binding.keys[slot].encode(row.key), value: row.present
      ? {tag: 'Some', value: binding.rows[slot].encode(row.value)} : {tag: 'None'}};
  });
  const value = own(decoded, 'value') ? binding.value.encode(decoded.value) : new Uint8Array();
  return {version, replaced, group: {sub: 0, value, rows}};
}

function subscribeHttp(client, options, binding, request, subscription = {}) {
  const controller = new AbortController();
  const signal = subscription.signal ? AbortSignal.any([controller.signal, subscription.signal]) : controller.signal;
  const state = {active: !signal.aborted, server: undefined, image: undefined};
  const handle = subscriptionHandle(state, binding.codec, () => controller.abort());
  const clear = () => {
    const hadImage = state.image !== undefined;
    state.server = state.image = undefined;
    if (hadImage) subscription.onChange?.(handle, invalidation);
  };
  const stop = () => { state.active = false; state.server = state.image = undefined; };
  signal.addEventListener('abort', stop, {once: true});
  handle.done = run().finally(() => {
    signal.removeEventListener('abort', stop);
    stop();
  });
  void handle.done.catch(() => {});
  return handle;

  async function run() {
    const clock = options.clock ?? defaultClock;
    const fetcher = options.fetch ?? globalThis.fetch;
    const url = new URL(request.path, options.url);
    let retry = 1000, backoff = retry, retried = false;
    const delay = async () => {
      const wait = clock.random() * backoff;
      backoff = Math.min(10000, backoff * 2);
      await pause(clock, wait, signal);
    };
    while (!signal.aborted) {
      let credential;
      try {
        credential = await client.httpSubscribeCredential(signal);
      } catch (error) {
        if (signal.aborted) break;
        throw error;
      }
      const attempt = AbortSignal.any([signal, credential.signal]);
      const headers = new Headers({Accept: 'text/event-stream'});
      if (credential.authorization !== undefined) headers.set('Authorization', credential.authorization);
      credential.signal.addEventListener('abort', clear, {once: true});
      let result;
      try {
        result = await consume();
      } finally {
        credential.signal.removeEventListener('abort', clear);
      }
      if (signal.aborted) break;
      if (credential.signal.aborted) continue;
      if (result.tag === 'Expired') {
        clear();
        await credential.refused(result.cause, retried);
        retried = true;
      } else if (result.tag === 'Ended') {
        stop();
        subscription.onEnd?.(result.reason);
        return result;
      } else if (result.tag === 'Refused') {
        return result;
      } else {
        clear();
        if (result.tag === 'Lost' || result.tag === 'Reset') await delay();
      }

      async function consume() {
        let response;
        try {
          response = await fetcher(url, {method: binding.method, headers, redirect: 'error', signal: attempt});
        } catch {
          return {tag: 'Lost'};
        }
        if (attempt.aborted) {
          await response.body?.cancel().catch(() => {});
          return {tag: 'Lost'};
        }
        if (response.status === 401) {
          void response.body?.cancel().catch(() => {});
          return {tag: 'Expired', cause: 'HTTP Subscribe 401'};
        }
        if (response.status !== binding.okStatus) {
          let body;
          try { body = await response.text(); } catch { body = ''; }
          return {tag: 'Refused', status: response.status, body};
        }
        retried = false;
        if (!/^text\/event-stream(?:\s*;|\s*$)/i.test(response.headers.get('Content-Type') ?? '') || !response.body) {
          void response.body?.cancel().catch(() => {});
          throw new TypeError('expected a text/event-stream response');
        }
        let lastVersion;
        const events = eventStream(response.body, attempt, ms => { retry = backoff = ms; });
        try {
          for (;;) {
            const event = await events.next();
            if (attempt.aborted) return {tag: 'Lost'};
            if (event.done) return {tag: event.value === 'lost' ? 'Lost' : 'Eof'};
            const {name, data} = event.value;
            if (name === 'batch') {
              const {version, replaced, group} = sseGroup(binding, JSON.parse(data), lastVersion !== undefined);
              if (lastVersion !== undefined && version < lastVersion) throw new TypeError('SSE version went backwards');
              lastVersion = version;
              const edit = stageImage(binding.codec, state.server, group, replaced);
              state.server ??= {value: edit.value, slots: new Map()};
              applyImage(state.server, edit);
              const change = commitVisible(state, edit);
              backoff = retry;
              if (change) subscription.onChange?.(handle, change);
            } else if (name === 'ended') {
              const ended = JSON.parse(data);
              if (ended.sub !== 0) throw new TypeError('invalid SSE ended');
              return {tag: 'Ended', reason: freezeDecoded(convert(...outcomeSchema, ended.outcome, false))};
            } else if (name === 'reset') {
              const reset = JSON.parse(data);
              if (reset.outcomes.length !== 0 || sseVersion(reset.version) < (lastVersion ?? 0n)) {
                throw new TypeError('invalid SSE reset');
              }
              return {tag: 'Reset'};
            } else if (name === 'expired') {
              if (JSON.parse(data).reason !== 'auth_expired') throw new TypeError('invalid SSE expiry');
              return {tag: 'Expired', cause: 'HTTP Subscribe expired'};
            }
          }
        } finally {
          await events.return();
        }
      }
    }
    return {tag: 'Aborted'};
  }
}
