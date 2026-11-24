# DTLS and SRTP

Version note (2026-11-24, media-dtls-spec-2317): first text of DTLS-1 to DTLS-9 and SRTP-1
to SRTP-6, the Wave 1 rows 1.2 and 1.3 of the real-time media track. Not yet built: every
fence is `unbuilt`, and every test path is planned. The sections end with
[build laws](#build-laws), [cost targets](#cost-targets) and [build slices](#build-slices).

Two pure Hirsel packages sit over [TLS-8](tls.md#tls-8-packet-ciphers-and-key-agreement) and
under any program that carries real-time media over UDP ([NET-15 to NET-17](net.md#udp)):

- `core/programs/dtls/`<!-- doc-ref: planned -->: a sans-IO DTLS 1.2 endpoint
  ([RFC 6347](https://www.rfc-editor.org/rfc/rfc6347)) for ECDHE-ECDSA AEAD suites, with
  certificate-fingerprint pinning ([RFC 8122](https://www.rfc-editor.org/rfc/rfc8122)), the
  SDP `setup` role rule, and the one-use `use_srtp` exporter
  ([RFC 5764](https://www.rfc-editor.org/rfc/rfc5764)).
- `core/programs/srtp/`<!-- doc-ref: planned -->: SRTP and SRTCP sessions
  ([RFC 3711](https://www.rfc-editor.org/rfc/rfc3711), [RFC 7714](https://www.rfc-editor.org/rfc/rfc7714))
  over TLS-8 schedules: header parsing, the rollover counter, the replay window and the SRTCP
  trailer.

Both are general primitives. They name no application, room, participant or media kind.
A selective forwarder, a recorder, a media gateway and a test peer use the same operations.
Neither package reaches `Foreign` code, so `hirsel test --sim` and recording and replay hold
for every program built on them ([FFI-8](foreign.md)). Neither performs I/O: the caller owns
sockets, clocks and demultiplexing. A UDP datagram whose first byte is 20 to 63 is DTLS and
one whose first byte is 128 to 191 is RTP or RTCP
([RFC 7983 §7](https://www.rfc-editor.org/rfc/rfc7983#section-7)); the caller routes it.

**What TLS-8 already fixes, and these packages do not repeat.** Key agreement, the TLS 1.2
PRF, the master secret with and without EMS, the key block, Finished values, the
`EXTRACTOR-dtls_srtp` exporter, the SRTP KDF, every nonce layout, and sender ownership live
in TLS-8. Its vectors (PC2, PC10) are the oracle for those values. The laws here cover only
what the libraries add: message framing, flights, the transcript, header boundaries, the
rollover counter, replay and the SRTCP trailer.

## DTLS 1.3 is out of Wave 1

Wave 1 implements DTLS 1.2 only. This overturns PLAN row 1.2, which listed "DTLS 1.2 + DTLS
1.3".

- **The consumer asks for 1.2.** The Wave 1 consumer contract names the DTLS 1.2 handshake,
  its flights, roles, fingerprint and exporter, and nothing of 1.3.
- **1.3 is additive.** TLS-8 already holds the DTLS 1.3 key schedule (`tls13` with
  `Dtls13(s)`), its record-number masks and the full 64-bit sequence width. A later `dtls13`
  module adds the unified header, record-number encryption, ACK, HelloRetryRequest and
  KeyUpdate behind the same `Endpoint` operations (DTLS-3). No operation in this text
  changes when it lands, and nothing here needs migrating.
- **It halves the attack surface of the one security review.** DTLS 1.3 brings its own state
  machine (ACKs replace implicit flight acknowledgement, RFC 9147 §7), record-number
  encryption and epoch-per-KeyUpdate. Reviewing one version well is worth more than
  reviewing two once.
- **A 1.3-capable peer still connects.** A client that offers 1.3 in `supported_versions`
  and 1.2 beside it gets a 1.2 handshake (law `offered_13_negotiates_12`). Such a client can
  send a ClientHello larger than one datagram (large key shares), so fragmented ClientHello
  reassembly is required here too (law `fragments_reassemble`).

Reversibility: fully reversible and additive. The decision is logged in `ENDSTATE.md`.

## DTLS

### DTLS-1: Scope

One `Endpoint` holds one DTLS 1.2 association with one peer address. It implements:

| item | Wave 1 | reference |
|---|---|---|
| version | DTLS 1.2 (`{254, 253}`) only; `legacy_version` of a ClientHello may be `{254, 253}` or `{254, 255}` | RFC 6347 §4.1 |
| cipher suites | `TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256` (0xC02B), `TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384` (0xC02C), `TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256` (0xCCA9): TLS-8's `Suite12` | RFC 5289, RFC 7905 |
| groups | `x25519` (0x001D), `secp256r1` (0x0017): TLS-8's `Group` | RFC 8422 |
| own signature | `ecdsa_secp256r1_sha256` (0x0403), with a P-256 key from `secret.p256_key` | RFC 8422 §5.4 |
| peer signature | 0x0403 over a P-256 key, `ecdsa_secp384r1_sha384` (0x0503) over a P-384 key, both strict DER (`p256.verify_der`, `p384.verify_der`) | RFC 8422 |
| extensions | `supported_groups` (10), `ec_point_formats` (11, uncompressed only), `signature_algorithms` (13), `use_srtp` (14), `extended_master_secret` (23), `renegotiation_info` (0xFF01, empty only) | RFC 5764 §9, RFC 7627 §5.1, RFC 5746 |
| Extended Master Secret | required: a peer that does not negotiate it fails the handshake | RFC 7627 §5.2 |
| client certificate | always requested by the server and always sent by the client | RFC 8122 §6.2 |
| HelloVerifyRequest cookie | per server, by admission (DTLS-5) | RFC 6347 §4.2.1 |
| application data | sealed records in epoch 1, for a caller that tunnels a protocol over DTLS | RFC 6347 §4.1 |

Not in Wave 1, each refused explicitly: renegotiation (a ClientHello after the handshake
gets a warning `no_renegotiation` (100) alert and changes nothing), session resumption and
tickets (a non-empty `session_id` or `SessionTicket` from a client is ignored, and the
server always sends an empty `session_id`), PSK, RSA, DHE, CBC and CCM suites, compression
other than null, `srtp_mki` (a non-empty MKI fails the handshake), and DTLS 1.0.

### DTLS-2: Types

```hirsel unbuilt
// package dtls
pub type Setup = Active | Passive | ActPass
pub type HashFunc = Sha256 | Sha384 | Sha512
pub type Fingerprint { hash: HashFunc, digest: Bytes }

pub type Config {
    suites: List[schedule.Suite12],      // in preference order; non-empty
    groups: List[secret.Group],          // in preference order; non-empty
    srtp: List[schedule.SrtpProfile],    // in preference order; empty: no use_srtp
    mtu: u16,                            // largest datagram sent; 256 to 65,507; default 1,200
    start_rto_ns: u64,                   // default 1 s
    max_rto_ns: u64,                     // default 60 s
    retries: u8,                         // retransmissions per flight; default 7
    handshake_ns: u64,                   // whole handshake deadline; default 60 s
    max_message: u32,                    // largest handshake message reassembled; default 16,384
    max_ahead: u16                       // future handshake fragments held; default 32
}

pub opaque type Identity                // a P-256 signing key and its certificate DER
pub opaque type Endpoint                // one association; holds TLS-8 resources, so it is move-only

pub type State = Handshaking | Connected | Closed(Close)
pub type Close = Local | PeerClosed | PeerAlert(u8) | SentAlert(u8) | Timeout
pub type Poll = Send(u64) | WakeAt(u64) | Idle
pub type Admission = Verified | Cookie(Inline[u8, 32])
pub type Discards { malformed: u64, replayed: u64, tampered: u64, stale_epoch: u64, overflow: u64 }

pub type DtlsError = Config | Setup | Fingerprint | Closed(Close) | NotConnected
                   | Taken | NoSrtp | Key(KeyError) | Schedule(schedule.ScheduleError)
                   | Packet(packet.PacketError) | Capacity(Capacity)

pub fn defaults() -> Config
pub fn identity(take key: Secret, take certificate: Bytes) -> Result[Identity, DtlsError]
pub fn fingerprint(i: Identity, h: HashFunc) -> Fingerprint
pub fn fingerprint_text(f: Fingerprint, mut out: Bytes) -> u64
pub fn parse_fingerprint(text: Bytes) -> Result[Fingerprint, DtlsError]
pub fn negotiate_setup(offer: Setup, answer: Setup) -> Result[schedule.Side, DtlsError]

pub fn client(take id: Identity, c: Config, peer: Addr) -> Result[Endpoint, DtlsError]
pub fn server(take id: Identity, c: Config, peer: Addr, a: Admission) -> Result[Endpoint, DtlsError]
pub fn expect(mut e: Endpoint, take fingerprints: List[Fingerprint]) -> Result[(), DtlsError]
pub fn receive(mut e: Endpoint, now: u64, r: Random, datagram: Bytes, mut data: Bytes) -> Result[u64, DtlsError]
pub fn poll(mut e: Endpoint, now: u64, r: Random, mut out: Bytes) -> Result[Poll, DtlsError]
pub fn send(mut e: Endpoint, data: Bytes, mut out: Bytes) -> Result[u64, DtlsError]
pub fn close(mut e: Endpoint, mut out: Bytes) -> Result[u64, DtlsError]
pub fn state(e: Endpoint) -> State
pub fn peer_certificate(e: Endpoint) -> Option[Bytes]
pub fn srtp_profile(e: Endpoint) -> Option[schedule.SrtpProfile]
pub fn take_srtp(mut e: Endpoint) -> Result[schedule.Schedule, DtlsError]
pub fn discards(e: Endpoint) -> Discards
```

`now` is nanoseconds on the caller's monotonic clock. A `now` smaller than an earlier call's
is `Config`, with no effect. `Random` is the source the endpoint draws its 32-byte hello
random, its key share and its ECDSA signature from (randomness, DTLS-8). The endpoint keeps
no `Random`; the caller passes one at every call that can start a flight.

`Config` is checked at construction: an empty `suites` or `groups`, a duplicate entry, an
`mtu` outside 256 to 65,507, `start_rto_ns` of 0 or above `max_rto_ns`, `handshake_ns` of 0,
or `max_message` below 2,048 is `Config`. No field is silently clamped.

### DTLS-3: Sans-IO driving

The endpoint is driven by three calls, as [dimpl](#prior-art) is:

- **`receive(mut e, now, r, datagram, mut data)`** takes one whole UDP datagram from the
  peer. It processes every record in it in order (a datagram may hold several, RFC 6347
  §4.1.1). Decrypted application data is appended to `data`'s spare and its length is the
  result. Handshake progress queues the next flight, which `poll` then hands out. A record
  that cannot be used is discarded and counted (DTLS-6); a discard is never an error.
- **`poll(mut e, now, r, mut out)`** hands out queued work. `Send(n)` appends one datagram of
  `n` bytes, at most `mtu`, to `out`; the caller sends it and polls again. `WakeAt(t)` says
  nothing is queued and the endpoint wants a `poll` at or after `t` (a retransmission or the
  handshake deadline). `Idle` says nothing is queued and no timer is armed. A `poll` at or
  after a `WakeAt` time runs that timer first. An `out` whose spare is shorter than the next
  datagram is `Capacity` before issue, with the datagram kept.
- **`send(mut e, data, mut out)`** seals `data` as application-data records in epoch 1,
  splitting at `mtu`, and appends the datagrams to `out`, answering the bytes appended. It is
  `NotConnected` before `Connected`.

A client starts its handshake at its first `poll`. Every `DtlsError` from `receive`, `poll`
or `send` other than `Capacity`, `Config` and `NotConnected` is **fatal**: the state becomes
`Closed`, every later call answers `Closed(c)`, and the caller drops the endpoint. Transient
conditions (malformed, replayed, tampered or stale records) are discards, never errors, as
in dimpl (`src/lib.rs:80`).

State moves `Handshaking` to `Connected` once, and then to `Closed`. `Connected` means all
of: the peer's Finished verified, the peer's certificate signature over the handshake
verified, and the certificate's digest equal to one of the fingerprints given to `expect`
(DTLS-7).

### DTLS-4: Records

**Framing.** A record is the 13-byte `DTLSPlaintext` header (type, version, epoch, 48-bit
sequence number, length) and a fragment (RFC 6347 §4.1). The endpoint accepts content
types 20 (change_cipher_spec), 21 (alert), 22 (handshake) and 23 (application_data). A
record whose length runs past the datagram, whose version is not `{254, 253}` (or
`{254, 255}` in an initial ClientHello record), or whose type is other is discarded as
`malformed`, with the rest of the datagram discarded too, since its boundaries are lost.

**Epochs.** Epoch 0 carries the plaintext handshake. Epoch 1 is protected by the `dtls12`
schedule's `Application` space: one sealer, issued with domain 0 and floor 0, and one opener.
The endpoint seals with TLS-8's DTLS 1.2 rows: for AES-GCM it writes the record number
`epoch ‖ seq` as the 8-byte explicit nonce, and `extra` is the 13 bytes
`epoch ‖ seq ‖ type ‖ version ‖ plaintext length` (RFC 6347 §4.1.2.1, RFC 5246 §6.2.3.3).
Record sequence numbers start at 0 in each epoch and increase by one per record sent, every
retransmission included (RFC 6347 §4.2.4), so the sealer's counter is always increasing. The
one exception is a `Cookie` server's HelloVerifyRequest and first ServerHello (DTLS-5
Admission), which are epoch 0 and never sealed. A
record in epoch 1 that arrives before the endpoint has epoch-1 keys (a Finished overtaking
the ChangeCipherSpec) is held, at most `max_ahead` records counted with handshake
fragments, and processed when the keys exist; past that it is discarded as `overflow` and
retransmission recovers it. A record in any other epoch is `stale_epoch`.

**Replay.** Each epoch has a 64-record sliding window (RFC 6347 §4.1.2.6), as in dimpl's
`ReplayWindow` (`src/window.rs`). A record whose number is in the window and already
marked, or below it, is discarded as `replayed` before decryption. The window is updated only
after the record authenticates. Epoch 0 records cannot authenticate, so the epoch-0 window
marks a record once it parses; it only filters exact duplicates, and a forged epoch-0 record
can at worst make the endpoint ignore one genuine copy, which retransmission repairs. After
`Connected`, the only epoch-0 records an endpoint acts on are the retransmissions that
DTLS-5 Final flights names; the rest are discarded as `stale_epoch`.

**Invalid records.** A record that fails authentication is discarded as `tampered` and
sends no alert (RFC 6347 §4.1.2.7). The association continues.

**Alerts.** A received fatal alert (level 2), or a received `close_notify` (0), closes the
endpoint as `PeerAlert(n)` or `PeerClosed`. A warning alert other than `close_notify` is
ignored. When the endpoint fails a handshake it queues one fatal alert record with the
description the table in DTLS-5 gives, protected if epoch 1 is active, and closes as
`SentAlert(n)`. `close` queues `close_notify` and closes as `Local`.

### DTLS-5: Handshake and flights

**Flights.** The full handshake is RFC 6347 §4.2.4's six flights, with the optional
cookie exchange as flights 1 and 2:

| flight | sender | messages |
|---|---|---|
| 1 | client | ClientHello |
| 2 | server | HelloVerifyRequest (only under `Cookie`) |
| 3 | client | ClientHello with the cookie (only under `Cookie`) |
| 4 | server | ServerHello, Certificate, ServerKeyExchange, CertificateRequest, ServerHelloDone |
| 5 | client | Certificate, ClientKeyExchange, CertificateVerify, ChangeCipherSpec, Finished |
| 6 | server | ChangeCipherSpec, Finished |

**ClientHello.** The client sends `client_version` `{254, 253}`, a fresh 32-byte random
drawn from `r`, an empty `session_id`, an empty cookie at first, `Config.suites` in order,
null compression, and the extensions of DTLS-1. `use_srtp` lists `Config.srtp` in order with
an empty MKI and is left out when `Config.srtp` is empty.

**Server choice.** The server picks the first entry of its own `suites` the client offered,
the first of its own `groups` the client offered, and the first of its own `srtp` profiles
the client offered. It fails with `handshake_failure` (40) when no suite or group is shared,
when the client does not offer `extended_master_secret`, or when `Config.srtp` is non-empty
and no profile is shared. It fails with `illegal_parameter` (47) when the client's `srtp_mki`
is non-empty, or `use_srtp` lists no profile or an odd number of bytes. A ClientHello
carrying `supported_versions` is answered as DTLS 1.2: the server ignores that extension and
any `key_share` and replies with a 1.2 ServerHello (law `offered_13_negotiates_12`).

**Server flight.** ServerHello carries the chosen suite, `use_srtp` with the one chosen
profile and an empty MKI (RFC 5764 §4.1.1), `extended_master_secret`, `renegotiation_info`
empty, and `ec_point_formats` with uncompressed only. Certificate carries the identity's one
DER certificate. ServerKeyExchange carries the server's key share for the chosen group
(`secret.key_share`) and the ECDSA signature, by `secret.sign(key, P256, r, ..)` then
re-encoded as DER, over `client_random ‖ server_random ‖ 0x03 ‖ named_curve ‖ point length ‖
point` (RFC 8422 §5.4). CertificateRequest lists `ecdsa_sign` (64) and signature algorithms
0x0403 and 0x0503, with no certificate authorities.

**Client flight.** The client checks the server's ServerHello (the suite, group and profile
are ones it offered; `extended_master_secret` is present; it fails with `illegal_parameter`
otherwise, or `handshake_failure` for a missing EMS), takes the public key from the server's
certificate (`std.x509_key`, below), verifies the ServerKeyExchange signature (a failure is
`decrypt_error` (51)), creates its key share, sends its Certificate, ClientKeyExchange with
its public point, CertificateVerify, ChangeCipherSpec and Finished. The server verifies the
client's certificate signature over the transcript (`decrypt_error` on failure), its
Finished (`decrypt_error`), and answers with flight 6.

**Keys.** After ClientKeyExchange each side calls `schedule.dtls12(suite, side, 1, take
share, peer point, client_random, server_random, Some(session_hash))`, where
`session_hash` is the transcript hash through ClientKeyExchange (RFC 7627 §4). `finished`
and `verify_finished` compute and check the Finished values. A delivered `Invalid` from
`dtls12` (an invalid peer point) fails with `illegal_parameter`. A delivered `Issued` from
the agreement registry fails with `handshake_failure` and surfaces as `Schedule(Issued)`.

**Transcript.** The transcript is every handshake message sent and received from the
ClientHello that the server answers with flight 4 onwards, excluding HelloVerifyRequest and
the ClientHello before it (RFC 6347 §4.2.1). Each message enters it once, in DTLS form: its
12-byte header with `fragment_offset` 0 and `fragment_length` equal to `length`, then its body,
however it was fragmented on the wire (RFC 6347 §4.2.6). Retransmitted copies do not enter
it again. The endpoint keeps the transcript bytes (CertificateVerify signs them raw, RFC 5246
§7.4.8, and the hash depends on the suite chosen after ClientHello) and frees them at
`Connected`. Its size is bounded by `3 × max_message`; a larger transcript fails with
`handshake_failure`.

**Fragmentation.** A handshake message longer than fits one datagram is split into fragments
(RFC 6347 §4.2.3) that each fit, with the record header and, in epoch 1, the 8-byte explicit
nonce and 16-byte tag counted. Several whole records go into one datagram while they fit
`mtu`. On receipt each message is reassembled by `message_seq` from fragments in any order,
overlapping or duplicated. A fragment whose `length` differs from an earlier fragment of
the same message, whose range runs past `length`, or whose `length` exceeds `max_message`
fails the handshake with `illegal_parameter`. A fragment of a message more than one ahead of
the next expected one is held up to `max_ahead` fragments and then discarded as `overflow`.
A fragment of a message already processed is a retransmission (below).

**Timers.** A flight is retransmitted whole (RFC 6347 §4.2.4). The first transmission arms
the retransmission timer at `start_rto_ns`. Each expiry resends the flight with fresh record
numbers and doubles the timer, up to `max_rto_ns` (RFC 6347 §4.2.4.1). After `retries`
retransmissions of one flight with no answer, the next expiry closes the endpoint as
`Timeout`. Receiving any part of the peer's next flight stops the timer. A flight that was
answered without being retransmitted resets the next flight's timer to `start_rto_ns`; a
flight that needed a retransmission keeps the doubled value for the next flight (§4.2.4.1).
`handshake_ns` after the first flight is sent (client) or the first ClientHello is accepted
(server), an endpoint still `Handshaking` closes as `Timeout`. There is no jitter: every
time is a pure function of the inputs, so sim traces are exact.

**Final flights.** The server sends the last flight (6). It keeps flight 6 after
`Connected`, and each time it receives a retransmission of flight 5 it resends flight 6 once
(§4.2.4), until the endpoint is dropped or closed. The client, once it has flight 6, treats a
retransmitted flight 4 as noise. A server that receives a retransmitted flight 3 while
waiting for flight 5 resends flight 4. Every other retransmitted message is ignored.

**Admission (cookies).** `server(.., Verified)` skips flights 2 and 3: the caller attests the
peer address is proven, for example by an ICE connectivity check that carried
`MESSAGE-INTEGRITY` under the peer's credentials ([RFC 8445](https://www.rfc-editor.org/rfc/rfc8445)).
`server(.., Cookie(key))` answers every ClientHello that lacks a valid cookie with a
HelloVerifyRequest whose cookie is `HMAC-SHA256(key, address bytes ‖ client_random)` (32
bytes, `std.hmac.sha256`), and creates no other state. The address bytes are the peer
`Addr`'s family byte, address and big-endian port. A ClientHello is admitted when its cookie
equals that value in constant time (`std.hmac.equal`). The cookie key is caller bytes, not a
protocol key: it authenticates reachability, protects no traffic, and the caller draws it
from its own `Random`. dimpl binds the cookie to the client random only
(`src/dtls12/server.rs:1217`); binding the address too means a cookie seen on one path cannot
admit a ClientHello from another. Before admission, a `Cookie` server sends exactly one
HelloVerifyRequest datagram per ClientHello datagram received, which is 13 (record header) + 12 (handshake header) + 2 (`server_version`) + 1 (cookie
length) + 32 = 60 bytes (law `cookie_bounds_unproven_address`). The HelloVerifyRequest and the
ServerHello that answers the cookie ClientHello take their record sequence numbers from the
ClientHello they answer (RFC 6347 §4.2.1), because a stateless server keeps no counter. Their
`message_seq` follows the client's: the HelloVerifyRequest is 0, and the ServerHello and the
rest of flight 4 continue from the cookie ClientHello's `message_seq` (1). After that the
server numbers its records and messages normally.

**Failures.** Each handshake failure sends one fatal alert and closes as `SentAlert(n)`:

| failure | alert |
|---|---|
| no shared suite or group, no EMS, no shared SRTP profile, transcript too large | `handshake_failure` (40) |
| malformed or contradictory message, non-empty MKI, invalid peer point, fragment inconsistency | `illegal_parameter` (47) |
| a message out of order for the state | `unexpected_message` (10) |
| peer certificate missing or not P-256/P-384 ECDSA, or fingerprint mismatch | `bad_certificate` (42) |
| a signature or Finished that does not verify | `decrypt_error` (51) |
| a version other than DTLS 1.2 in ServerHello | `protocol_version` (70) |

### DTLS-6: Discards

Every datagram or record the endpoint drops without failing is counted in `discards(e)`:
`malformed` (framing), `replayed` (window), `tampered` (authentication), `stale_epoch`
(an epoch it holds no keys for and cannot hold) and `overflow` (the `max_ahead` bound). A
dropped datagram never changes any other state. These counts are the observable cost of noise
and attack traffic; nothing is dropped silently.

### DTLS-7: Certificates, fingerprints and roles

**Identity.** `identity(take key, take certificate)` pairs a P-256 signing key (policy
`sign(p256)`, from `secret.p256_key`) with its DER certificate, as
[`std.x509_build.self_signed_p256`](../../core/std/api/x509_build.md) makes it. It checks
that the certificate's subject public key is the key's point (by signing a fixed message and
verifying with `p256.verify` against the certificate's key) and is `Config` otherwise. The
endpoint never validates a certificate chain, name or validity period: in this model the
peer is authenticated by the fingerprint that the signalling channel carried
([RFC 8122 §6.2](https://www.rfc-editor.org/rfc/rfc8122#section-6.2)).

**Reading a certificate's key.** A new std module, `std.x509_key`<!-- doc-ref: planned -->,
reads the `subjectPublicKeyInfo` of a DER certificate and answers `P256(point)` or
`P384(point)` for an `id-ecPublicKey` key on those curves with an uncompressed point, and
`Invalid` for anything else, including trailing bytes or a non-minimal DER length. It reads
no other field. It lives in std because it is a general X.509 primitive, beside
`x509_build`.

**Fingerprints.** `fingerprint(i, h)` is the hash `h` of the identity's whole certificate
DER, signature included. `fingerprint_text(f, mut out)` appends RFC 8122 §5's form:
`sha-256`, `sha-384` or `sha-512`, a space, and the digest as uppercase hexadecimal byte
pairs separated by colons. `parse_fingerprint(text)` reads the same form after the
`fingerprint:` attribute name: the hash name case-insensitively, the hex in either case (a
reader is liberal; RFC 8122's grammar requires uppercase on output), and refuses any other
hash name, a digest of the wrong length, or malformed pairs as `Fingerprint`. SHA-1 and
SHA-224, though in RFC 8122's registry, are refused: no current peer offers only those.

**Pinning.** `expect(mut e, take fingerprints)` gives the peer fingerprints from signalling;
any one matching suffices (RFC 8122 §5 allows several). It may be called before the
handshake or after it has completed, because an answer can outrace signalling (RFC 8122
§6.2). Until it has been called, a completed handshake stays `Handshaking`: no application
data is delivered, `take_srtp` is `NotConnected`, and the endpoint keeps answering
retransmissions. When the peer's certificate is known and fingerprints are given, the first
`receive` or `poll` compares them. A mismatch fails with `bad_certificate` (42), as RFC 8122
§6.2 requires of both sides. A second `expect` is `Fingerprint`. Fingerprints and the peer
certificate are public data; the comparison need not be constant time.

**Roles.** `negotiate_setup(offer, answer)` applies the SDP `setup` rule
([RFC 5763 §5](https://www.rfc-editor.org/rfc/rfc5763#section-5), RFC 8122 §6.2) and
answers the **offerer's** DTLS side. The `active` endpoint is the DTLS client.

| offer | answer | offerer is |
|---|---|---|
| `ActPass` | `Active` | `Server` |
| `ActPass` | `Passive` | `Client` |
| `Active` | `Passive` | `Client` |
| `Passive` | `Active` | `Server` |
| anything else (`ActPass` in an answer, two equal roles) | | `Setup` error |

The answerer's side is the other one. The caller passes the side to `client` or `server`.
Neither package reads SDP: the caller extracts `a=setup` and `a=fingerprint` and passes them
here.

### DTLS-8: Keys, randomness and revocation

**The exporter.** `take_srtp(mut e)` is defined once the endpoint is `Connected` and
negotiated a profile. It calls TLS-8's `schedule.srtp(s, profile)` on the endpoint's `dtls12`
schedule and gives the caller the SRTP schedule, whose sending keys are this side's and
receiving keys the peer's (RFC 5764 §4.2). A second call is `Taken`, from TLS-8's `Issued`.
An endpoint that negotiated no profile answers `NoSrtp`. `srtp_profile(e)` names the profile.

**Nothing leaks.** The endpoint holds its share, schedule, sealer and opener for its whole
life, and no operation returns a sealer, an opener or a `dtls12` schedule. The only key
material that leaves is the SRTP schedule from `take_srtp`, which is itself move-only.

**Revocation.** Dropping the `Endpoint` drops its schedule, sealer and opener, after which
nothing can seal or open a record of that association. Dropping an SRTP `Session` (SRTP-2)
does the same for media. A caller that must stop forwarding synchronously therefore drops
both objects, and the drop is the fence: there is no other handle to stop. TLS-8 sealers and
openers keep working after their schedule drops, which is why neither package ever hands one
out.

**Randomness.** Every draw (the hello random, `key_share`, ECDSA `sign`) uses the `r` the
caller passes. Under TLS-8, `sign(P256)` of a key drawn from a host `Random` with a virtual
`Random` is `Denied`, so an endpoint is passed the same kind of `Random` its identity key was
made with: host keys with host randomness, simulated keys with a virtual stream. A mismatch
fails the handshake with `handshake_failure` and surfaces as `Key(Denied)`. Under simulation
each endpoint needs its own stream: two same-side handshakes that draw the same seed agree on
the same value, and TLS-8's agreement registry refuses the second with `Issued`.

**Errors from the layers below.**

| below | value | Endpoint outcome |
|---|---|---|
| `secret.key_share`, `secret.sign` | `Capacity(c)` | `Capacity(c)` from the call that started the flight; the flight is not sent and the endpoint stays usable, so the next `poll` retries |
| `secret.sign` | `Denied`, `Invalid` | fatal `Key(..)`, alert 40 |
| `schedule.dtls12` | `Invalid` delivered | fatal, alert 47 |
| `schedule.dtls12` | `Issued` delivered | fatal `Schedule(Issued)`, alert 40 |
| `schedule.*` | `Capacity(c)` before issue | `Capacity(c)`; retried at the next call |
| `schedule.srtp` | `Issued` | `Taken` |
| `packet.seal` | `Range` (2^48 records in epoch 1) | fatal `Packet(Range)`, closes with `close_notify` |
| `packet.seal` | `Capacity(c)` | `Capacity(c)`, record not sent |
| `packet.open` | `Tampered`, `Range` | discard, `tampered` |
| `packet.seal` | `Reused`, `TooLong` | cannot happen: the endpoint numbers records itself and splits at `mtu` |

### DTLS-9: What the caller does

The caller owns the socket and the clock, demultiplexes datagrams by first byte, decides
when to construct the endpoint (after ICE selects a path, or at once for an offerer that must
listen first), passes the setup side and the fingerprints from signalling, calls `poll` until
`Idle` or `WakeAt` after every `receive`, and polls again at `WakeAt`. Once `Connected`, it
calls `take_srtp` and builds an SRTP `Session`. Lifetime limits beyond the handshake (rekey)
are not in Wave 1: a new association is a new endpoint.

## SRTP

### SRTP-1: Scope

One `Session` protects and unprotects the RTP and RTCP of one association, under one
TLS-8 SRTP schedule per direction. It supports `AEAD_AES_128_GCM` and `AEAD_AES_256_GCM`
(RFC 7714) and `AES_CM_128_HMAC_SHA1_80` (RFC 3711), TLS-8's `SrtpProfile`. The keys come
from `dtls.take_srtp` (both directions in one schedule) or from two TLS-8 imports (one
`srtp_import(send)` and one `srtp_import(receive)`), for keys signalled another way. No MKI,
no key derivation rate other than 0, no `AES_CM_128_HMAC_SHA1_32`, no NULL cipher.

### SRTP-2: Types

```hirsel unbuilt
// package srtp
pub type Discard = Malformed | Replayed | TooOld | Tampered
pub type SrtpError = Discarded(Discard) | Reused | Exhausted | Streams | Retired
                   | Short | Direction | Schedule(schedule.ScheduleError)
                   | Capacity(Capacity)
pub type Limits { send_streams: u32, receive_streams: u32 }
pub type Opened { length: u64, ssrc: u32, index: u64 }

pub opaque type Session                 // holds TLS-8 sealers and openers; move-only

pub fn session(take s: schedule.Schedule, p: schedule.SrtpProfile, l: Limits) -> Result[Session, SrtpError]
pub fn session_pair(take send: schedule.Schedule, take receive: schedule.Schedule, p: schedule.SrtpProfile, l: Limits) -> Result[Session, SrtpError]
pub fn send_from(mut s: Session, ssrc: u32, roc: u32, seq: u16) -> Result[(), SrtpError]
pub fn receive_from(mut s: Session, ssrc: u32, roc: u32, seq: u16) -> Result[(), SrtpError]
pub fn end_send(mut s: Session, ssrc: u32)
pub fn end_receive(mut s: Session, ssrc: u32)
pub fn protect_rtp(mut s: Session, mut buf: Bytes) -> Result[u64, SrtpError]
pub fn protect_rtp_into(mut s: Session, header: Bytes, src: Bytes, from: u64, mut dst: Bytes) -> Result[u64, SrtpError]
pub fn unprotect_rtp(mut s: Session, mut buf: Bytes) -> Result[Opened, SrtpError]
pub fn protect_rtcp(mut s: Session, mut buf: Bytes) -> Result[u64, SrtpError]
pub fn unprotect_rtcp(mut s: Session, mut buf: Bytes) -> Result[u64, SrtpError]
pub fn tag_room(p: schedule.SrtpProfile) -> u64
```

`session` takes a schedule holding both directions (from `take_srtp`); `session_pair` takes
a `send` import and a `receive` import of the same profile. A schedule of the wrong
direction or profile is `Direction`. The send and receive keys are always different: under
DTLS-SRTP they are the client's and the server's halves of the exporter, and under imports
they are two separately issued secrets. Nothing in a `Session` lets a received packet's
context serve sending, or the reverse.

### SRTP-3: Streams, ROC and sending

**Send streams.** Each SSRC a session sends on has a sealer for `Rtp` and one for `Rtcp`,
issued from the schedule at the SSRC's first packet (TLS-8 domain = SSRC), plus its highest
sent index, ROC and SRTCP index. At most `send_streams` SSRCs exist at once; one more is
`Streams`, before issue. `end_send(ssrc)` frees the stream's sealers but keeps the SSRC
marked: TLS-8 never issues a domain twice in one send epoch, so a later packet on an ended
SSRC is `Retired`. A sender that needs a new stream uses a new SSRC.

**Sending index.** The packet index is `i = 2^16 × ROC + SEQ` (RFC 3711 §3.3.1). The sender
starts each SSRC at ROC 0 unless `send_from(ssrc, roc, seq)` set it. For each packet it
estimates `v` against its highest sent `s_l` with RFC 3711 Appendix A and uses
`i = 2^16 × v + SEQ`. The sealer refuses any index not above every index it sealed (TLS-8
sender ownership), so `protect_rtp` of an index at or below the highest sent is `Reused`
before issue, with `buf` unchanged and no counter consumed. This is a real constraint on
callers:

- A forwarder preserves the packet's mapped media sequence number in source order (RTP-11).
  For Late(index, ts), send an RFC 4588 repair only if RTX was negotiated for that format.
  Use the low 16 bits of index as OSN, the mapped timestamp and marker, and the RTX SSRC,
  payload type and next RTX sequence number. Otherwise drop the packet. Never allocate a new
  media sequence number to a late packet.
- A retransmission on the original SSRC resends the bytes already protected (TLS-8: a
  protocol never seals the same number twice); a retransmission that must be sealed again goes
  on a separate retransmission SSRC with its own sequence (RFC 4588).

The index `2^48 − 1` seals, and the next is `Exhausted`: the session must be replaced
(RFC 7714 §8.4). Nothing wraps.

**Protecting RTP.** `protect_rtp(mut s, mut buf)` parses the header: version 2, then
`12 + 4 × CC` bytes, then, when X is set, the 4-byte extension header and `4 × length` bytes.
A header that runs past `buf`, or a version other than 2, is `Malformed` with `buf`
unchanged. The whole header, extension included, is authenticated and not encrypted; the
payload and any padding (inside the payload, RFC 3711 §3.1) are encrypted. For AEAD, `seal`
with `aad_at = 0`, `at` = header length, empty `extra`. For CM, `extra` is the 4-byte ROC
(RFC 3711 §4.2). The tag (16 or 10 bytes) is appended to `buf`; `buf` must have
`tag_room(p)` bytes spare, or it is `Short` before issue. The result is the protected length.

**Fan-out.** `protect_rtp_into(mut s, header, src, from, mut dst)` appends `header` to `dst`,
then the ciphertext of `src[from..]` and the tag, through TLS-8's `seal_into`, and answers
the bytes appended. The SSRC and sequence come from `header`. One shared payload is
encrypted once per receiving session without being copied first.

**Protecting RTCP.** `protect_rtcp(mut s, mut buf)` reads the sender SSRC at bytes 4..7 of
the first RTCP packet (a compound shorter than 8 bytes, or version other than 2, is
`Malformed`). It uses and then increments that stream's SRTCP index, starting at 0, and
always encrypts (`E = 1`). Under AEAD it seals with the first 8 bytes as AAD and `extra` =
`E ‖ index`, then appends `E ‖ index` after the tag (RFC 7714 §9). Under CM it seals with the
first 8 bytes as AAD and `extra` = `E ‖ index`, then moves the 10-byte tag 4 bytes on and
writes `E ‖ index` before it (RFC 3711 §3.4). `buf` must have `tag_room(p) + 4` spare.
Index `2^31 − 1` seals and the next is `Exhausted`. Nothing wraps.

### SRTP-4: Receiving, ROC estimation and replay

**Receive streams.** A receive stream is keyed by SSRC and holds ROC, `s_l` (highest
authenticated SEQ) and a replay window of 128 indices for RTP, plus a 128-entry window for
the SRTCP index. Openers are stateless (TLS-8), so the session holds one `Rtp` and one
`Rtcp` opener. A packet on an unknown SSRC is authenticated first, with ROC 0 and `s_l` =
its SEQ (RFC 3711 §3.3.1), or with the values `receive_from` set; only a packet that
authenticates creates the stream. When `receive_streams` streams exist, an authenticated
packet on a new SSRC is `Streams` and creates nothing. `end_receive(ssrc)` frees the stream; a
later packet on it starts a new one from ROC 0.

**Index estimate.** For a known stream, `v` is chosen from `{ROC − 1, ROC, ROC + 1}` modulo
`2^32` by RFC 3711 Appendix A, and `i = 2^16 × v + SEQ`. This is the same function the RTP
spec of media-rtp-spec-2318 defines as `rtp.seq.estimate` (RTP-2); once that lands, `srtp`
depends on `rtp` and calls it rather than keeping a second copy, and sending uses it too. A `v` of `ROC − 1` when ROC is 0 is
`TooOld`, since no such packet was ever sent.

**Replay.** Before opening, an index more than 127 below the highest authenticated index is
`TooOld`, and one inside the window already marked is `Replayed`. Both are discards with
`buf` unchanged. After `open` authenticates, the window marks the index, and if `i` is the
new highest, `s_l` becomes SEQ and ROC becomes `v` (RFC 3711 §3.3.2). A packet that fails
authentication changes nothing: neither the window nor ROC moves, so a forged packet with a
far-ahead index cannot push genuine packets out of the window. The window is 128, as in
str0m's `SeqNoWindow` (`src/rtp/seq_no_window.rs`); RFC 3711 asks for at least 64.

**Unprotecting RTP.** `unprotect_rtp(mut s, mut buf)` parses the header as protection does,
checks replay, opens with domain = SSRC and the estimated index (`extra` = the 4-byte `v` for
CM), truncates the tag and answers `Opened`: the plaintext length, the SSRC and the
authenticated 48-bit index. Every stateful RTP operation downstream takes that index, never
one it estimated itself from an unauthenticated packet. A failed open is
`Discarded(Tampered)`, with `buf`'s length unchanged and, per TLS-8, its ciphertext range
zeroed. A packet shorter than its header plus tag is `Malformed`.

**Unprotecting RTCP.** `unprotect_rtcp(mut s, mut buf)` reads `E ‖ index` from the end
(AEAD: the last 4 bytes, after the tag; CM: the 4 bytes before the 10-byte tag, and moves the
tag back over them before `open`), checks the SRTCP window of the sender SSRC, opens with
`extra` = `E ‖ index`, and answers the length of the RTCP compound. `E = 0` is accepted: the
compound was authenticated, not encrypted (RFC 3711 §3.4), and is returned as it is.

### SRTP-5: Revocation and lifetime

Dropping a `Session` drops every sealer and opener it holds; no operation hands one out. A
session dropped mid-stream cannot protect or unprotect another packet. Rekeying is a new
DTLS association and a new session. When any stream reaches `Exhausted` the caller replaces
the session; the library never resets an index.

### SRTP-6: Errors from TLS-8

| below | value | Session outcome |
|---|---|---|
| `schedule.sealer` | `Issued` (SSRC already in the ledger) | `Retired` |
| `schedule.sealer`, `schedule.opener` | `Invalid` (wrong direction) | `Direction` |
| `schedule.*` | `Capacity(c)` | `Capacity(c)`, before issue, nothing changed |
| `packet.seal` | `Reused` | `Reused` |
| `packet.seal` | `Range` | `Exhausted` |
| `packet.seal` | `TooLong` (CM, over 2^20 bytes) | `Malformed` |
| `packet.seal`, `packet.open` | `Capacity(c)` | `Capacity(c)` |
| `packet.open` | `Tampered`, `Range` | `Discarded(Tampered)` |

## Consumer contract coverage

Every in-scope requirement of the Wave 1 consumer contract, in generic form:

| requirement | where | laws |
|---|---|---|
| DTLS 1.2 flights and retransmit | DTLS-5 Flights, Timers, Final flights | `flight_timer_schedule`, `final_flight_resent`, `handshake_survives_loss` |
| SDP certificate fingerprint via `x509_build` | DTLS-7 Identity, Fingerprints, Pinning | `fingerprint_pins_peer`, `interop_openssl` |
| negotiated roles | DTLS-7 Roles | `setup_roles` |
| `use_srtp` exporter | DTLS-5 Server choice, DTLS-8 The exporter | `interop_openssl`, `exporter_once_after_pin` |
| SRTP/SRTCP AES-GCM and AES-CM/HMAC | SRTP-1, SRTP-3, SRTP-4 | `rtp_header_boundary`, `srtcp_trailer_order` |
| independent in and out keys | SRTP-2, DTLS-8 | `directions_independent` |
| SSRC, ROC and the replay window | SRTP-3, SRTP-4 | `roc_rollover`, `replay_window`, `sender_index_monotone`, `streams_after_auth` |
| invalidates DTLS state on revocation | DTLS-8 Revocation, SRTP-5 | structural: no operation returns a sealer or opener; dropping is the fence |

The rest of the contract (RTP forwarding, dynacast, the `revoke` call itself, ICE, rooms)
belongs to media-rtp and media-webrtc-sfu, which consume these two packages.

## Prior art

Goose (`/tmp/ref-goose` at `d5ae5bb`) and dimpl (`/tmp/ref-dimpl` at `e341a90`) for DTLS,
with str0m (`/tmp/ref-str0m` at `2ada76a`) for SRTP.

| item | source | verdict |
|---|---|---|
| Any DTLS, SRTP, UDP or crypto surface | Goose: its standard library is `base64`, `binary`, `csv`, `dictionary`, `exception`, `ffi`, `json`, `math`, `os`, `regex`, `slice_pool`, `slot_pool`, `std` and `vec` (`stdlib/`); it has no network or cryptography modules | NOT TAKEN: nothing to take |
| Fixed-size inline state with no heap per packet | Goose's no-heap design (`README.md`) | NOT TAKEN as a source: the zero-allocation target here comes from PLAN, and the replay windows are inline `u128` bitmaps for their own reason |
| Sans-IO `handle_packet` / `poll_output` / `handle_timeout` | dimpl `src/lib.rs:58` | ADOPTED as `receive` / `poll` (timers run inside `poll`) |
| Every error fatal, noise discarded | dimpl `src/lib.rs:80` | ADOPTED, with counted discards (DTLS-6) where dimpl only logs |
| Application validates the peer certificate | dimpl `src/lib.rs:50` | ADAPTED: the library pins RFC 8122 fingerprints itself, so no caller can forget |
| DTLS 1.3 and version auto-sense | dimpl `src/lib.rs:27` | NOT TAKEN for Wave 1 (DTLS 1.3 is out of Wave 1) |
| Stateless cookie `HMAC(secret, client_random)` | dimpl `src/dtls12/server.rs:378`, `:1217` | ADAPTED: the address is bound too, and an ICE-verified address skips it |
| Flight timer 1 s, 4 retries, 40 s handshake, 50% jitter | dimpl `src/config.rs:93-95`, `src/timer.rs` | ADAPTED: RFC 6347's 1 s doubling to 60 s, 7 retries, 60 s deadline, no jitter, so sim time is exact |
| MTU 1,150, 30 queued records | dimpl `src/config.rs:88-89` | ADAPTED: MTU 1,200 and 32 held fragments, both explicit `Config` |
| 64-record replay window per epoch, updated after decryption | dimpl `src/window.rs` | ADOPTED |
| Extended Master Secret enforced | dimpl `src/lib.rs:48` | ADOPTED |
| PSK and CCM_8 suites, P-384 key exchange | dimpl `src/lib.rs:36-44` | NOT TAKEN: TLS-8 offers X25519 and P-256 only, and WebRTC peers use certificates |
| Pluggable crypto provider traits | dimpl `src/crypto/provider.rs` | REPLACED by TLS-8 schedules, which never let a key leave |
| 128-entry SRTP replay window, check before and update after authentication | str0m `src/rtp/seq_no_window.rs` | ADOPTED |
| `index >> 16` as ROC | str0m `src/rtp/srtp.rs:314` | ADOPTED (RFC 3711 §3.3.1) |
| SRTCP index wraps modulo `2^31` | str0m `src/rtp/srtp.rs:471` | REPLACED: wrapping reuses a nonce under the same key; SRTP-3 refuses with `Exhausted` |
| `protect_rtcp` returns a new `Vec` | str0m `src/rtp/srtp.rs:464` | REPLACED: in place in the caller's spare, zero allocations |

## Build laws

Each law is written red first in its named harness. Its oracle is outside the code under
test. Laws that run a handshake in-process use a virtual `Random` per endpoint with distinct
seeds (DTLS-8). `std.sim.udp` (Wave 0.3) has not landed, so the loss law drives a seeded
in-process pair. It switches to `std.sim.udp` once that lands. The DTLS harness is
`core/programs/dtls/laws.hirsel`<!-- doc-ref: planned --> and the SRTP harness is
`core/programs/srtp/laws.hirsel`<!-- doc-ref: planned -->. The OpenSSL interop runs from
`core/programs/dtls/interop.py`<!-- doc-ref: planned --> on loopback.

### DTLS

- **`interop_openssl`.** *Scenario:* a Hirsel client against
  `openssl s_server -dtls1_2 -use_srtp <profile> -keymatexport EXTRACTOR-dtls_srtp -keymatexportlen <n>`
  with a P-256 certificate and `-verify 1`, and a Hirsel server against
  `openssl s_client -dtls1_2` with the same options. Run each of the three suites at least
  once and each of the three SRTP profiles at least once, and the Hirsel server once in
  `Cookie` mode (OpenSSL's client answers a HelloVerifyRequest by default). *Oracle:* OpenSSL's wire output and
  printed keying material. The handshake completes on both sides. Python `cryptography`
  seals one RTP packet under OpenSSL's exported bytes, cut as TLS-8 `srtp` cuts them, and
  gets the bytes Hirsel's `protect_rtp` makes, and the reverse opens. The peer certificate
  Hirsel reports has the SHA-256 that `openssl x509 -fingerprint -sha256` prints for
  OpenSSL's certificate.
- **`interop_dimpl`.** *Scenario:* a Hirsel client against a dimpl 1.2 server and a Hirsel
  server against a dimpl 1.2 client, each for one GCM suite and one GCM SRTP profile, through
  a Rust driver that exchanges datagrams in memory. *Oracle:* dimpl's wire output and its
  exported keying material, which the driver prints. Both complete, and the SRTP keys cut from
  dimpl's exporter output equal the Hirsel session's, checked by one packet sealed on each
  side and opened on the other.
- **`handshake_survives_loss`.** *Scenario:* client and server in process, each datagram
  independently dropped with probability 0.2, duplicated with 0.05 and delayed by up to
  three datagrams, from a seeded stream; 32 seeds, under 15 s. Then the same 32 seeds with
  duplication and delay but no loss. *Oracle:* an invariant per seed, by hand. Every seed
  either reaches `Connected` on both sides with a cross-check (the client's SRTP packet opens
  at the server and the reverse), or both sides close as `Timeout` at the instant the timer
  schedule gives; no other end is allowed. With no loss, every seed connects. The number of
  lossy seeds that connect is reported and gates nothing, since no hand bound on it is
  tight.
- **`flight_timer_schedule`.** *Scenario:* a client whose every datagram is dropped, three
  times: at defaults; with `handshake_ns` raised to 300 s; and with `retries` 2. Then a client
  whose flight 1 is answered without loss and whose flight 3 needs one retransmission. *Oracle:*
  RFC 6347 §4.2.4.1 by hand, each `WakeAt` exact. At defaults the ClientHello is sent at 0 and
  resent at 1, 3, 7, 15 and 31 s, and the deadline closes it as `Timeout` at 60 s, before the
  next resend at 63 s. With 300 s it is resent at 1, 3, 7, 15, 31, 63 and 123 s (the timer
  capped at 60 s for the last), and the eighth expiry closes it as `Timeout` at 183 s. With
  `retries` 2 it is resent at 1 and 3 s and closes at 7 s. In the last case flight 1 is
  answered at 0.5 s, so flight 3 starts at 0.5 s with a 1 s timer; it is resent at 1.5 s and
  answered at 2 s, so flight 5 starts at 2 s with the doubled 2 s timer (`WakeAt` 4 s).
- **`final_flight_resent`.** *Scenario:* the server's flight 6 is dropped. The client
  retransmits flight 5 twice. *Oracle:* RFC 6347 §4.2.4 by hand. The server is `Connected`
  after flight 5. Each retransmitted flight 5 gets exactly one flight 6, with record numbers
  above the previous ones, and the client then connects. A retransmitted flight 4 reaching a
  connected client produces nothing.
- **`fragments_reassemble`.** *Scenario:* a 3,000-byte Certificate split into fragments
  delivered in reverse order with one overlap and one duplicate; a ClientHello split in two;
  `openssl s_server -mtu 300` live. *Oracle:* the source messages and OpenSSL. The
  reassembled message equals the original bytes, the transcript enters it once with the
  unfragmented header (RFC 6347 §4.2.6), and the handshake completes. Fragments with
  conflicting `length`, or a range past `length`, fail with `illegal_parameter`.
- **`offered_13_negotiates_12`.** *Scenario:* a hand-built ClientHello with
  `supported_versions` listing `{254, 252}` and `{254, 253}` and a `key_share` extension, plus
  this text's other extensions. *Oracle:* RFC 9147 §5.3 and RFC 6347 by hand. The server
  answers a ServerHello of `{254, 253}` with no `supported_versions` and no `key_share`.
- **`fingerprint_pins_peer`.** *Scenario:* `expect` with a fingerprint that differs in one
  byte; `expect` called only after the handshake completes; two fingerprints with the
  second matching. *Oracle:* RFC 8122 §6.2 by hand. In the first case the endpoint sends
  alert 42 and closes as `SentAlert(42)` without `Connected`. In the second it stays
  `Handshaking` and delivers no data until `expect`, then connects. In the third it
  connects. `fingerprint_text` of a fixed certificate equals the
  `openssl x509 -noout -fingerprint -sha256` output after its `=`, and parses back.
- **`setup_roles`.** *Oracle:* the DTLS-7 table from RFC 5763 §5 by hand, all 9 pairs of
  `Setup`.
- **`refusals`.** *Scenario:* hand-built peer messages: a ClientHello without EMS; one with a
  non-empty `srtp_mki`; one sharing no SRTP profile while `Config.srtp` is non-empty; a
  ServerHello choosing a suite the client did not offer; a ServerKeyExchange with a broken
  signature. *Oracle:* the DTLS-5 failure table by hand: alerts 40, 47, 40, 47 and 51.
- **`record_replay_and_discards`.** *Scenario:* after connecting, an application record
  delivered twice, one 70 records behind the newest, one with a flipped tag bit, and one in
  epoch 2. *Oracle:* RFC 6347 §§4.1.2.6, 4.1.2.7 by hand. The four are discarded and counted
  as `replayed`, `replayed`, `tampered` and `stale_epoch`, no alert is sent, and the next
  genuine record is delivered.
- **`cookie_bounds_unproven_address`.** *Scenario:* a `Cookie` server receives 10
  ClientHellos without cookies, one with a cookie computed for another address, and one with
  the correct cookie. *Oracle:* RFC 6347 §4.2.1 by hand. Each of the first 11 gets one
  60-byte HelloVerifyRequest and no other datagram, with the record sequence number of the
  ClientHello it answers, and the server holds no state beyond the endpoint. The twelfth gets
  flight 4, whose first record has that ClientHello's record sequence number and whose
  ServerHello has `message_seq` 1.
- **`exporter_once_after_pin`.** *Scenario:* `take_srtp` before `Connected`, after
  `Connected`, and again; an endpoint that negotiated no profile. *Oracle:* DTLS-8 by hand:
  `NotConnected`, a schedule, `Taken`, `NoSrtp`.

### SRTP

- **`rtp_header_boundary`.** *Scenario:* RTP packets with CC 0, 1 and 15, with no extension,
  a one-byte-header extension (`0xBEDE`) and a two-byte-header one (`0x100`), and with the
  padding bit; then a header extension whose length runs past the packet, and version 1.
  *Oracle:* Python `cryptography` over session keys derived from libSRTP `srtp_validate_gcm`'s
  and `srtp_validate`'s master keys (TLS-8 gives the derived values), with the whole header as
  AAD and the hand-computed index. Each protected packet equals Python's and unprotects
  back. The last two are `Malformed` with `buf` unchanged.
- **`roc_rollover`.** *Scenario:* a sender sends SEQ 65,534, 65,535, 0 and 1. A receiver gets
  them in the order 65,535, 1, 65,534, 0. A third receiver is given ROC 5 by `receive_from`.
  *Oracle:* RFC 3711 §3.3.1 and Appendix A by hand. The sender's indices are 65,534, 65,535,
  65,536 and 65,537, and its CM packets carry ROC 0, 0, 1 and 1 in the authentication. The
  receiver opens all four, and ends with ROC 1 and `s_l` 1. The third receiver opens a packet
  sent at index `5 × 2^16 + 7`.
- **`replay_window`.** *Scenario:* after indices 0 to 200 except 73, a repeat of 150, then 73,
  then 72, then a forged packet at index 10,000 with a bad tag, then 201. *Oracle:* SRTP-4 by
  hand. 150 is `Replayed`. 73 is 127 below and unseen in the window, so it opens. 72 is
  `TooOld`. The forgery is `Tampered` and moves nothing, so 201 opens.
- **`srtcp_trailer_order`.** *Scenario:* `protect_rtcp` of libSRTP `rtcp_plaintext_ref`
  under both validation keys; an `E = 0` compound; index 2^31 − 1, then one more.
  *Oracle:* libSRTP `test/srtp_driver.c` at `fd08747f` for the protected bytes (the tag
  before `E ‖ index` under AEAD, after it under CM), RFC 3711 §3.4 for `E = 0`, and SRTP-3 for
  the last: it seals, and the next is `Exhausted`.
- **`directions_independent`.** *Scenario:* the two sessions of one in-process DTLS
  association. *Oracle:* TLS-8's direction rule by hand. A's protected packet unprotects at
  B. A's own `unprotect_rtp` of it is `Discarded(Tampered)`, because its opener holds B's key.
  `session_pair` with two `send` imports is `Direction`.
- **`sender_index_monotone`.** *Scenario:* send SEQ 10, then 9, then 11; on a stream set by
  `send_from` to ROC `2^32 − 1`, send SEQ 65,535 (index `2^48 − 1`), then SEQ 0. *Oracle:*
  SRTP-3 by hand. 9 is `Reused` with `buf` unchanged, and 11 then seals. Index `2^48 − 1`
  seals, and SEQ 0, whose index would be `2^48`, is `Exhausted`.
- **`streams_after_auth`.** *Scenario:* `receive_streams` 2. Forged packets on 5 new SSRCs,
  then genuine packets on 3 new SSRCs. Then `end_send` of an SSRC and a packet on it.
  *Oracle:* SRTP-3 and SRTP-4 by hand. The forgeries create no stream. The first two genuine
  SSRCs open, and the third is `Streams`. The ended SSRC is `Retired`.
- **`fan_out_into`.** *Scenario:* one 1,000-byte payload through `protect_rtp_into` to three
  sessions with three different headers. *Oracle:* Python per session. Each output equals
  Python's, and `src` is unchanged.

## Cost targets

From PLAN rows 1.2 and 1.3, and nothing else:

- **SRTP: 0 allocations per packet.** `protect_rtp`, `protect_rtp_into`, `unprotect_rtp`,
  `protect_rtcp` and `unprotect_rtcp` on an established stream allocate nothing. The
  measurement is a counted allocator over 10^6 packets of 1,200 bytes per operation and
  profile, after setup. Stream creation and session construction are outside it.
- **SRTP AES-128-GCM kernel within TLS-8's bound.** TLS-8's ≤30 ns over ring's fused call per
  1,200-byte packet, which media-w0-crypto-build-2282 met (seal 25.94 ns, open 27.79 ns). The
  SRTP lane reports instructions per packet for `protect_rtp` and `unprotect_rtp` beside the
  kernel's, as the library's overhead on top. PLAN sets no number for that overhead, so it is
  reported and gates nothing.
- **DTLS: the handshake under 20% loss and reordering** (`handshake_survives_loss`). PLAN
  sets no time or throughput target for DTLS.

## Build slices

| slice | size | depends on | content | laws |
|---|---|---|---|---|
| **S0** expose TLS-8 DTLS and SRTP schedule operations | M | — | `schedule.dtls12`, `schedule.srtp`, `schedule.finished`, `schedule.verify_finished`, `schedule.import_srtp` and `schedule.SrtpProfile`, `schedule.Suite12` in `core/core/schedule.hirsel`, over the TLS-8 T14 kernels | TLS-8 PC10 and PC2. The [source laws](../../core/std/tests/secret_agree.hirsel) check DTLS 1.2 records and Finished, SRTP import direction and libSRTP packets, and both sides of the DTLS-SRTP exporter on the evaluator, Cranelift and LLVM. |
| **S1** `std.x509_key` | S | — | SPKI reader for P-256 and P-384 points (DTLS-7) | one case in the std x509 laws: OpenSSL-generated P-256 and P-384 certificates give the points `openssl ec -pubout` prints; RSA and trailing bytes give `Invalid` |
| **S2** DTLS records | M | S0 | `core/programs/dtls/`<!-- doc-ref: planned --> package, record framing, epochs, replay window, alerts, fragmentation and reassembly, the transcript (DTLS-4, DTLS-5 Fragmentation and Transcript, DTLS-6) | `record_replay_and_discards`, `fragments_reassemble` (hand-built parts) |
| **S3** DTLS handshake | L | S1, S2 | client and server flights, cookie admission, server choice, EMS, `use_srtp`, failures, timers and final flights, `take_srtp`, error mapping (DTLS-3, DTLS-5, DTLS-8) | `flight_timer_schedule`, `final_flight_resent`, `offered_13_negotiates_12`, `refusals`, `cookie_bounds_unproven_address`, `exporter_once_after_pin`, `handshake_survives_loss` |
| **S4** identity, fingerprints, roles | S | S1 | DTLS-7 | `fingerprint_pins_peer`, `setup_roles` |
| **S5** SRTP sessions | M | S0; `rtp.seq.estimate` once rtp.md's RTP-2 lands (until then S5 keeps the Appendix A estimate and switches) | `core/programs/srtp/`<!-- doc-ref: planned --> package (SRTP-2 to SRTP-6) | every SRTP law |
| **S6** interop | M | S3, S4, S5 | `interop.py` against OpenSSL on loopback, and against dimpl at `e341a90` through a small Rust driver built from a clone in `/tmp`, never vendored | `interop_openssl` and `interop_dimpl`, the live part of `fragments_reassemble` |
| **S7** SRTP cost | S | S5 | counted-allocator and instruction-count run (cost targets) | the cost targets |

S1, S2 and S5 run in parallel after S0. S0 blocks everything else. A Sol lane can take
S0, S1, S2, S4, S5 and S7 from this text. S3 is the security-bearing slice: PLAN row 1.2
asks for an Astra security attack on it (the handshake state machine, cookie admission,
fingerprint pinning, the exporter) before it lands. S6 closes the build.

**Deviation from PLAN row 1.2's verification.** PLAN asks for interop with Chromium, Firefox,
str0m and dimpl under `sim.udp` at 20% loss. This text covers OpenSSL and dimpl at wire level
(S6), and loss in process (`handshake_survives_loss`), because `std.sim.udp` has not landed
and a browser needs the SDP and ICE glue of PLAN row 1.6. Browser interop therefore moves to
row 1.6's headless Chromium end-to-end test, which drives this endpoint; str0m uses dimpl for
DTLS, so the dimpl law covers it. The loss law moves to `std.sim.udp` when that lands.
