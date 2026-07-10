# Device pairing and per-device auth over iroh

Executes [ADR-0006]'s milestone-two transport and settles how a phone authenticates.

## Context

[ADR-0006] made the client↔host protocol a transport-agnostic message stream and set iroh (node-key auth, no open ports, no DNS) as the target transport, swapping in "as an ALPN carrying the same message stream, touching connection setup only." It also flagged the browser iroh leg as the speculative, expensive piece (no npm package, wasm wrapper cost, and a browser only ever gets a relayed connection anyway).

Two facts force the design. First, the host binds `127.0.0.1` — a phone on mobile data cannot reach it, and we do not want to open inbound ports, run public DNS, or manage TLS certs for the phone path. Second, auth today is a single static `HIRSEL_TOKEN` shared secret, checked on every connection — fine for localhost/desktop, wrong to type into a phone and impossible to revoke per device.

## Decision

**iroh for native clients, WSS for the browser.** The host runs an iroh `Endpoint` (persisted secret key → stable `NodeId`) alongside the existing axum WSS server. Both carry the identical transport-agnostic message stream; iroh is a bidirectional QUIC stream under an ALPN (e.g. `hirsel/owner/1`) with length-delimited JSON framing of the same `ClientToHost`/`HostToClient` serde frames. The native phone app (Rust `hirsel-client-core`, no wasm) uses iroh; the desktop/web client stays on WSS per [ADR-0006]. To make this reuse clean, the protocol loop in `ws.rs::handle_socket` is extracted to be transport-agnostic (drives any framed `Sink<HostToClient> + Stream<ClientToHost>`), so WSS and iroh share it verbatim.

**QR pairing → per-device token.** Onboarding replaces "type the master token":
1. The host mints a **one-time pairing code** — short TTL, single-use.
2. A QR encodes `hirsel://pair?ticket=<iroh-ticket>&code=<pairing-code>`, with both query values
   URL-encoded. The ticket contains the NodeId + relay/direct addrs; the QR never contains the
   master token.
3. The phone scans, iroh-connects, and sends `pair_request { code, device_label, node_id }`.
4. The host validates the code (unexpired, unused), issues a random **per-device token** stored in a `device_tokens` table (token, device_label, node_id, created_ts, last_seen_ts, revoked), and returns it.
5. The phone persists the token **and its own iroh `SecretKey`** (same secure store) and authenticates future connects with the token (the device token is the first frame on reconnect, replacing the static-token check on the iroh path).

Device tokens are **revocable** via a host list/revoke surface (debug/admin now, desktop-shell panel later). The device's iroh `NodeId` is **pinned** to its token (a token presented from a different node is rejected) as defense-in-depth — the token alone is not bearer-portable. The QR carries a one-time code, never the master secret.

**Client identity must persist.** Because the device token is NodeId-pinned, the client's iroh `SecretKey` (which determines its `NodeId`) MUST be generated once at pairing and persisted, so the same device presents the same `NodeId` on every relaunch. A client that mints a fresh identity per process would be rejected by its own pinned token on reconnect. The `SecretKey` lives in the device's secure storage (Keystore-backed on Android) alongside the token; compromising it requires the same full-device compromise as stealing the token, and revocation still cuts the device off server-side.

**Relays.** Use iroh's default public relays for NAT traversal in v1 (personal scale, one user); self-hosting a relay is deferred. Direct hole-punched connections are used when available, relay-fallback otherwise — transparent to the protocol.

**WSS auth is unchanged for now** — the desktop/localhost path keeps static `HIRSEL_TOKEN`. Extending pairing/device-tokens to WSS is possible later but out of scope.

## Consequences

- No open inbound ports, no public DNS, no TLS-cert management for the phone; connection is e2e-encrypted by node keypairs.
- Per-device revocation; a lost phone is cut off without rotating a shared secret; the QR is a one-time code.
- Costs: an iroh dependency and reliance on public relays; a second serving path on the host (kept cheap by sharing the extracted protocol loop); a pairing state machine + `device_tokens` storage; a NodeId-pinning check on the auth path.
- Browser parity is explicitly not attempted (ADR-0006 rationale); the desktop client remains the WSS reference.

## Status

Accepted. Built in phases: (1) transport-agnostic loop + iroh transport with static-token auth (connectivity spike), (2) pairing + per-device tokens, (3) QR generation + phone scanner + onboarding. Implementers pin the current stable `iroh` from crates.io and follow current docs — the iroh API has churned across releases; do not assume an older API surface.

[ADR-0006]: 0006-wss-first-iroh-later-transport-agnostic-protocol.md
