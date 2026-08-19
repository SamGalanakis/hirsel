// The app's one HTTP helper. Everything durable still travels over the single
// WebSocket (see ws/client.ts); this is the small REST surface that genuinely
// cannot: the plugin tier's roster and administration endpoints, plus the
// per-plugin routers the Host mounts under `/api/plugins/<id>/…`, which plugin
// UI calls directly.
//
// Auth is the same owner token the `hello` frame carries, presented as a
// standard `Authorization: Bearer` header — never a query parameter, per
// PROTOCOL.md ("The protocol never puts bearer tokens in blob URLs or query
// strings").
import { resolveHttpBase } from "./endpoint";
import { getStoredToken } from "../ws/client";

/** Absolute URL for a host-relative path (`/api/...`). */
export function apiUrl(path: string): string {
  return `${resolveHttpBase()}${path}`;
}

/** An HTTP-level failure carrying the status, so callers can tell "the Host
 * said no" from "the fetch never happened". */
export class ApiError extends Error {
  readonly status: number;
  constructor(status: number, message: string) {
    super(message);
    this.name = "ApiError";
    this.status = status;
  }
}

/** Authenticated `fetch` against the Host origin. Same signature and semantics
 * as the platform `fetch` — a non-2xx is a resolved Response, not a throw — so
 * plugin UI can use it exactly like the real thing. A string body is assumed to
 * be JSON unless the caller says otherwise. */
export function apiFetch(path: string, init?: RequestInit): Promise<Response> {
  const token = getStoredToken();
  const headers = new Headers(init?.headers);
  if (!headers.has("Accept")) headers.set("Accept", "application/json");
  if (token && !headers.has("Authorization")) {
    headers.set("Authorization", `Bearer ${token}`);
  }
  if (typeof init?.body === "string" && !headers.has("Content-Type")) {
    headers.set("Content-Type", "application/json");
  }
  return fetch(apiUrl(path), { ...init, headers });
}

/** Authenticated JSON round-trip: encodes `body`, resolves with the parsed
 * reply, and throws `ApiError` on a non-2xx. */
export async function apiJson<T>(
  path: string,
  init?: { method?: string; body?: unknown },
): Promise<T> {
  const response = await apiFetch(path, {
    method: init?.method ?? "GET",
    body: init?.body === undefined ? undefined : JSON.stringify(init.body),
  });
  if (!response.ok) {
    const detail = await response.text().catch(() => "");
    throw new ApiError(
      response.status,
      detail.trim().length > 0 ? detail.trim() : `HTTP ${response.status}`,
    );
  }
  return (await response.json()) as T;
}
