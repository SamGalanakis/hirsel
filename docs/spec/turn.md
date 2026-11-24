# TURN, long-term credentials and the TCP fallback

Version note (2026-11-24, media-turn-spec-2335): first text of TURN-1 to TURN-11, the Wave 1
connectivity remainder of the real-time media consumer contract (§2). Not yet built: every
fence is `unbuilt`, and every test path is planned. The sections end with
[build laws](#build-laws), [cost targets](#cost-targets) and [build slices](#build-slices).

The built `core/programs/stun_ice/` package covers STUN Binding with short-term credentials
and ICE ([RFC 8489](https://www.rfc-editor.org/rfc/rfc8489),
[RFC 8445](https://www.rfc-editor.org/rfc/rfc8445)). This text adds three things to it:

- **TURN** ([RFC 8656](https://www.rfc-editor.org/rfc/rfc8656)): Allocate, Refresh,
  CreatePermission, ChannelBind, Send and Data indications, and ChannelData, as a sans-IO
  client and a sans-IO relay server, in a new package `core/programs/turn/`<!-- doc-ref: planned -->.
- **Long-term credentials** (RFC 8489 §9.2): realm, nonce, the nonce cookie and bid-down
  prevention, PASSWORD-ALGORITHM(S), MESSAGE-INTEGRITY and MESSAGE-INTEGRITY-SHA256 keyed
  by a `Secret`, and USERHASH. The STUN message layer for them lands in `stun_ice` itself
  (TURN-4); two new TLS-5 sinks keep every long-term key inside the secret boundary (TURN-5).
- **The TCP fallback** for peers that reach only TCP 443: TURN over a stream, with UDP
  relays, and the host terminating TLS in front of it (TURN-2). Passive ICE-TCP on the SFU,
  webrtc.md's slice S8, is withdrawn.

Both packages are general primitives. They name no application, room, participant or media
kind: a selective forwarder, a peer-to-peer client, a gateway and a test peer use the same
operations. Neither reaches `Foreign` code, so `hirsel test --sim` and recording and replay
hold for every program built on them ([FFI-8](foreign.md)). Neither performs I/O: the caller
owns sockets, clocks, randomness, credential storage and demultiplexing. A UDP datagram whose
first byte is 0 to 3 is STUN and one whose first byte is 64 to 79 is TURN ChannelData
([RFC 7983 §7](https://www.rfc-editor.org/rfc/rfc7983#section-7)); the caller routes it.

## TURN

### TURN-1: Scope

In scope, for client and server:

- RFC 8656 §7 to §12: Allocate, Refresh, CreatePermission, ChannelBind, Send and Data
  indications, ChannelData, with UDP relayed transport addresses only.
- Client-to-server transport over UDP or over a stream (TCP, or TLS that the caller or host
  terminates), RFC 8656 §3.1. The server-to-peer leg is always UDP.
- IPv4 and IPv6 allocations (REQUESTED-ADDRESS-FAMILY), one family per allocation.
- Long-term credentials as RFC 8489 §9.2 specifies, with MD5 and SHA-256 password
  algorithms, MESSAGE-INTEGRITY and MESSAGE-INTEGRITY-SHA256 (full 32 bytes, no truncation),
  and USERHASH on the client.
- Time-limited credentials minted from one master `Secret` (TURN-5), the scheme coturn,
  Pion and LiveKit all use, so signaling can hand a browser a password the relay never
  stores.

Out of scope, each with the exact answer the server gives (TURN-7):

| feature | answer |
|---|---|
| TCP relays to peers, [RFC 6062](https://www.rfc-editor.org/rfc/rfc6062) (REQUESTED-TRANSPORT 6) | 442 Unsupported Transport Protocol |
| DONT-FRAGMENT | 420 Unknown Attribute, UNKNOWN-ATTRIBUTES listing 0x001A (RFC 8656 §7.2 step 4) |
| RESERVATION-TOKEN | 400 with EVEN-PORT or an address-family attribute; 508 alone, since no token is ever issued (§7.2 step 5) |
| EVEN-PORT with R = 1 | 400 with ADDITIONAL-ADDRESS-FAMILY, else 508 (step 8); R = 0 is honoured from the pool |
| a second family in one allocation (ADDITIONAL-ADDRESS-FAMILY IPv6) | success for IPv4 only, with ADDRESS-ERROR-CODE 440 (step 9's partial case) |
| TURN over DTLS | nothing to answer: the caller opens no DTLS listener (TURN-2) |
| ALTERNATE-SERVER, 300 Try Alternate | never sent; the client treats a 300 as `Rejected(300)` |
| SOFTWARE, ICMP (0x8004) | never sent; received ones are ignored |
| OAuth third-party authorization ([RFC 7635](https://www.rfc-editor.org/rfc/rfc7635)) | not offered; a request with ACCESS-TOKEN is handled as one without it |
| static user tables (coturn `userdb`) | not offered: every username is a time-limited credential (TURN-5) |

### TURN-2: The TCP fallback

**Decision: TURN over a stream, client to server, with UDP relays. Passive ICE-TCP on the
SFU is withdrawn.** It is reversible: nothing of ICE-TCP is built.

Three classes of strict network, and what reaches through each:

| network | passive ICE-TCP on the SFU | TURN over TCP | TURN over TLS on 443 |
|---|---|---|---|
| UDP blocked, any TCP out | yes | yes | yes |
| only TCP 443 out | yes, if the SFU's TCP candidate is on 443 | yes, on 443 | yes |
| only TLS to 443 out, passed undecrypted (SNI allow-list, HTTP CONNECT proxy) | no: the bytes are not TLS | no | yes |

LiveKit keeps both and picks by RTT (`pkg/rtc/transportmanager_test.go:52-53`: ICE/TCP
first, TURN/TLS when the node offers no ICE/TCP). Its reasons for ICE-TCP are no relay hop
and no TURN credentials. Neither survives here:

- **One mechanism, not two (/wholehog: like things alike, no parallel paths).** TURN over
  TLS reaches every class ICE-TCP reaches, and the third. Keeping ICE-TCP is a second
  fallback with its own framing (RFC 4571), its own candidate type, its own TCP path inside
  every media unit and its own laws, for classes TURN already covers.
- **The relay hop is local.** The relay runs on the SFU's host (TURN-8 Co-location), so the
  extra hop is one loopback datagram, not a network path. RTT is the client's TCP leg either
  way.
- **The credentials are already there.** Signaling mints the ICE ufrag and password for the
  session; it mints the TURN username and password in the same message (TURN-5). No extra
  round trip.
- **Port 443 is shared, not owned.** ICE-TCP on 443 needs the SFU to own TCP 443 or a
  pre-TLS byte demux in front of HTTPS. TURN over TLS can share 443 with HTTPS behind one TLS
  front that routes by SNI (dependency H1 below).
- **TURN is generic; ICE-TCP is SFU-only.** A TURN client also serves a Hirsel program that
  is itself behind a strict firewall. ICE-TCP on the SFU helps only browsers reaching the SFU.

**Where TLS terminates.** Server-side TLS is not a provider operation: a place's ingress
belongs to its host, which terminates inbound TLS in front of the program
([TLS-4](tls.md#tls-4-provider-calls)). So the relay never sees TLS. It sees a plaintext
stream from an accepted socket ([NET-1](net.md#net-1-socket-operation-declarations)), and
TURN over TCP and TURN over TLS are one code path in the program. The difference is only the
host's ingress: plain TCP on 3478 or a dedicated 443 address, or TLS on 443 terminated by the
front and handed over as plaintext. The front routes by SNI (the TURN URL's host name); this
text assumes no TURN ALPN, since browsers do not send one reliably.

**Dependency H1, host TLS ingress for a stream listener** (owner: the runner and launch
track): [launch.md, "Stream ingress behind a TLS front"](launch.md#stream-ingress-behind-a-tls-front).
A separate TLS front routes by SNI, with no ALPN, and prepends a PROXY protocol v2 header.
On an H1 listener the header is required: `proxy.accept` closes an untrusted peer, a missing
or malformed header, a LOCAL command and an oversized header before any byte reaches the
`Framer`. Its `source` is `Origin.client` (XOR-MAPPED-ADDRESS, nonce, quotas), and its
`destination` is the server's local address in the nonce. A TLS close reaches the relay as
`disconnected`. Slice U6 below builds the decoder `proxy.step`.

**What works without H1:** TURN over TCP on a dedicated address's port 443, and over UDP.
That reaches the first two network classes, exactly what ICE-TCP would have reached.
TURN over TLS on a shared 443 waits on H1.

**Not taken.** RFC 6062 (TCP from relay to peer; LiveKit refuses it too, with its
`rejectTCPRelayAddressGenerator`): media peers are UDP. TURN over DTLS: no browser offers it,
and it would need a server DTLS listener for no network class TLS does not reach.

### TURN-3: Types

```hirsel unbuilt
// package turn
pub type Family = V4 | V6
pub type Algorithm = Md5 | Sha256           // RFC 8489 §18.5.1
pub type Transport = Datagram | Stream      // client-to-server leg; the relay leg is UDP

pub type Now { mono_ns: u64, unix_s: u64 }  // monotonic for timers, wall for credential expiry

pub type Poll = Send(u64) | WakeAt(u64) | Idle

pub type TurnError = Config | Clock | Closed(Failure) | NotAllocated | Unpermitted(Addr)
                   | ChannelsFull | Framing | Wire(stun_ice.Error)
                   | Secret(SecretError) | Mac(MacError) | Capacity(Capacity)
pub type Failure = Local | Rejected(u16) | Timeout | Credentials | Mismatch | Disconnected
```

`Now.mono_ns` is the caller's monotonic clock in nanoseconds; a value smaller than an
earlier call's is `Clock`, with no effect. `Now.unix_s` is wall seconds, used only to compare
against a credential's expiry (TURN-5). Every `TurnError` from `receive` or `poll` other
than `Capacity`, `Config`, `Clock`, `NotAllocated`, `Unpermitted` and `ChannelsFull` is
**fatal** for a `Client`, as in [DTLS-3](dtls.md#dtls-3-sans-io-driving): state becomes
`Closed`, and every later call answers `Closed(f)`. A `Relay` is never closed by an error:
a `Secret`, `Mac` or `Wire` error while handling one request fails that request (500 if it
was authenticated far enough to answer, else a counted discard) and leaves every allocation
as it was. Transient conditions are counted
discards (TURN-6, TURN-7), never errors.

There is no jitter anywhere. A `WakeAt` is exact, so a run under `std.sim.udp` or
`std.sim.net` is a function of its seeds.

### TURN-4: The STUN message layer

`stun_ice`'s `stun.hirsel` today decodes one shape of message (Binding) into a fixed
`Message` record and keys integrity with plaintext `Bytes`. TURN needs every method, its own
attributes and `Secret` keys. Slice U0 generalizes it, in place, with no second codec:

```hirsel unbuilt
// package stun_ice, added beside the Binding functions
pub type Key = Short(Bytes) | Long(Secret)            // RFC 8489 §9.1 and §9.2
pub type Attr { kind: u16, value: Range }
pub type Header { method: u16, class: Class, transaction: Transaction, length: u16 }

pub fn header(packet: Bytes) -> Result[Header, Error]
pub fn attrs(packet: Bytes, mut out: List[Attr]) -> Result[Integrity, Error]
pub fn verify_key(packet: Bytes, key: Key, mut scratch: Bytes) -> Result[Integrity, Error]

pub opaque type Builder
pub fn start(method: u16, class: Class, tx: Transaction, mut out: Bytes) -> Result[Builder, Error]
pub fn add(mut b: Builder, kind: u16, value: Bytes, mut out: Bytes) -> Result[(), Error]
pub fn add_xor_addr(mut b: Builder, kind: u16, a: Addr, mut out: Bytes) -> Result[(), Error]
pub fn add_error(mut b: Builder, code: u16, mut out: Bytes) -> Result[(), Error]
pub fn finish(take b: Builder, integrity: Integrity, key: Key, fingerprint: bool, mut out: Bytes, mut scratch: Bytes) -> Result[u64, Error]
```

- `attrs` walks every attribute once, checks padding, length and order (nothing but
  FINGERPRINT after MESSAGE-INTEGRITY-SHA256, nothing but those two after
  MESSAGE-INTEGRITY), and appends each `Attr` with its value range. An unknown attribute
  below 0x8000 is `UnknownRequired(kind)`; the caller answers 420 with UNKNOWN-ATTRIBUTES.
- `Key.Short` is the ICE password, which SDP already carries in plaintext; `Key.Long` is a
  long-term key derived as TURN-5 says, always a `Secret` with `mac` or `verify`.
- For `Long`, the MAC input (the message up to the attribute before the tag, with the length
  field adjusted, RFC 8489 §14.5 and §14.6) is copied once into `scratch`, and the tag is
  `secret.hmac_sha1` or `secret.hmac_sha256` over it; verification is the matching
  `secret.verify_hmac_*` (TURN-5, [TLS-5](tls.md#tls-5-secrets)). For `Short`, the existing
  streaming `integrity.hirsel` path is kept.
- `binding_request`, `binding_success`, `binding_error`, `decode` and `verify` keep their
  signatures and become thin forms over `start`/`add`/`finish` and `attrs`. Their laws stay
  as they are; slice U0 rewrites the package's law lock with `hirsel laws write`.

New attribute and method numbers are constants in `stun_ice` (RFC 8656 §18, RFC 8489
§18.3): CHANNEL-NUMBER 0x000C, LIFETIME 0x000D, XOR-PEER-ADDRESS 0x0012, DATA 0x0013,
REALM 0x0014, NONCE 0x0015, XOR-RELAYED-ADDRESS 0x0016, REQUESTED-ADDRESS-FAMILY 0x0017,
EVEN-PORT 0x0018, REQUESTED-TRANSPORT 0x0019, DONT-FRAGMENT 0x001A, MESSAGE-INTEGRITY-SHA256
0x001C, PASSWORD-ALGORITHM 0x001D, USERHASH 0x001E, RESERVATION-TOKEN 0x0022,
ADDITIONAL-ADDRESS-FAMILY 0x8000, ADDRESS-ERROR-CODE 0x8001, PASSWORD-ALGORITHMS 0x8002,
ICMP 0x8004; methods Allocate 0x003, Refresh 0x004, Send 0x006, Data 0x007,
CreatePermission 0x008, ChannelBind 0x009.

### TURN-5: Long-term credentials and secrets

**The key.** RFC 8489 §18.5.1: `key = H(username ":" OpaqueString(realm) ":"
OpaqueString(password))`, H being MD5 (16 bytes) or SHA-256 (32 bytes). The library
applies no OpaqueString processing: the caller passes realm and password already processed,
and every credential this text mints is printable ASCII, which OpaqueString leaves unchanged.

**Two new TLS-5 sinks** (slice U1), so that no long-term key and no password the relay
checks is ever plaintext in program memory:

```hirsel unbuilt
// package secret, beside the MAC and derivation sinks of TLS-5
pub type DigestAlg = Md5 | Sha256
pub type Inner = Raw | Base64
pub fn derive_digest(s: Secret, alg: DigestAlg, prefix: Bytes, inner: Inner, suffix: Bytes, uses: String) -> Result[Secret, SecretError]
pub fn hmac_sha1(s: Secret, data: Bytes) -> Result[Inline[u8, 20], MacError]
pub fn verify_hmac_sha1(s: Secret, data: Bytes, tag: Bytes) -> Result[bool, MacError]
```

- `derive_digest` keeps `H(prefix ‖ enc(bytes of s) ‖ suffix)` as a new sealed secret, where
  `enc` is the identity for `Raw` and RFC 4648 §4 padded base64 for `Base64`. It is under
  TLS-5's derivation rule unchanged: the parent needs `kdf`, the child set is the rule's
  (here `mac`, `verify`, `kdf`), the child is opaque exactly when the parent is, and the
  charges, frames and replay follow `derive_hmac_sha256`'s. MD5 runs in the provider over
  RustCrypto `md-5` (ring has none), SHA-256 over ring's `digest`. MD5 is offered only
  because RFC 8489 makes it the algorithm every TURN client without PASSWORD-ALGORITHMS
  uses, which is every browser today.
- `hmac_sha1` and `verify_hmac_sha1` extend TLS-5's MAC sink to the 20-byte HMAC-SHA1 that
  MESSAGE-INTEGRITY is, with the same use requirements (`mac` to sign, `mac` or `verify` to
  verify), the same charges and the same frames. SHA-1 is acceptable as HMAC; it is the only
  MAC browsers send in TURN.

**Time-limited credentials.** One master `Secret` per deployment, never sent:

- `username = expiry ":" subject`: `expiry` is decimal Unix seconds, `subject` is 1 to 64
  printable ASCII bytes chosen by the minting application, with no `:`.
- `password = base64(HMAC-SHA256(master, username))`, 44 ASCII bytes.

```hirsel unbuilt
// package turn
pub fn mint(master: Secret, expiry_unix_s: u64, subject: Bytes, mut username: Bytes, mut password: Bytes) -> Result[(), TurnError]
pub fn relay_key(master: Secret, username: Bytes, realm: Bytes, alg: Algorithm) -> Result[Secret, TurnError]
```

`mint` is for the signaling side, which must hand the browser its password in plaintext: it
calls `secret.hmac_sha256(master, username)` (master needs `mac`) and base64-encodes the tag
into `password`. This password is a bearer value that leaves the program by design, and it
expires at `expiry`. `relay_key` is for the relay: `derive_hmac_sha256(master, "", username,
"kdf")`, then `derive_digest(that, alg, username ":" realm ":", Base64, "", "mac")`. The
relay's master needs only `kdf`; the relay never holds the password or the key in plaintext.
Signaling and the relay may be separate places, each granted the master narrowed to its one
use ([TLS-5 `narrow`](tls.md#tls-5-secrets)).

A **client** holding a password that it did not mint (a configured corporate TURN server)
takes it as a `Secret` with `kdf`, from a grant or `seal`, and derives
`derive_digest(password, alg, username ":" realm ":", Raw, "", "mac")`.

**The nonce** is stateless and bound to the client's 5-tuple, after Pion's
(`/tmp/ref-pion-turn` at `7bffbda`, `internal/server/nonce.go`) with the address added:

`nonce = "obMatJos2" ‖ base64(features, 3 bytes) ‖ hex(t) ‖ hex(HMAC-SHA256(nonce_key, t ‖ five_tuple)[0..16])`

`t` is `mono_ns / 10^6` as 8 bytes big-endian, `five_tuple` is the ingress kind, the
server's local address and the client's address as their canonical byte forms, and
`nonce_key` is a `Secret` with `mac` (from `derive_hkdf_sha256(master, "", "hirsel turn
nonce", 32, "mac")` or any caller key). A nonce older than `nonce_s` or with a wrong tag is
stale (438). The features byte string has bit 0 (Password algorithms) set and bit 1
(Username anonymity) clear (RFC 8489 §18.1).

**Why the server sets no Username anonymity bit.** A stateless server cannot invert
`SHA-256(username ":" realm)`: it has no table of usernames to hash. The minted `subject` is
the application's opaque per-session identifier, so USERHASH would hide nothing the
username reveals. The client implements USERHASH anyway, for servers that offer it (RFC
8489 §9.2.5); its value is `SHA-256(username ":" realm)` with `std.sha256`, since username
and realm are not secret.

**Server check order** (RFC 8489 §9.2.4, applied to every request; indications and
ChannelData are never authenticated):

1. No MESSAGE-INTEGRITY and no MESSAGE-INTEGRITY-SHA256: 401 with REALM, a fresh NONCE and
   PASSWORD-ALGORITHMS [SHA-256, MD5]. The answer is computed, not stored: the relay keeps no
   state for an unauthenticated request.
2. Integrity present but USERNAME (or USERHASH), REALM or NONCE missing: 400, unauthenticated.
3. The NONCE carries the cookie with the Password algorithms bit (every nonce this relay
   issues does). If the request has neither PASSWORD-ALGORITHMS nor PASSWORD-ALGORITHM, it is
   processed as MD5: this is every browser today, and it must succeed. Otherwise, unless both
   are present, PASSWORD-ALGORITHMS equals [SHA-256, MD5] and PASSWORD-ALGORITHM is one of
   them: 400 (bid-down, §9.2.1).
4. USERHASH present: 401 (not offered). USERNAME not `expiry ":" subject`, `expiry` at or
   before `unix_s`, or `subject` revoked (TURN-9): 401 with REALM, a fresh NONCE and
   PASSWORD-ALGORITHMS.
5. The nonce's tag does not verify for this 5-tuple (a nonce this relay never issued to this
   client): 438 with a fresh NONCE, REALM and PASSWORD-ALGORITHMS. This step is ahead of
   §9.2.4's order on purpose: it costs one `secret.verify_hmac_sha256` and no derivation, and
   it means every derivation below is for a client that received a nonce at its own address.
   A genuine nonce that has merely aged is handled at step 8, in RFC order.
6. Key. A request on an existing allocation uses the key `Secret` the allocation keeps from
   its Allocate (no derivation). Otherwise the relay derives `relay_key`, if the derive budget
   (`derive_per_s`, per relay) allows; if not, the request is discarded and counted
   `throttled`, and the client's retransmission retries.
7. Integrity does not verify: 401 with REALM and NONCE. MESSAGE-INTEGRITY-SHA256 is checked
   when present, else MESSAGE-INTEGRITY.
8. Nonce older than `nonce_s`: 438 with a fresh NONCE, REALM and PASSWORD-ALGORITHMS.
9. On an existing allocation, a username other than the one that made it: 441.

Every response to an authenticated request carries MESSAGE-INTEGRITY-SHA256 under the same
key, except when the request was processed as MD5 (step 3): then it carries
MESSAGE-INTEGRITY and no REALM, NONCE, USERNAME or USERHASH (§9.2.4, last paragraph). The
pre-authentication cost of a request is one STUN parse, at most one nonce MAC check, and no
state.

**Client rules** (RFC 8489 §9.2.5): the first request goes without integrity. On 401 with
REALM and NONCE it derives the key once for that realm and algorithm (SHA-256 when the nonce
cookie has the Password algorithms bit and PASSWORD-ALGORITHMS lists it, else MD5) and
retries once; a second 401 with the same credentials is `Closed(Credentials)`, never a loop.
On 438 it retries with the new nonce, without re-deriving. Once a response has shown
PASSWORD-ALGORITHMS, it never falls back to MD5 for that server. A 400 without integrity
after a request with integrity is discarded and counted (it may be forged). Every response to
an authenticated request must verify, or it is discarded and counted `tampered`.

### TURN-6: The client

```hirsel unbuilt
pub type ClientConfig {
    transport: Transport,
    family: Family,
    lifetime_s: u32,        // requested; default 600; 0 asks for the server's default
    rto_ms: u32,            // Datagram only; default 500 (RFC 8489 §6.2.1)
    rc: u8,                 // requests sent per transaction; default 7
    rm: u8,                 // final wait, in units of rto; default 16
    ti_ms: u32,             // Stream only; default 39,500 (RFC 8489 §6.2.2)
    channels: u16           // most channels bound at once; 1 to 4,096; default 64
}
pub type ClientState = Allocating | Allocated(Allocation) | Closed(Failure)
pub type Allocation { relayed: Addr, mapped: Addr, expires_ns: u64 }
pub type Received = Data(Addr, Range) | Control | Discarded
pub type ClientDiscards { malformed: u64, stray: u64, tampered: u64, unauthenticated: u64, unbound_channel: u64 }

pub opaque type Client

pub fn client_defaults() -> ClientConfig
pub fn client(c: ClientConfig, server: Addr, take username: Bytes, take password: Secret) -> Result[Client, TurnError]
pub fn permit(mut c: Client, peer: Addr, now: Now) -> Result[(), TurnError]
pub fn bind(mut c: Client, peer: Addr, now: Now) -> Result[u16, TurnError]
pub fn send_to(mut c: Client, peer: Addr, mut slot: Bytes, lo: u64, hi: u64) -> Result[Range, TurnError]
pub fn receive(mut c: Client, now: Now, r: Random, mut packet: Bytes) -> Result[Received, TurnError]
pub fn poll(mut c: Client, now: Now, r: Random, mut out: Bytes) -> Result[Poll, TurnError]
pub fn close(mut c: Client, now: Now)
pub fn client_state(c: Client) -> ClientState
pub fn client_discards(c: Client) -> ClientDiscards
```

**Driving.** As DTLS-3: `receive` takes one whole datagram, or one whole message from the
stream framer (TURN-8); `poll` hands out queued requests (`Send(n)` appends one message to
`out`), runs due timers, and answers `WakeAt` or `Idle`; `Random` gives the 96-bit
transaction IDs and is never kept. The client starts Allocate at its first `poll`.

**Data in place.** `send_to` frames the payload at `slot[lo..hi]` in place: the caller leaves
`HEADROOM` = 48 bytes before `lo` and 3 after `hi`, and the result is the range to send.
ChannelData (4-byte header, padded to 4 on a `Stream`) is used once the peer's ChannelBind
succeeded; before that a Send indication (header, XOR-PEER-ADDRESS, DATA). A peer whose IP
has no installed permission is `Unpermitted(peer)` before anything is written. `receive`
answers `Data(peer, range)` for a ChannelData or Data indication, the range being the payload
inside `packet`; no copy.

**Timers**, all exact:

- Transactions over `Datagram`: sends at 0, `rto`, `3·rto`, `7·rto` and so on to `rc`
  sends, then `Timeout` after a further `rm·rto` (defaults: 0, 500, 1,500, 3,500, 7,500,
  15,500, 31,500 ms; failure at 39,500 ms). Over `Stream`: one send, failure at `ti_ms`.
  An Allocate or Refresh timeout is `Closed(Timeout)`; a CreatePermission or ChannelBind
  timeout drops that permission or channel and is not fatal.
- Allocation refresh at granted lifetime minus 60 s; permission refresh every 240 s while
  the permission is wanted; channel refresh every 540 s (RFC 8656 §9 and §12 lifetimes of
  300 s and 600 s, minus 60 s of margin).
- `close` sends Refresh with LIFETIME 0 once, without retransmission, and moves to
  `Closed(Local)`.

**Errors.** 437 is `Closed(Mismatch)`. 441 and a repeated 401 are `Closed(Credentials)`.
Any other error response to Allocate is `Closed(Rejected(code))`. 403, 443 or 508 to a
CreatePermission or ChannelBind drops that permission or channel and is counted; the
caller sees `Unpermitted` on its next `send_to`. Channel numbers are chosen as the lowest
free number from 0x4000, so runs are deterministic; a number whose binding lapsed is not
reused for 300 s (RFC 8656 §12).

### TURN-7: The relay server

```hirsel unbuilt
pub type Prefix { ip: Ip, bits: u8 }
pub type Allow { prefix: Prefix, ports_lo: u16, ports_hi: u16 }
pub type PeerPolicy { deny: List[Prefix], allow: List[Allow] }

pub type RelayConfig {
    realm: Bytes,             // printable ASCII, 1 to 127 bytes
    lifetime_s: u32,          // default 600 (RFC 8656 §7.2)
    lifetime_max_s: u32,      // default 3,600
    nonce_s: u32,             // default 600
    quota: u16,               // allocations per username; default 12 (LiveKit)
    permissions: u16,         // per allocation; default 64
    channels: u16,            // per allocation; default 64
    derive_per_s: u32,        // relay_key derivations per second, whole relay; default 1,000
    policy: PeerPolicy
}
pub type Ingress = Udp(Addr) | Stream(u64)          // the local UDP address, or a connection
pub type Origin { ingress: Ingress, client: Addr }  // the client side of the 5-tuple
pub type Out = ToClient(Origin, Range) | ToPeer(Addr, Addr, Range) | Reply(Origin, u64) | Nothing
pub type RelayDiscards { malformed: u64, no_allocation: u64, no_permission: u64, policy: u64,
                         unbound_channel: u64, reserved_channel: u64, throttled: u64 }
pub type RelayCounts { allocations: u64, permissions: u64, channels: u64, pool_free: u64 }

pub opaque type Relay

pub const HEADROOM: u64 = 48
pub fn relay_defaults(realm: Bytes) -> RelayConfig
pub fn default_deny() -> List[Prefix]
pub fn relay(c: RelayConfig, take master: Secret, take nonce_key: Secret, take pool: List[Addr]) -> Result[Relay, TurnError]
pub fn from_client(mut r: Relay, now: Now, rnd: Random, o: Origin, mut packet: Bytes, mut out: Bytes) -> Result[Out, TurnError]
pub fn from_peer(mut r: Relay, now: Now, relayed: Addr, peer: Addr, mut slot: Bytes, lo: u64, hi: u64) -> Result[Out, TurnError]
pub fn disconnected(mut r: Relay, conn: u64)
pub fn poll(mut r: Relay, now: Now) -> Poll
pub fn revoke(mut r: Relay, subject: Bytes, until_unix_s: u64) -> Result[u64, TurnError]
pub fn relay_discards(r: Relay) -> RelayDiscards
pub fn relay_counts(r: Relay) -> RelayCounts
```

**Sockets.** The caller binds every address in `pool` as a UDP socket before `relay`
([NET-15](net.md#net-15-udp-rules)) and maps each relayed address to its socket. The pool
is the relay's whole capacity: an allocation takes one address, chosen with `rnd` among the
free ones of the requested family (RFC 6056 randomization, from 49152 to 65535 when the
caller binds those), and returns it at the tail of the free list when the allocation ends.

**Paths.** `from_client` takes one datagram or one framed message. Its answer:
`Reply(o, n)` (a response appended to `out`, to send back on `o`), `ToPeer(relayed, peer,
range)` (send `packet[range]` from the socket bound to `relayed` to `peer`; the range lies in
`packet`, no copy), or `Nothing` (consumed or discarded). `from_peer` takes a datagram
received on `relayed` from `peer` into `slot[lo..hi]` with `HEADROOM` before `lo` and 3 bytes
after `hi`; it writes the ChannelData or Data indication header in place and answers
`ToClient(o, range)`. ChannelData is used when a channel binds that peer (MUST, RFC 8656
§12.4), with padding to 4 on a `Stream` origin. A peer IP without permission, or no
allocation on `relayed`, is `Nothing` and counted. `poll` expires allocations, permissions,
channels and channel quarantines at their exact times and answers the next `WakeAt`.

**Allocate**, RFC 8656 §7.2 steps 2 to 10 in this order, after TURN-5's checks (step 1):

2. The origin already has an allocation: a retransmission (same transaction ID) gets the
   allocation's success again; any other Allocate is 437.
3. REQUESTED-TRANSPORT missing or malformed: 400; not 17: 442.
4. DONT-FRAGMENT: treated as an unknown comprehension-required attribute, 420 with
   UNKNOWN-ATTRIBUTES 0x001A.
5. RESERVATION-TOKEN with EVEN-PORT, REQUESTED-ADDRESS-FAMILY or ADDITIONAL-ADDRESS-FAMILY:
   400; otherwise 508, since no token is valid.
6. REQUESTED-ADDRESS-FAMILY and ADDITIONAL-ADDRESS-FAMILY both: 400.
7. REQUESTED-ADDRESS-FAMILY names a family with no address in the pool, or it is absent and
   the pool has no IPv4 address: 440.
8. EVEN-PORT with R = 1: 400 with ADDITIONAL-ADDRESS-FAMILY, otherwise 508 (no reservation
   is ever made).
9. ADDITIONAL-ADDRESS-FAMILY with value IPv4: 400. With IPv6: the allocation is IPv4 only,
   and success carries ADDRESS-ERROR-CODE 440 (the partial case).
10. The username holds `quota` allocations: 486.

Then: no free pool address of the family (an even port under EVEN-PORT R = 0): 508.

Success carries XOR-RELAYED-ADDRESS, LIFETIME, XOR-MAPPED-ADDRESS (`o.client`) and, for a
dual-family request, ADDRESS-ERROR-CODE 440 for IPv6. The lifetime is the requested one
clamped to `[lifetime_s, lifetime_max_s]`: a request below the default gets the default.
The allocation keeps its username, realm, algorithm and the derived key `Secret` (TURN-5
step 6) until it ends.

**Refresh.** LIFETIME 0 deletes the allocation and answers success with LIFETIME 0. A
REQUESTED-ADDRESS-FAMILY other than the allocation's: 443. No allocation on the origin: 437.

**CreatePermission.** One or more XOR-PEER-ADDRESS. A family other than the allocation's:
443; any address the policy denies: 403, and none is installed; more than `permissions`: 508.
Each installs or refreshes a permission for the IP (not the port) for 300 s.

**ChannelBind.** CHANNEL-NUMBER outside 0x4000 to 0x4FFF, a number bound to another peer,
a peer bound to another number, or a pair still in its 300 s quarantine: 400. Family: 443.
Policy: 403. More than `channels`: 508. Success installs or refreshes the channel for 600 s
and the peer's permission for 300 s.

**Send indication.** No allocation, no DATA, no XOR-PEER-ADDRESS, or no permission for the
peer's IP: discarded and counted. The policy is checked again on the full address (an `Allow`
carries ports, a permission does not). A Send never refreshes a permission. **ChannelData**
on an unbound number, with a length past the datagram, or numbered 0x5000 to 0xFFFF: discarded
and counted. Indications and ChannelData without an allocation on the origin are discarded,
never answered.

**The peer policy.** An address is denied when a `deny` prefix contains it and no `allow`
entry covers it and its port; an IPv4-mapped IPv6 address is checked as IPv4. `default_deny`
is LiveKit's list (`/tmp/ref-livekit` at `d210b4d`, `pkg/service/turnrelay.go`): loopback,
link-local unicast and multicast, multicast, private (10/8, 172.16/12, 192.168/16, fc00::/7),
unspecified, 0/8, 100.64/10, 198.18/15, 240/4, 64:ff9b::/96 and 64:ff9b:1::/48. A
co-located SFU's own address and media ports go in `allow` (TURN-8).

### TURN-8: Streams, simulation and co-location

**Stream framing.** One sans-IO framer per accepted or connected stream, used by both ends:

```hirsel unbuilt
pub opaque type Framer
pub type Frame = Message(Range) | Pending
pub fn framer() -> Framer
pub fn frame_next(mut f: Framer, buffer: Bytes) -> Result[Frame, TurnError]
pub fn frame_consumed(mut f: Framer, mut buffer: Bytes)
```

The caller appends whatever a `recv` returned to `buffer` and calls `frame_next` until
`Pending`. A first byte whose top two bits are 00 starts a STUN message of `20 + length`
bytes; 01 starts ChannelData of `4 + length` bytes rounded up to a multiple of 4 (RFC 8656
§12.5). Top bits 10 or 11 are `Framing`, which is fatal: a byte stream that lost its
framing cannot be resynchronized, and the caller closes the connection (and the relay is told
`disconnected`). A STUN message is at most 20 + 65,532 = 65,552 bytes (its length is a
multiple of 4) and a padded ChannelData message at most 4 + 65,536 = 65,540, so a buffer of
65,552 bytes past the consumed prefix always suffices. On a stream
there is no retransmission and no loss, and RFC 8656 §3.1 ties the allocation to the
connection: `disconnected(conn)` deletes it at once.

**Back-pressure.** The relay never queues. Toward a `Stream` client the caller writes
`ToClient` ranges to the connection and, when the socket's send buffer is full, drops the
message and counts it; media prefers loss to delay. Toward a UDP client or peer the caller's
`udp_send` batch is the queue.

**Determinism.** Over UDP, client, relay and peer run under `std.sim.udp`
(`core/std/api/sim/udp.md`) with its seeded loss, delay and rebinding. Over a stream, they run
under `std.sim.net` (`core/std/api/sim/net.md`), whose seeded prefix plan splits and joins
every send and receive, which is exactly what the framer must survive. Neither needs a new
simulator.

**Co-location with the SFU.** The relay is its own place, not code inside the media units:
webrtc.md's RTC-3 still drops `Turn`-class datagrams on media ports, because the media ports
are not TURN ports. The relay sends `ToPeer` datagrams from its pool sockets to the SFU's
media port, and the SFU sees an ordinary UDP source, the relayed address. That source is
authenticated like any other: by the ICE connectivity check under the session's ICE
password, then by DTLS fingerprint pinning, so a relayed path gets no trust a direct one
does not. The operator adds the SFU's address and media port range to `allow`, as LiveKit
restricts its embedded relay to the node's media ports. An in-process hand-off that skips
the socket is not offered: it would need a relayed path kind in `PeerSession`, a second
path identity.

### TURN-9: Revocation

`revoke(r, subject, until_unix_s)` deletes every allocation whose username's subject is
`subject` at once (no further `ToPeer` or `ToClient` for them), returns their addresses to the
pool, and refuses that subject at TURN-5 step 4 until `until_unix_s`, which the caller sets
to the latest expiry it minted for the subject. Entries leave the revoked set at their time.
It answers the number of allocations deleted. When an SFU revokes a participant (webrtc.md
RTC-7), its signaling side also sends the participant's subject to the relay place, which
calls `revoke`. That is not part of RTC-7's acknowledgement condition: the SFU's own fence
already stops every packet for the epoch, whatever path it arrives on, and the relay revoke
only reclaims the allocation and refuses new ones.

### TURN-10: Discards and counters

Every discard is counted in `ClientDiscards` or `RelayDiscards` by cause, as DTLS-6 counts
its own; none is an error and none is logged per packet. The relay also reports allocations,
permissions and channels live, and pool addresses free, through `relay_counts`, which the
caller's trace reads.

### TURN-11: What the caller does

- Binds the pool and the client-facing UDP and TCP sockets; accepts streams; runs one
  `Framer` per stream; tells the relay `disconnected`.
- Owns `Random` (transaction IDs, pool choice) and both clocks.
- Holds the master `Secret` (signaling: `mac`; relay: `kdf`) and the nonce key; never seals
  a password it minted.
- Puts the minted username and password into the session's ICE servers list next to the ICE
  credentials, with `turn:` on UDP 3478, `turn:` with `transport=tcp` on 443 (dedicated
  address), and `turns:` on 443 once H1 exists.
- Strips and checks a PROXY v2 header (U6, H1) before the first byte reaches the `Framer`,
  and passes the address it carries as `Origin.client`.
- Demultiplexes by first byte (RFC 7983) on every socket it shares.

## Consumer contract coverage

| requirement (CONSUMER-CONTRACT §2) | where | laws |
|---|---|---|
| TURN over TLS-over-TCP, or ICE-TCP on the SFU, for strict firewalls | TURN-2 (TURN over a stream; ICE-TCP withdrawn), TURN-8 framing; TLS on shared 443 waits on H1 | `stream_framing`, `interop_coturn` (TCP), `sim_relay_loss` |
| alongside public-IP ICE-Lite | TURN-8 co-location: relayed peers reach the ICE-Lite SFU as ordinary UDP sources | `sim_relay_loss` (SFU-shaped peer) |
| TURN client and relay server (brief) | TURN-6, TURN-7 | `allocate_check_order`, `unsupported_answers`, `lifetimes_exact`, `permissions_by_ip`, `channel_rules`, `interop_coturn`, `interop_pion` |
| long-term credentials, MI-SHA256, USERHASH (brief) | TURN-4, TURN-5 | `long_term_key_vectors`, `userhash_vector`, `challenge_exchange`, `bid_down_refused`, `nonce_bound_to_tuple`, `minted_credentials` |
| secrets never plaintext where a sink can do the job (brief) | TURN-5 sinks, `relay_key` | `long_term_key_vectors`, `no_key_in_recording` |
| relay abuse | TURN-7 policy and quota, TURN-5 steps 4 and 5 | `peer_policy`, `quota_and_capacity`, `expired_costs_no_derivation` |
| revocation | TURN-9 | `revoke_drops_paths` |

## Prior art

Goose (`/tmp/ref-goose` at `d5ae5bb`), LiveKit (`/tmp/ref-livekit` at `d210b4d`), Pion turn
(`/tmp/ref-pion-turn` at `7bffbda`) and coturn (`/tmp/ref-coturn` at `04fad0c`).

| item | source | verdict |
|---|---|---|
| Any STUN, TURN, socket or crypto surface | Goose: no network or cryptography modules in its standard library | NOT TAKEN: nothing to take |
| ICE/TCP and TURN/TLS both, chosen by RTT | LiveKit `pkg/rtc/transportmanager_test.go:52-53`, `TCPFallbackRTTThreshold` | REPLACED by one fallback, TURN over a stream (TURN-2) |
| Embedded relay limited to the node's media ports | LiveKit `pkg/service/turnrelay.go` | ADAPTED: `allow` entries in a generic policy |
| Restricted peer ranges | LiveKit `pkg/service/turnrelay.go` (`isRestrictedPeerIP`) | ADOPTED as `default_deny` |
| TCP relays rejected | LiveKit `pkg/service/turnrelay.go:67` | ADOPTED as 442 |
| 12 allocations per user | LiveKit `pkg/config/config.go:74`, `pkg/service/turn.go:119` | ADOPTED as the `quota` default |
| PROXY protocol behind an external TLS front, trusted CIDRs | LiveKit `pkg/service/turn.go:236` (`ExternalTLS`, `ProxyProtocol`) | ADAPTED as H1 and slice U6 |
| `apiKey`/`pID`/expiry username, `sha256(secret ‖ …)` password | LiveKit `pkg/service/turn.go` | ADAPTED: HMAC-SHA256 under a sealed master, not a bare hash of a concatenation |
| REST credentials `HMAC-SHA1(secret, "timestamp:user")` | Pion `lt_cred.go`; coturn `use-auth-secret` (its `turnserver.conf` option) | ADAPTED: same shape, HMAC-SHA256, expiry first |
| Nonce = timestamp ‖ HMAC(key, timestamp), 1 h | Pion `internal/server/nonce.go:17` | ADAPTED: bound to the 5-tuple, `nonce_s` 600 as coturn's default |
| Allocation lifetime 600 s default, 3,600 s max | Pion, coturn, RFC 8656 | ADOPTED |
| Key = H(user:realm:pass) over SHA-256/384/512 or MD5 | coturn `stun_produce_integrity_key_str` | ADAPTED: MD5 and SHA-256 only (RFC 8489 §18.5), inside the secret boundary |
| Static user database | coturn `userdb` | NOT TAKEN: time-limited credentials only |
| RFC 6062 TCP relay, TURN over DTLS | coturn | NOT TAKEN (TURN-2) |
| USERHASH, PASSWORD-ALGORITHMS | neither Pion nor coturn has them | NOT TAKEN: nothing to take; specified from RFC 8489, USERHASH client-only (TURN-5) |

## Build laws

Each law is written red first in its named harness, with an oracle outside the code under
test. The harnesses are `core/programs/turn/laws.hirsel`<!-- doc-ref: planned -->, the
existing `core/programs/stun_ice/laws.hirsel` for U0, and TLS-5's sink laws for U1. Interop
runs from `core/programs/turn/interop.py`<!-- doc-ref: planned --> on loopback, with coturn
and Pion built from clones in `/tmp`, never vendored. The existing `binding_vectors` law is
not re-pinned; U0 must leave it green.

- **`long_term_key_vectors`** (U1). *Scenario:* `seal` the password, `derive_digest` with
  `Md5` and `Raw` for `user`/`realm`/`pass`; with `Sha256` for RFC 8489 Appendix B.1's
  username, realm and password; then `verify_key` with `Long` on B.1's request. *Oracle:* RFC
  8489 §9.2.2's MD5 key 0x8493fbc53ba582fb4c044c456bdc40eb (checked by `verify_hmac_sha1`
  over a hand-built message, since the key itself is opaque), and B.1's listed
  MESSAGE-INTEGRITY-SHA256 with erratum 6268.
- **`userhash_vector`** (U2). *Scenario:* the client's USERHASH for B.1's username and
  realm. *Oracle:* B.1's listed 32 bytes.
- **`minted_credentials`** (U1, U2). *Scenario:* a master sealed from the 20 bytes `0x0b`
  repeated (RFC 4231 test case 1's key), `mint(master, 1700000000, "alice")`, then
  `relay_key` for realm `example.org` with `Md5` and with `Sha256`, and an Allocate signed by
  a client given the minted password. *Oracle:* the username `1700000000:alice` by hand; the
  password and both keys from an independent Python `hmac`/`hashlib` run whose inputs and
  outputs are written into the case; the relay must verify the client's MESSAGE-INTEGRITY
  and MESSAGE-INTEGRITY-SHA256.
- **`challenge_exchange`** (U2). *Scenario:* client against relay: 401, authenticated
  Allocate, success; a stale nonce gives 438 and one retry without re-derivation; wrong
  password gives one retry then `Closed(Credentials)`. *Oracle:* RFC 8489 §9.2.5 by hand;
  the count of derivation frames in the recording (one per realm and algorithm).
- **`bid_down_refused`** (U2). *Scenario:* requests whose PASSWORD-ALGORITHMS differ from the
  nonce cookie's offer, a request with only one of the two attributes, a request with
  neither (it must succeed, processed as MD5, its response carrying MESSAGE-INTEGRITY only),
  and a client given a response with the cookie bit but no PASSWORD-ALGORITHMS. *Oracle:*
  RFC 8489 §9.2.1 and §9.2.4 by hand: 400, 400, success, and a counted client discard.
- **`nonce_bound_to_tuple`** (U2). *Scenario:* a valid nonce replayed from another client
  address, a nonce with a forged tag, and a genuine one used after `nonce_s`. *Oracle:* by
  hand from TURN-5: 438 each time, no derivation frame for the first two, and success at
  `nonce_s − 1 ms`.
- **`expired_costs_no_derivation`** (U2). *Scenario:* 10^4 requests with expired, malformed
  and revoked usernames or forged nonces, then 2,000 valid first Allocates in one second, then
  Refreshes on the allocations made. *Oracle:* the recording holds no derivation frame for the
  first group or for the Refreshes; exactly `derive_per_s` derivations happen in the second
  and the rest are `throttled`.
- **`allocate_check_order`** (U3). *Scenario:* one request per step of TURN-7's Allocate list
  (steps 2 to 10 and pool exhaustion), each violating that step and every later one.
  *Oracle:* RFC 8656 §7.2 by hand: the earliest step's code.
- **`unsupported_answers`** (U3). *Scenario:* each row of TURN-1's out-of-scope table.
  *Oracle:* that table, which cites RFC 8656 §7.2 per row.
- **`lifetimes_exact`** (U3, U2). *Scenario:* under `std.sim.udp` with no loss: allocation,
  permission and channel expiry on the relay, the client's refresh times, the 300 s channel
  quarantine, and LIFETIME below the default and above the maximum. *Oracle:* RFC 8656
  §7, §9 and §12 numbers by hand, to the nanosecond.
- **`retransmit_schedule`** (U2). *Scenario:* an unanswered Allocate over `Datagram` and over
  `Stream`. *Oracle:* RFC 8489 §6.2.1 arithmetic (0, 500, 1,500, 3,500, 7,500, 15,500,
  31,500 ms; failure at 39,500 ms) and §6.2.2 (one send, failure at 39,500 ms).
- **`permissions_by_ip`** (U3). *Scenario:* a permission for 192.0.2.1, then peer datagrams
  from 192.0.2.1 on two ports and from 192.0.2.2, and a Send indication after 300 s that
  must not have refreshed it. *Oracle:* RFC 8656 §9 by hand.
- **`channel_rules`** (U3). *Scenario:* numbers 0x3FFF, 0x4000, 0x4FFF, 0x5000; rebinding a
  number to a second peer and a peer to a second number; ChannelData on 0x5000 and on an
  unbound number; a peer datagram once bound. *Oracle:* RFC 8656 §11 and §12 by hand,
  including ChannelData (not a Data indication) after the bind.
- **`peer_policy`** (U4). *Scenario:* one CreatePermission per `default_deny` prefix, its
  first and last address, the IPv4-mapped form, and an `allow` entry for 10.0.0.5 ports
  50000 to 50100. *Oracle:* LiveKit's `isRestrictedPeerIP` over the same addresses, run in
  Go from the clone, with its answers written into the case; and RFC 6890's registry.
- **`quota_and_capacity`** (U4). *Scenario:* 13 allocations for one username; a pool of two
  addresses and three usernames. *Oracle:* by hand: 486, then 508.
- **`revoke_drops_paths`** (U4). *Scenario:* revoke a subject with live allocations while both
  directions carry data under `std.sim.udp`. *Oracle:* the invariant that no `ToPeer` or
  `ToClient` for those allocations follows the call, checked on the capture, and 401 at
  TURN-5 step 4 for a new Allocate until `until_unix_s`.
- **`stream_framing`** (U5). *Scenario:* client and relay over `std.sim.net` with 1,000
  seeds of split and join points; ChannelData of every length mod 4; a first byte 0x80.
  *Oracle:* RFC 8656 §12.5 by hand: every payload arrives once, in order, byte-exact, and
  0x80 is `Framing`.
- **`sim_relay_loss`** (U5). *Scenario:* a client, the relay and an echoing UDP peer under
  `std.sim.udp` at 20% Gilbert–Elliott loss with rebinding of the client's mapping. *Oracle:*
  invariants: the allocation completes; echoed payloads are a byte-exact subsequence of
  those sent; two runs with one seed give equal captures.
- **`interop_coturn`** (U7). *Scenario:* the Hirsel client against `turnserver` with a
  long-term user, over UDP and TCP: allocate, permission, channel, echo through a UDP peer.
  *Oracle:* an independent server.
- **`interop_pion`** (U7). *Scenario:* Pion's `examples/turn-client` (UDP and TCP) against the
  Hirsel relay with minted credentials. *Oracle:* an independent client.
- **`no_key_in_recording`** (U1, U3). *Scenario:* `minted_credentials` and
  `challenge_exchange` recorded. *Oracle:* the known password, the known MD5 key and the
  known master bytes from the case occur nowhere in the recording file.

Browser behaviour (Chromium sending MESSAGE-INTEGRITY with MD5 and no
PASSWORD-ALGORITHMS, ChannelBind before media) is covered by Pion's interop history and
checked end to end by webrtc.md's headless Chromium case once it runs with
`iceTransportPolicy: "relay"`; this text adds no browser harness.

## Cost targets

PLAN (row 1.1) sets no target for TURN: it defers the TURN client, and its one number, zero
allocations per packet, is for the Binding response. This text applies the same rule to
the relay's data path, as a gate:

- **0 allocations per relayed packet**: `from_client` on ChannelData and `from_peer` on an
  established allocation and channel allocate nothing, measured with a counted allocator over
  10^6 packets of 1,200 bytes each way, after setup. The in-place ranges of TURN-7 make it
  structural.
- **0 sink calls per relayed packet**: indications and ChannelData are never authenticated,
  and a relayed packet takes no `Secret` operation; the recording of the same run has no
  frame per packet.

Throughput and latency per packet are reported (instructions per `from_peer`), and gate
nothing, since PLAN sets no number.

## Build slices

Sizes are S (under a day of one lane), M (one to three days) and L (more than three).

| slice | size | depends on | content | laws |
|---|---|---|---|---|
| **U0** STUN message layer | M | — | TURN-4 in `stun_ice`: `Key`, `Attr`, `Header`, `attrs`, `verify_key`, `Builder`; Binding functions rebuilt over them; constants; law lock rewritten | existing `binding_vectors`, `full_connectivity`, `lite_nat_rebinding`, `lite_peer_signaling` stay green |
| **U1** TLS-5 sinks | M | — | `secret.derive_digest` (MD5 over RustCrypto `md-5` as a Buck third-party crate, SHA-256 over ring), `secret.hmac_sha1`, `secret.verify_hmac_sha1`, in the OS, scripted and virtual paths; charges and recording frames as TLS-5's sinks; std hmac page | `long_term_key_vectors`, `minted_credentials` (relay half), `no_key_in_recording` |
| **U2** long-term credentials and client | L | U0, U1 | `core/programs/turn/`<!-- doc-ref: planned --> package; TURN-5 (`mint`, `relay_key`, nonce, server check order as a function the relay calls, client rules, USERHASH); TURN-6 client | `userhash_vector`, `challenge_exchange`, `bid_down_refused`, `nonce_bound_to_tuple`, `expired_costs_no_derivation`, `retransmit_schedule`, client half of `lifetimes_exact` |
| **U3** relay server | L | U2 | TURN-7 Allocate, Refresh, CreatePermission, ChannelBind, Send, Data, ChannelData; pool; timers; in-place framing | `allocate_check_order`, `unsupported_answers`, `lifetimes_exact`, `permissions_by_ip`, `channel_rules` |
| **U4** relay policy, quota, revocation | M | U3 | TURN-7 policy and `default_deny`, quota, capacity; TURN-9 | `peer_policy`, `quota_and_capacity`, `revoke_drops_paths` |
| **U5** streams and simulation | M | U3 | TURN-8 `Framer`; stream origins; the sim runs | `stream_framing`, `sim_relay_loss` |
| **U6** PROXY v2 header | S | — | `proxy.step`, the generic sans-IO PROXY protocol v2 header decoder of [launch.md H1](launch.md#the-header-required-checked-consumed-exactly); `proxy.accept` and the front are H1's slices | the decoder half of `proxy_v2_header` (launch.md H1 laws) |
| **U7** interop and cost | M | U4, U5 | `interop.py` against coturn and Pion from `/tmp` clones; counted-allocator run | `interop_coturn`, `interop_pion`, the cost targets |
| **U8** SFU wiring | S | U4, webrtc S3 | the SFU's deployment runs the relay as a sibling place; signaling mints TURN credentials beside ICE ones and forwards revokes to the relay (TURN-9) | webrtc's browser end-to-end case with `iceTransportPolicy: "relay"` over UDP and TCP |

U0, U1 and U6 run in parallel. U3 waits on U2 because the relay uses TURN-5's check order.
U4 and U5 run in parallel after U3.

**Model fit.** Sol takes U0, U3, U5, U7 and U8 from this text. Devin SWE-2.1 high takes U6
(small, self-contained). U1, U2 and U4 are the security-bearing slices: U1 extends TLS-5's
provider surface and U2 holds the credential design, so both are Opus; U4 (policy, quota,
revocation) is Sol. Each of U1, U2 and U4 gets an Astra security attack before it lands:
key custody, the derivation sink's opacity and frames, the check order's pre-authentication
cost, bid-down, nonce replay, and relay abuse toward internal ranges.
