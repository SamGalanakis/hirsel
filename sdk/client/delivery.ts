import {decimalU64, streamHex, type Caller, type HttpRequest, type StreamId} from './persistence.js';
import type {Refusal} from './wire.js';

export interface HttpReply { status: number; body: string; committed: boolean; replayed: boolean }
export interface HttpAttempt extends HttpReply { refusal?: string; at?: bigint; retention?: bigint }
export interface HttpOptions { url: string; fetch?: typeof fetch }
export const authorization = (credential: Uint8Array | undefined) =>
  credential && Array.from(credential, b => String.fromCharCode(b)).join('');

// One delivery of a pending HTTP operation under its idempotency key.
export async function sendHttp(options: HttpOptions, credential: Uint8Array | undefined, request: HttpRequest,
  id: StreamId, p: bigint, caller: Caller, signal: AbortSignal): Promise<HttpAttempt> {
  const headers = new Headers();
  const auth = authorization(credential);
  if (auth !== undefined) headers.set('Authorization', auth);
  if (request.body !== undefined) headers.set('Content-Type', 'application/json');
  const key = `"${streamHex(id)}.${p}"`;
  headers.set('Idempotency-Key', key);
  headers.set('Hirsel-Expected-Caller', btoa(String.fromCharCode(caller.issuer, caller.subject.length, ...caller.subject)));
  const send = options.fetch ?? globalThis.fetch;
  const result = await send(new URL(request.path, options.url), {
    method: 'POST', headers, body: request.body, redirect: 'error', signal,
  });
  const marker = /^([a-z_]+);key=(.+)$/.exec(result.headers.get('Hirsel-Refused') ?? '');
  return {status: result.status, body: await result.text(),
    committed: result.headers.get('Idempotent-Outcome') === 'committed',
    replayed: result.headers.get('Idempotent-Replayed') === 'true',
    refusal: marker?.[2] === key ? marker[1] : undefined,
    at: decimalU64(result.headers.get('Hirsel-Now')),
    retention: decimalU64(result.headers.get('Hirsel-Retention'))};
}

// What one HTTP answer proves about its pending call. `proven` retries know this delivery did not run.
export type HttpVerdict =
  | {tag: 'Committed' | 'CredentialLost' | 'Fenced' | 'RequestMismatch' | 'HistoryUnavailable'}
  | {tag: 'Retry'; proven: boolean}
  | {tag: 'Refused'; refusal: Refusal};

export function httpVerdict(reply: HttpReply, refusal: string | undefined): HttpVerdict {
  // 401: the binding refused or expired the credential before anything was recorded.
  if (!reply.committed && reply.status === 401) return {tag: 'CredentialLost'};
  if (reply.committed) return {tag: 'Committed'};
  if (reply.status === 403 && reply.body === '{"refused":"fenced"}') return {tag: 'Fenced'};
  if (reply.status === 422 && refusal === 'request_mismatch') return {tag: 'RequestMismatch'};
  // This marker proves only the delivery it answers. A proxy or a supplied
  // fetch implementation may already have sent this key before that delivery.
  if (reply.status === 410 && refusal === 'history_unavailable') return {tag: 'HistoryUnavailable'};
  if ((reply.status === 503 && (refusal === 'overloaded' || refusal === 'budget')) ||
      (reply.status === 504 && refusal === 'deadline')) return {tag: 'Retry', proven: true};
  if (reply.status === 404 && refusal === 'no_entry') return {tag: 'Refused', refusal: {tag: 'NoEntry'}};
  if (reply.status === 403 && refusal === 'not_allowed') return {tag: 'Refused', refusal: {tag: 'NotAllowed'}};
  if (reply.status === 400 && refusal === 'malformed') {
    const kind = Number(/^\{"refused":"malformed","kind":([0-9]{1,10})\}$/.exec(reply.body)?.[1]);
    if (kind <= 0xffffffff) return {tag: 'Refused', refusal: {tag: 'Malformed', value: {kind, at: 0n}}};
  }
  return {tag: 'Retry', proven: false};
}
