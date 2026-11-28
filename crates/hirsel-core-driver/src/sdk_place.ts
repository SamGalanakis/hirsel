import {BrowserStorage} from './browser.js';
import {Client, ClientError, type Options} from './client.js';
import {Frame} from './frame.js';
import {bind, identity, kind, want} from './entries.js';

/** Decode auth.Challenge.challenge before passing it to navigator.credentials. */
export function passkeyChallenge(text: string): Uint8Array<ArrayBuffer> {
  if (!/^[A-Za-z0-9_-]{42}[AEIMQUYcgkosw048]$/.test(text)) {
    throw new ClientError('InvalidChallenge');
  }
  return Uint8Array.from(atob(text.replace(/-/g, '+').replace(/_/g, '/') + '='), ch => ch.charCodeAt(0));
}

export interface ScopedToken {
  authorization: string;
  expiresAtMs: number;
}

export interface PlaceOptions {
  refreshToken?: () => Promise<ScopedToken>;
  onState?: Options['onState'];
  onRecovered?: Options['onRecovered'];
}

export async function openPlace(path: string | URL, options: PlaceOptions = {}) {
  const url = new URL(path, location.href);
  if (url.protocol !== 'https:' || url.origin !== location.origin || url.username ||
      url.password || url.search || url.hash) throw new ClientError('InvalidPlaceOrigin');
  const socket = new URL(url);
  socket.protocol = 'wss:';
  const refresh = options.refreshToken;
  let active: ScopedToken | undefined;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let client: Client | undefined;
  const lifetime = new AbortController();

  async function expire(token: ScopedToken) {
    if (!client || active !== token || lifetime.signal.aborted) return;
    const current = await client.httpSubscribeCredential(lifetime.signal);
    if (active === token && current.authorization === token.authorization) {
      await current.refused('token expired', false);
    }
  }

  async function acquire() {
    active = undefined;
    clearTimeout(timer);
    const token = await refresh!();
    if (lifetime.signal.aborted) throw new ClientError('Closed');
    const remaining = token.expiresAtMs - Date.now();
    if (!Number.isSafeInteger(token.expiresAtMs) || remaining <= 0 || remaining > 300000 ||
        !/^Bearer [A-Za-z0-9._~-]+$/.test(token.authorization)) {
      throw new ClientError('InvalidScopedToken');
    }
    const issued = {authorization: token.authorization, expiresAtMs: token.expiresAtMs};
    active = issued;
    const bytes = new TextEncoder().encode(token.authorization);
    timer = setTimeout(() => { void expire(issued).catch(() => {}); }, remaining);
    return bytes;
  }

  const send: typeof fetch = async (input, init) => {
    const request = new Request(input, init);
    if (new URL(request.url).origin !== url.origin) throw new ClientError('InvalidPlaceOrigin');
    const headers = new Headers(request.headers);
    if (refresh) {
      if (!client) throw new ClientError('CredentialNeeded');
      const current = await client.httpSubscribeCredential(request.signal);
      if (!active || Date.now() >= active.expiresAtMs || current.authorization !== active.authorization ||
          (headers.has('Authorization') && headers.get('Authorization') !== active.authorization)) {
        throw new ClientError('CredentialNeeded');
      }
      headers.set('Authorization', active.authorization);
      return fetch(request, {headers, signal: AbortSignal.any([request.signal, current.signal]),
        mode: 'same-origin', credentials: 'omit', redirect: 'error'});
    }
    return fetch(request, {mode: 'same-origin', credentials: 'same-origin', redirect: 'error'});
  };

  const connection = await Client.open({
    url: socket.href, storage: new BrowserStorage('hirsel:' + url.href + ':' + identity.n_place),
    frame: Frame, wants: want, kinds: kind, identity, credential: refresh && acquire,
    http: {url: url.href, fetch: send}, onState: options.onState, onRecovered: options.onRecovered,
  });
  client = connection;
  const {call, subscribe, subscribeDecoded} = bind(connection);
  const http = bindHttp(connection, {url: url.href, fetch: send});
  return {call, http, subscribe, subscribeDecoded, whenReady: () => connection.whenReady(),
    async close() { lifetime.abort(); clearTimeout(timer); active = undefined; await connection.close(); }};
}
