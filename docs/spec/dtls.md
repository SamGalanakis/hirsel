# DTLS and SRTP

Version note (2026-11-24, dtls-s3-handshake-2371, from attack 2434): a ChangeCipherSpec
that arrives before the flight preceding it is complete is now discarded as `overflow`
instead of refused with 10, as OpenSSL does (`ssl/record/rec_layer_d1.c` 649-657), and
`possession_before_keys` (a) ends in `Timeout`. Plain reordering is normal on UDP and the
peer's retransmission brings the ChangeCipherSpec again; a CertificateVerify can still never
be skipped silently, because keys are derived only after it verifies. Once the peer's
ChangeCipherSpec has arrived, every other epoch-0 record is `stale_epoch` (DTLS-4 Replay),
and a first ClientHello with `message_seq` above 1 is not admitted (DTLS-3).

Version note (2026-11-24, media-dtls-spec-2317): first text of DTLS-1 to DTLS-9 and SRTP-1
to SRTP-6, the Wave 1 rows 1.2 and 1.3 of the real-time media track. Not yet built: every
fence is `unbuilt`, and every test path is planned. The sections end with
[build laws](#build-laws), [cost targets](#cost-targets) and [build slices](#build-slices).

Amended 2026-11-24 (dtls-spec-amend-2341) for the soundness review dtls-spec-review-2331:
the required message order and proof of possession (DTLS-5 Message order), stateless
cookie admission through `admit` (DTLS-5 Admission), no epoch-0 replay window (DTLS-4),
flight 6 resent only for an authenticated Finished, kept replay floors after `end_receive`
(SRTP-4), the unreduced sender estimate (SRTP-3), and the version fields (DTLS-1, DTLS-4).

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
| version | DTLS 1.2 (`{254, 253}`) only. A ClientHello body's `client_version` is `{254, 253}` or newer (DTLS-5 Versions); `{254, 255}` appears only in record headers sent before the version is fixed (DTLS-4 Framing) and in HelloVerifyRequest's `server_version` | RFC 6347 §4.1, §4.2.1 |
| cipher suites | `TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256` (0xC02B), `TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384` (0xC02C), `TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256` (0xCCA9): TLS-8's `Suite12` | RFC 5289, RFC 7905 |
| groups | `x25519` (0x001D), `secp256r1` (0x0017): TLS-8's `Group` | RFC 8422 |
| own signature | `ecdsa_secp256r1_sha256` (0x0403), with a P-256 key from `secret.p256_key` | RFC 8422 §5.4 |
| peer signature | 0x0403 over a P-256 key, `ecdsa_secp384r1_sha384` (0x0503) over a P-384 key, both strict DER (`p256.verify_der`, `p384.verify_der`) | RFC 8422 |
| extensions | `supported_groups` (10), `ec_point_formats` (11, uncompressed only), `signature_algorithms` (13), `use_srtp` (14), `extended_master_secret` (23), `renegotiation_info` (0xFF01, empty only) | RFC 5764 §9, RFC 7627 §5.1, RFC 5746 |
| Extended Master Secret | required: a peer that does not negotiate it fails the handshake | RFC 7627 §5.2 |
| client certificate | always requested by the server and always sent by the client | RFC 8122 §6.2 |
| HelloVerifyRequest cookie | stateless, by `admit` before any `Endpoint` exists, or skipped for an ICE-verified address (DTLS-5 Admission) | RFC 6347 §4.2.1 |
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
pub type Admit = Reply(u64) | Admitted | Discard
pub type Discards { malformed: u64, replayed: u64, tampered: u64, stale_epoch: u64, overflow: u64 }

pub type DtlsError = Config | Setup | Fingerprint | Closed(Close) | NotConnected
                   | Taken | NoSrtp | Key(KeyError) | Schedule(schedule.ScheduleError)
                   | Packet(packet.PacketError) | Capacity(Capacity)

pub fn defaults() -> Config
pub fn identity(take key: Secret, take certificate: Bytes, r: Random) -> Result[Identity, DtlsError]
pub fn share_identity(i: Identity) -> Identity
pub fn fingerprint(i: Identity, h: HashFunc) -> Fingerprint
pub fn fingerprint_text(f: Fingerprint, mut out: Bytes) -> u64
pub fn parse_fingerprint(text: Bytes) -> Result[Fingerprint, DtlsError]
pub fn negotiate_setup(offer: Setup, answer: Setup) -> Result[schedule.Side, DtlsError]

pub fn admit(key: Inline[u8, 32], peer: Addr, datagram: Bytes, mut out: Bytes) -> Admit
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

Application data is never delivered before `Connected`. An epoch-1 application-data record
that authenticates while the endpoint is still `Handshaking` (the peer finished first, or
`expect` has not been called) is held with the early records of DTLS-4 Epochs, within
`max_ahead`, and delivered only once the state is `Connected`; past the bound it is
`overflow`. An epoch-0 application-data record is `malformed`.

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
record whose length runs past the datagram, whose version is not `{254, 253}`, or whose
type is other is discarded as `malformed`, with the rest of the datagram discarded too,
since its boundaries are lost. One exception: an epoch-0 handshake record received before
the version is fixed may carry `{254, 255}`. For a server that is every ClientHello
record; for a client, every record before the ServerHello is processed. OpenSSL stamps
`{254, 255}` on all records it writes under its version-flexible `DTLS_*_method` until a
version is chosen (`ssl/record/rec_layer_d1.c:929-931`, and `ssl/d1_lib.c:1111` for
`DTLSv1_listen`), and `openssl s_server -dtls1_2` and `s_client -dtls1_2` use that method
(`apps/s_server.c:2245`), so its ClientHellos and HelloVerifyRequests carry it. Hirsel
writes `{254, 255}` on the HelloVerifyRequest record and `{254, 253}` on every other
record.

**Epochs.** Epoch 0 carries the plaintext handshake. Epoch 1 is protected by the `dtls12`
schedule's `Application` space: one sealer, issued with domain 0 and floor 0, and one opener.
The endpoint seals with TLS-8's DTLS 1.2 rows: for AES-GCM it writes the record number
`epoch ‖ seq` as the 8-byte explicit nonce, and `extra` is the 13 bytes
`epoch ‖ seq ‖ type ‖ version ‖ plaintext length` (RFC 6347 §4.1.2.1, RFC 5246 §6.2.3.3).
Record sequence numbers start at 0 in each epoch and increase by one per record sent, every
retransmission included (RFC 6347 §4.2.4), so the sealer's counter is always increasing. The
one exception is a `Cookie` server's HelloVerifyRequest and first ServerHello (DTLS-5
Admission), which are epoch 0 and never sealed. A
record in epoch 1 that arrives before the endpoint has epoch-1 keys or before the peer's
ChangeCipherSpec (a Finished overtaking the ChangeCipherSpec) is held, at most `max_ahead`
records counted with handshake fragments, and processed once the keys exist and that
ChangeCipherSpec has arrived; past that it is discarded as `overflow` and
retransmission recovers it. An epoch-1 record longer than a sealed 16,384-byte plaintext
(16,384 + 8 + 16 = 16,408 bytes) is `tampered` and never held, so held records stay
bounded by `max_ahead` times that size. A record in any other epoch is `stale_epoch`.

**Replay.** Epoch 1 has a 64-record sliding window (RFC 6347 §4.1.2.6), as in dimpl's
`ReplayWindow` (`src/window.rs`). A record whose number is in the window and already
marked, or below it, is discarded as `replayed` before decryption. The window is updated only
after the record authenticates. **Epoch 0 keeps no replay window.** Its records cannot
authenticate, so a window there would let one forged record numbered `2^48 − 1` push every
later genuine epoch-0 record below the window and stall the handshake until `Timeout`.
Instead an unauthenticated record number changes no state at all: handshake messages are
deduplicated by `message_seq` and fragment range (DTLS-5 Fragmentation), a repeated
ChangeCipherSpec is a no-op, and alerts follow Alerts below. Once the peer's
ChangeCipherSpec has arrived, every epoch-0 record except a repeated ChangeCipherSpec is
discarded as `stale_epoch`: nothing genuine can follow it in epoch 0, and acting on a forged
one would let anyone on the path close the handshake with 10, or make the endpoint seal one
alert for each 12-byte fragment of a datagram. After `Connected`, every
epoch-0 record is discarded as `stale_epoch`; the only retransmission an endpoint answers
then is the authenticated one in DTLS-5 Final flights.

**Invalid records.** A record that fails authentication is discarded as `tampered` and
sends no alert (RFC 6347 §4.1.2.7). The association continues.

**Alerts.** A received fatal alert (level 2), or a received `close_notify` (0), closes the
endpoint as `PeerAlert(n)` or `PeerClosed`. A warning alert other than `close_notify` is
ignored. This holds for a plaintext epoch-0 alert too, until the peer's ChangeCipherSpec
has arrived: anyone on the path can close a handshake that way, as in every DTLS 1.2
implementation, and the endpoint gains nothing by ignoring it, because the same attacker
can drop every datagram. Once the peer's ChangeCipherSpec has arrived, an epoch-0 alert is
discarded as `stale_epoch` and only epoch-1 alerts act. When the endpoint fails a handshake it queues one fatal alert record with the
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

**Message order.** Every message in the table is required, and each side accepts the
peer's messages only in this order (RFC 5246 §7.3, §7.4.8, §7.4.9; RFC 6347 §4.2.4):

- **Server receives:** ClientHello (epoch 0); then Certificate, ClientKeyExchange,
  CertificateVerify, ChangeCipherSpec (all epoch 0); then Finished in epoch 1.
- **Client receives:** HelloVerifyRequest (only before any ServerHello, and only in answer
  to a ClientHello), then ServerHello, Certificate, ServerKeyExchange, CertificateRequest,
  ServerHelloDone (all epoch 0); then ChangeCipherSpec (epoch 0); then Finished in epoch 1.

A handshake message whose type is not the next one this list expects, or a Finished in epoch
0, is `unexpected_message` (10). A Finished in epoch 1 that arrives before the peer's
ChangeCipherSpec is not refused: DTLS-4 Records holds it until that ChangeCipherSpec. A
ChangeCipherSpec that arrives before the messages this list puts ahead of it is not refused
either: it is discarded as `overflow` and the peer's retransmission brings it again, since
plain reordering on UDP delivers it early (OpenSSL `ssl/record/rec_layer_d1.c` 649-657).
This cannot skip CertificateVerify: the server derives no keys before one verifies, so a
flight 5 without it never connects and ends in `Timeout`. In
particular a non-empty client Certificate followed by anything other than
ClientKeyExchange and then CertificateVerify is 10: CertificateVerify is the only proof
that the peer holds the certificate's key, since a certificate and its fingerprint are
public. An empty client Certificate (RFC 5246 §7.4.6 lets a client send one) is
`bad_certificate` (42). A server flight 4 without ServerKeyExchange is 10. No message is
optional, so the state machine holds no "skipped" branch. Retransmitted copies of messages
already processed are recognised by `message_seq` before the order check and never fail it.

**ClientHello.** The client sends `client_version` `{254, 253}`, a fresh 32-byte random
drawn from `r`, an empty `session_id`, an empty cookie at first, `Config.suites` in order,
null compression, and the extensions of DTLS-1. `use_srtp` lists `Config.srtp` in order with
an empty MKI and is left out when `Config.srtp` is empty.

**Versions.** The server reads a ClientHello's body `client_version`. `{254, 253}`, or a
newer DTLS version (major 254, minor below 253), is answered as DTLS 1.2 (RFC 5246
Appendix E.1: the server answers with the highest version both support). `{254, 255}` is a
DTLS 1.0 client, and so is `{254, 254}` or any other value: each is `protocol_version` (70),
since the only version both could share is unsupported here, as dimpl refuses it
(`src/dtls12/server.rs:355`). The HelloVerifyRequest's `server_version` is `{254, 255}`
(RFC 6347 §4.2.1 SHOULD; OpenSSL writes `DTLS1_VERSION` at `ssl/statem/statem_srvr.c:1896`,
dimpl at `src/dtls12/server.rs:400`); a client accepts `{254, 255}` or `{254, 253}` there,
as dimpl does (`src/dtls12/client.rs:422`). A ServerHello's `server_version` other than
`{254, 253}` is 70.

**Server choice.** The server picks the first entry of its own `suites` the client offered,
the first of its own `groups` the client offered, and the first of its own `srtp` profiles
the client offered. It fails with `handshake_failure` (40) when no suite or group is shared,
when the client does not offer `extended_master_secret`, when the client's
`signature_algorithms` lacks 0x0403 (the server's only signature), or when `Config.srtp` is
non-empty and no profile is shared. It fails with `illegal_parameter` (47) when the client's `srtp_mki`
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
are ones it offered; `extended_master_secret` is present; `use_srtp` is present exactly when
the client offered it; it fails with `illegal_parameter` otherwise, or `handshake_failure`
for a missing EMS or a missing `use_srtp` (RFC 5764 §4.1.1 lets a server decline it, and
this client then has no keys for media); a CertificateRequest whose algorithms lack 0x0403
is also `handshake_failure`), takes the public key from the server's
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
(RFC 6347 §4.2.3) that each fit, with the record header and, in epoch 1, the 16-byte tag
counted, and under the AES-GCM suites the 8-byte explicit nonce too (TLS-8's DTLS 1.2
rows; ChaCha20-Poly1305 has none). Several whole records go into one datagram while they fit
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
`handshake_ns` after the first flight is sent (client), or after the first `receive` that
keeps any part of a ClientHello (server), an endpoint still `Handshaking` closes as
`Timeout`. A server that holds only fragments answers `poll` with `WakeAt` for that
deadline, never `Idle`. There is no jitter: every
time is a pure function of the inputs, so sim traces are exact.

**Final flights.** The server sends the last flight (6). It keeps flight 6 after
`Connected` until the endpoint is dropped or closed, and resends it once for each
retransmitted client Finished that authenticates in epoch 1 under a record number it has not
seen (§4.2.4). That is the only trigger. A retransmitted flight 5 spanning several records
or datagrams carries one Finished, so it gets exactly one flight 6. Its epoch-0 part
(Certificate, ClientKeyExchange, CertificateVerify, ChangeCipherSpec) is `stale_epoch`
after `Connected` (DTLS-4 Replay) and triggers nothing, so no unauthenticated datagram
makes the server seal or send. A replayed Finished record is `replayed` by the epoch-1
window. The client, once it has flight 6, discards a retransmitted flight 4 as
`stale_epoch`. A server that receives a retransmitted flight 3 while
waiting for flight 5 resends flight 4. Every other retransmitted message is ignored.

**Admission (cookies).** `server(.., Verified)` skips flights 2 and 3: the caller attests the
peer address is proven, for example by an ICE connectivity check that carried
`MESSAGE-INTEGRITY` under the peer's credentials ([RFC 8445](https://www.rfc-editor.org/rfc/rfc8445)).
For an unproven address the caller runs **`admit(key, peer, datagram, mut out)` before it
builds anything**. `admit` is a pure function: it takes no `Endpoint`, no `Identity` and no
`Random`, and it keeps nothing, so a sender spoofing many source addresses makes the
caller allocate nothing per address. It reads only the first record of `datagram`:

- `Reply(n)`: the record is an epoch-0 handshake record holding a ClientHello fragment with
  `fragment_offset` 0 that reaches at least through the cookie field, and the cookie is
  empty or wrong. `admit` appends one HelloVerifyRequest datagram of `n` = 60 bytes to `out`
  and the caller sends it to `peer`.
- `Admitted`: the same, and the cookie is right. The caller now builds
  `server(take id, c, peer, Cookie(key))` and passes it the same datagram through `receive`.
- `Discard`: anything else (not a ClientHello, a later fragment, a first fragment cut before
  the cookie, malformed framing, an `out` without 60 bytes spare). Nothing is sent.

The cookie is `HMAC-SHA256(key, address bytes ‖ client_random)` (32 bytes,
`std.hmac.sha256`), where the address bytes are the peer `Addr`'s family byte, address and
big-endian port; it is right when it equals that value in constant time
(`std.hmac.equal`). A `Cookie` server checks the cookie again on the first ClientHello it
receives: one that is not right is discarded as `malformed` and gets no reply, so a caller
that skips `admit` never sends flight 4 to an unproven address.

**A fragmented ClientHello.** The cookie field follows only the version, random and
`session_id`, before the suites and extensions, so `admit` judges the first fragment alone, as
OpenSSL's `DTLSv1_listen` does (`ssl/d1_lib.c:1015-1026`: "we only look at the first
ClientHello fragment and require that the cookie must be contained within it"). The later
fragments of a cookieless ClientHello are `Discard`; the client resends the whole message
with the cookie. Once `admit` answers `Admitted`, the server endpoint exists and
reassembles the rest of that ClientHello under DTLS-5 Fragmentation, so a large
DTLS 1.3-style ClientHello still connects under `Cookie` (law
`cookie_bounds_unproven_address`). The
review asked for `Discard` of every ClientHello that does not fit one datagram; that would
refuse such peers under `Cookie` for no gain, since the first fragment already carries all
the cookie binds.

**No amplification.** The smallest datagram that earns a reply is 13 (record header) + 12
(handshake header) + 2 (`client_version`) + 32 (random) + 1 (`session_id` length, 0) + 1
(cookie length, 0) = 61 bytes. The reply is 13 + 12 + 2 (`server_version`) + 1 (cookie
length) + 32 = 60 bytes, and `admit` answers at most one per datagram. The
HelloVerifyRequest takes its record sequence number from the ClientHello record it answers
(RFC 6347 §4.2.1), since `admit` keeps no counter, and its `message_seq` is 0. The
ServerHello and the rest of flight 4 take the cookie ClientHello's record sequence number for
their first record and continue from its `message_seq` (1). After that the
server's record numbers continue from the copied one, and its message numbers from 1. A
ClientHello whose `message_seq` is above 1 is `Discard`, as in OpenSSL
(`ssl/d1_lib.c:1009-1013`): a first ClientHello is 0 and its cookie copy 1, and admitting
`0xFFFF` would push flight 4's numbering past 16 bits.

**Why stateless, against OpenSSL, pion and webrtc-rs.** OpenSSL's `DTLSv1_listen` is
stateless in the same way and is the model here. pion's listener creates a connection per
source address before any cookie (`listener.go:17` accepts any handshake record, and the
cookie is per-connection random state, `internal/flight/flight12/flight0handler.go:211`), as
does webrtc-rs (`rtc-dtls/src/endpoint.rs:227` inserts a connection per `SocketAddr` on
first read). Both let a spoofing sender make the server allocate per address, which is what
RFC 6347 §4.2.1's cookie exists to prevent; the clean end state is the stateless gate.

**Deviations in the cookie.** dimpl binds the cookie to the client random only
(`src/dtls12/server.rs:1217`); binding the address too means a cookie seen on one path cannot
admit a ClientHello from another. RFC 6347 §4.2.1 says the cookie SHOULD also cover the
version, `session_id`, cipher suites and compression methods; this one does not, and it
never expires. Both are harmless here: the transcript starts at the cookie ClientHello, so a
client that changes those fields after the cookie only negotiates on its own second
ClientHello, and the address binding is what bounds reuse. Rotating the key (and so
expiring cookies) is the caller's: it passes a new key to `admit` and `server`.

**Failures.** Each handshake failure sends one fatal alert and closes as `SentAlert(n)`:

| failure | alert |
|---|---|
| no shared suite or group, no EMS, no shared SRTP profile, `use_srtp` offered but not answered, no 0x0403 in the peer's `signature_algorithms` or CertificateRequest, transcript too large | `handshake_failure` (40) |
| malformed or contradictory message, non-empty MKI, invalid peer point, fragment inconsistency, an answered extension that was not offered | `illegal_parameter` (47) |
| a message out of the order in Message order, including a skipped CertificateVerify or ServerKeyExchange and a Finished in epoch 0 | `unexpected_message` (10) |
| peer certificate missing, empty or not P-256/P-384 ECDSA, or fingerprint mismatch | `bad_certificate` (42) |
| a signature or Finished that does not verify, including a CertificateVerify by a key other than the certificate's | `decrypt_error` (51) |
| a ClientHello `client_version` older than DTLS 1.2, or a ServerHello version other than DTLS 1.2 | `protocol_version` (70) |

### DTLS-6: Discards

Every datagram or record the endpoint drops without failing is counted in `discards(e)`:
`malformed` (framing), `replayed` (window), `tampered` (authentication), `stale_epoch`
(an epoch it holds no keys for and cannot hold) and `overflow` (the `max_ahead` bound). A
dropped datagram never changes any other state. These counts are the observable cost of noise
and attack traffic; nothing is dropped silently.

### DTLS-7: Certificates, fingerprints and roles

**Identity.** `identity(take key, take certificate, r)` pairs a P-256 signing key (policy
`sign(p256)`, from `secret.p256_key`) with its DER certificate, as
[`std.x509_build.self_signed_p256`](../../core/std/api/x509_build.md) makes it. It checks
that the certificate's subject public key is the key's point (by signing a fixed message
with `secret.sign(key, P256, r, ..)`, which needs the `Random`, and verifying with
`p256.verify` against the certificate's key) and is `Config` otherwise; a `sign` error maps
as DTLS-8's table gives it.

**One certificate, many endpoints.** `client` and `server` take an `Identity`, so each
endpoint holds one. `share_identity(i)` makes another `Identity` for the same key and
certificate, through `secret.share` on the key ([TLS-5](tls.md#tls-5-secrets)) and a copy
of the certificate bytes, without signing again. A server that answers many peers under one
certificate (one `a=fingerprint` in all its SDP) builds one `Identity` once and shares it
per admitted peer. Sharing grants no new use: the key keeps its `sign(p256)` policy, and
dropping one endpoint's identity leaves the others working. The
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
retransmissions. The `handshake_ns` deadline (DTLS-5 Timers) still runs while it waits: an
endpoint whose `expect` has not come by then closes as `Timeout`. When the peer's certificate is known and fingerprints are given, the first
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
listen first), runs `admit` on each datagram from an unproven address and builds a server
only on `Admitted`, passes the setup side and the fingerprints from signalling, calls `poll` until
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
pub type Limits { send_streams: u32, receive_streams: u32, ended_streams: u32 }
pub type Opened { length: u64, ssrc: u32, index: u64 }

pub opaque type Session                 // holds TLS-8 sealers and openers; move-only

pub fn session(take s: schedule.Schedule, p: schedule.SrtpProfile, l: Limits) -> Result[Session, SrtpError]
pub fn session_pair(take send: schedule.Schedule, take receive: schedule.Schedule, p: schedule.SrtpProfile, l: Limits) -> Result[Session, SrtpError]
pub fn send_from(mut s: Session, ssrc: u32, roc: u32, seq: u16, srtcp: u32) -> Result[(), SrtpError]
pub fn receive_from(mut s: Session, ssrc: u32, roc: u32, seq: u16, srtcp: u32) -> Result[(), SrtpError]
pub fn end_send(mut s: Session, ssrc: u32)
pub fn end_receive(mut s: Session, ssrc: u32) -> Result[(), SrtpError]
pub fn protect_rtp(mut s: Session, mut buf: Bytes) -> Result[u64, SrtpError]
pub fn protect_rtp_into(mut s: Session, header: Bytes, src: Bytes, from: u64, mut dst: Bytes) -> Result[u64, SrtpError]
pub fn unprotect_rtp(mut s: Session, mut buf: Bytes) -> Result[Opened, SrtpError]
pub fn protect_rtcp(mut s: Session, mut buf: Bytes) -> Result[u64, SrtpError]
pub fn unprotect_rtcp(mut s: Session, mut buf: Bytes) -> Result[u64, SrtpError]
pub fn tag_room(p: schedule.SrtpProfile) -> u64
```

`session` takes a schedule holding both directions (from `take_srtp`); `session_pair` takes
a `send` import and a `receive` import of the same profile. A schedule of the wrong
direction or profile is `Direction`. The send and receive keys differ. Under DTLS-SRTP they
always do: they are the client's and the server's halves of the exporter. Under imports they
are two separately issued secrets, and that they differ is the caller's obligation: two
independently designated secrets can hold the same bytes, and `session_pair` does not
compare them, because an equality probe on key material would be a key oracle. Nothing in a
`Session` lets a received packet's
context serve sending, or the reverse.

### SRTP-3: Streams, ROC and sending

**Send streams.** Each SSRC a session sends on has a sealer for `Rtp` and one for `Rtcp`,
issued from the schedule at the SSRC's first packet (TLS-8 domain = SSRC), plus its highest
sent index, ROC and SRTCP index. At most `send_streams` SSRCs exist at once; one more is
`Streams`, before issue. `end_send(ssrc)` frees the stream's sealers but keeps the SSRC
marked: TLS-8 never issues a domain twice in one send epoch, so a later packet on an ended
SSRC is `Retired`. A sender that needs a new stream uses a new SSRC.

**Starting a send stream.** `send_from(ssrc, roc, seq, srtcp)` places a stream before its
first packet: the first RTP packet is estimated against `2^16 × roc + seq` and may equal it
(the `Rtp` sealer is issued with that index as its TLS-8 `floor`), and the first SRTCP index
is `srtcp` (the `Rtcp` sealer's floor), which must be below `2^31`. Without it a stream
starts at ROC 0 with `s_l` equal to its first SEQ, and SRTCP index 0. `send_from` on an SSRC
that has already sent, or has ended, is `Retired`: a start can never lower a floor.

**Sending index.** The packet index is `i = 2^16 × ROC + SEQ` (RFC 3711 §3.3.1). For each
packet the sender estimates `v` against its highest sent index with RFC 3711 Appendix A's
three cases **without reducing modulo `2^32`**, exactly `rtp.seq.estimate`
([RTP-2](rtp.md#rtp-2-extended-sequence-numbers)), and uses `i = 2^16 × v + SEQ`. `v = 2^32`
(the next ROC past the last) is `Exhausted`, since the index would pass `2^48 − 1`. `v = ROC
− 1`, at ROC 0 too (where `rtp.seq.estimate` answers `None`), is `Reused`: it names an index
below the highest sent. The sealer refuses any index not above every index it sealed (TLS-8
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
`Malformed`). It uses and then increments that stream's SRTCP index, starting at 0 or at
`send_from`'s `srtcp`, and always encrypts (`E = 1`). Under AEAD the first 8 bytes followed
by `E ‖ index` form the AAD, and `E ‖ index` follows the tag on the wire (RFC 7714 §9).
Under CM it seals with the first 8 bytes as AAD and `extra` = `E ‖ index`, then moves the
10-byte tag 4 bytes on and
writes `E ‖ index` before it (RFC 3711 §3.4). `buf` must have `tag_room(p) + 4` spare.
Index `2^31 − 1` seals and the next is `Exhausted`. Nothing wraps.

### SRTP-4: Receiving, ROC estimation and replay

**Receive streams.** A receive stream is keyed by SSRC and holds ROC, `s_l` (highest
authenticated SEQ) and a replay window of 128 indices for RTP, plus a 128-entry window for
the SRTCP index. Openers are stateless (TLS-8), so the session holds one `Rtp` and one
`Rtcp` opener. A packet on an unknown SSRC is authenticated first, with ROC 0 and `s_l` =
its SEQ (RFC 3711 §3.3.1), or with the values `receive_from` set; only a packet that
authenticates creates the stream. When `receive_streams` streams exist, an authenticated
packet on a new SSRC is `Streams` and creates nothing. `receive_from(ssrc, roc, seq, srtcp)`
places a stream before its first packet (RTP estimated against `2^16 × roc + seq`, SRTCP
window starting at `srtcp`); on an SSRC that has a live stream or a kept entry (below) it is
`Retired`, so it can never lower a replay floor.

**Ending a receive stream.** `end_receive(ssrc)` frees the stream's windows but keeps, for
that SSRC, the highest authenticated RTP index and the highest authenticated SRTCP index:
the same key still opens every packet ever recorded on it, so forgetting them would let a
recorded packet authenticate and be delivered again (RFC 3711 §3.3.2). A later RTP packet
whose estimate against the kept index is at or below it, or a later SRTCP packet whose index
is at or below its kept index, is `TooOld`. One above authenticates first and then resumes
the stream: both windows start empty and both kept indices stay as floors. Until a packet of
a kind authenticates on the resumed stream, that kind is estimated and checked against its
kept index; an RTP packet that authenticates sets the ROC its estimate gave. Whichever kind
resumed it, an RTP index at or below the kept RTP index and an SRTCP index at or below the
kept SRTCP index remain `TooOld`, though they lie inside the new windows. If
`receive_streams` streams exist, that packet is `Streams` and the entry is unchanged. A kept
entry lasts as long as the session: a resume does not free it, and ending that SSRC again
raises its two indices. At most `ended_streams` entries exist; an `end_receive` of an SSRC
without one, beyond that, is `Streams` and leaves the stream open, windows and all, so the
bound never costs replay protection.

**Index estimate.** For a known stream, `v` is chosen from `{ROC − 1, ROC, ROC + 1}` by RFC
3711 Appendix A's three cases, without reduction modulo `2^32`, and `i = 2^16 × v + SEQ`.
This is exactly `rtp.seq.estimate` ([RTP-2](rtp.md#rtp-2-extended-sequence-numbers)), which
also does not reduce; once that lands, `srtp` depends on `rtp` and calls it rather than
keeping a second copy, and sending uses it too. `rtp.seq.estimate` answers `Before` (a `v`
of `ROC − 1` when ROC is 0), which is `TooOld`, or `Beyond` (above `2^48 − 1`), which a
receiver discards and a sender treats as exhausted.

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
tag back over them before `open`), checks the SRTCP window of the sender SSRC (an index more
than 127 below the highest authenticated one is `TooOld`, a marked one `Replayed`). It
authenticates `E ‖ index` when opening the compound and returns the plaintext length. As
for RTP, the SRTCP window marks an index and moves its highest only after `open` authenticates, so a
forged far-ahead SRTCP index moves nothing. `E = 0` is accepted: the
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
| SDP certificate fingerprint via `x509_build` | DTLS-7 Identity, Fingerprints, Pinning; DTLS-5 Message order | `fingerprint_pins_peer`, `possession_before_keys`, `interop_openssl` |
| negotiated roles | DTLS-7 Roles | `setup_roles` |
| `use_srtp` exporter | DTLS-5 Server choice, DTLS-8 The exporter | `interop_openssl`, `exporter_once_after_pin`, `possession_before_keys` |
| SRTP/SRTCP AES-GCM and AES-CM/HMAC | SRTP-1, SRTP-3, SRTP-4 | `rtp_header_boundary`, `srtcp_trailer_order` |
| independent in and out keys | SRTP-2, DTLS-8 | `directions_independent` |
| SSRC, ROC and the replay window | SRTP-3, SRTP-4 | `roc_rollover`, `replay_window`, `sender_index_monotone`, `streams_after_auth` |
| invalidates DTLS state on revocation | DTLS-8 Revocation, SRTP-5 | structural: no operation returns a sealer or opener; dropping is the fence |

The rest of the contract (RTP forwarding, dynacast, the `revoke` call itself, ICE, rooms)
belongs to media-rtp and media-webrtc-sfu, which consume these two packages.

## Prior art

Goose (`/tmp/ref-goose` at `d5ae5bb`) and dimpl (`/tmp/ref-dimpl` at `e341a90`) for DTLS,
with str0m (`/tmp/ref-str0m` at `2ada76a`) for SRTP. The 2341 amend added OpenSSL
(`/tmp/ref-openssl` at `ce0aecb6`), pion/dtls (`/tmp/ref-pion-dtls` at `67c3adf`) and
webrtc-rs's sans-IO `rtc` (`/tmp/ref-webrtc-rs` at `0fbcaa7`) for cookie admission and
record versions.

| item | source | verdict |
|---|---|---|
| Any DTLS, SRTP, UDP or crypto surface | Goose: its standard library is `base64`, `binary`, `csv`, `dictionary`, `exception`, `ffi`, `json`, `math`, `os`, `regex`, `slice_pool`, `slot_pool`, `std` and `vec` (`stdlib/`); it has no network or cryptography modules | NOT TAKEN: nothing to take |
| Fixed-size inline state with no heap per packet | Goose's no-heap design (`README.md`) | NOT TAKEN as a source: the zero-allocation target here comes from PLAN, and the replay windows are inline `u128` bitmaps for their own reason |
| Sans-IO `handle_packet` / `poll_output` / `handle_timeout` | dimpl `src/lib.rs:58` | ADOPTED as `receive` / `poll` (timers run inside `poll`) |
| Every error fatal, noise discarded | dimpl `src/lib.rs:80` | ADOPTED, with counted discards (DTLS-6) where dimpl only logs |
| Application validates the peer certificate | dimpl `src/lib.rs:50` | ADAPTED: the library pins RFC 8122 fingerprints itself, so no caller can forget |
| DTLS 1.3 and version auto-sense | dimpl `src/lib.rs:27` | NOT TAKEN for Wave 1 (DTLS 1.3 is out of Wave 1) |
| Stateless cookie `HMAC(secret, client_random)` | dimpl `src/dtls12/server.rs:378`, `:1217` | ADAPTED: the address is bound too, and an ICE-verified address skips it |
| Stateless listen before any connection; cookie judged from the first ClientHello fragment | OpenSSL `DTLSv1_listen`, `ssl/d1_lib.c:1015-1026` | ADOPTED as `admit` |
| `{254, 255}` on records before the version is fixed, including HelloVerifyRequest | OpenSSL `ssl/record/rec_layer_d1.c:929-931`, `ssl/d1_lib.c:1111` | ADOPTED for HelloVerifyRequest; accepted on receipt (DTLS-4 Framing) |
| A connection per source address before the cookie | pion/dtls `listener.go:17`, `internal/flight/flight12/flight0handler.go:211`; webrtc-rs `rtc-dtls/src/endpoint.rs:227` | REPLACED by `admit`: per-address state before the cookie is what RFC 6347 §4.2.1 exists to prevent |
| Flight timer 1 s, 4 retries, 40 s handshake, 50% jitter | dimpl `src/config.rs:93-95`, `src/timer.rs` | ADAPTED: RFC 6347's 1 s doubling to 60 s, 7 retries, 60 s deadline, no jitter, so sim time is exact |
| MTU 1,150, 30 queued records | dimpl `src/config.rs:88-89` | ADAPTED: MTU 1,200 and 32 held fragments, both explicit `Config` |
| 64-record replay window per epoch, updated after decryption | dimpl `src/window.rs` | ADAPTED: epoch 1 only; epoch 0 keeps none, since its records cannot authenticate (DTLS-4 Replay) |
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
`core/programs/dtls/interop.py` on loopback; it also runs the dimpl law.

### DTLS

- **`interop_openssl`.** *Scenario:* a Hirsel client against
  `openssl s_server -dtls1_2 -use_srtp <profile> -keymatexport EXTRACTOR-dtls_srtp -keymatexportlen <n>`
  with a P-256 certificate and `-verify 1`, and a Hirsel server against
  `openssl s_client -dtls1_2` with the same options. Run each of the three suites at least
  once and each of the three SRTP profiles at least once, the Hirsel server once in
  `Cookie` mode through `admit` (OpenSSL's client answers a HelloVerifyRequest by default,
  and its ClientHello records carry `{254, 255}`), and the Hirsel client once against
  `openssl s_server -dtls1_2 -listen`, whose HelloVerifyRequest record and body carry
  `{254, 255}` (DTLS-4 Framing). *Oracle:* OpenSSL's wire output and printed keying
  material. The handshake completes on both sides. Python `cryptography`
  seals one RTP packet under OpenSSL's exported bytes, cut as TLS-8 `srtp` cuts them, and
  gets the bytes Hirsel's `protect_rtp` makes, and the reverse opens. The peer certificate
  Hirsel reports has the SHA-256 that `openssl x509 -fingerprint -sha256` prints for
  OpenSSL's certificate.
- **`interop_dimpl`.** *Scenario:* a Hirsel client against a dimpl 1.2 server and a Hirsel
  server against a dimpl 1.2 client, each for one GCM suite and one GCM SRTP profile, through
  a Rust driver (`core/programs/dtls/interop_dimpl.rs`) that exchanges datagrams with the
  Hirsel endpoint over loopback UDP, since the two run as separate processes. *Oracle:* dimpl's wire output and its
  exported keying material, which the driver prints. Both complete, and the SRTP keys cut from
  dimpl's exporter output equal the Hirsel session's, checked by one packet sealed on each
  side and opened on the other.
- **`handshake_survives_loss`.** *Scenario:* client and server in process, each datagram
  independently dropped with probability 0.2, duplicated with 0.05 and delayed by up to
  three datagrams, from a seeded stream; 32 seeds, under 15 s. Then the same 32 seeds with
  duplication and delay but no loss. *Oracle:* an exhaustive list of ends per seed, by hand
  from DTLS-5. Let `t_c` be the client's first send and `t_s` the instant the server accepted
  its first ClientHello, both read from the harness's own delivery log. At defaults a flight
  cannot exhaust its 7 retries before `handshake_ns` (that takes at least 1 + 2 + 4 + 8 + 16
  + 32 + 60 + 60 = 183 s, past 60 s), so every `Timeout` is at its side's start plus 60 s. The
  allowed ends are exactly: (1) both `Connected`, with a cross-check (the client's SRTP
  packet opens at the server and the reverse); (2) the client `Timeout` at `t_c + 60 s` and
  the server `Timeout` at `t_s + 60 s`; (3) the client `Timeout` at `t_c + 60 s` and the
  server `Connected` (flight 5 arrived and every flight 6 was lost); (4) the client
  `Timeout` at `t_c + 60 s` and the server still `Handshaking` with `poll` answering `Idle`,
  because no ClientHello ever reached it. The client cannot connect without the server,
  since flight 6 follows a verified flight 5. Any other end fails. With no loss, every seed
  ends in (1). The number of
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
- **`final_flight_resent`.** *Scenario:* `mtu` 300, so flight 5 spans three datagrams. The
  server's flight 6 is dropped. The client retransmits flight 5 twice. Then, at the
  connected server, a replay of an epoch-0 Certificate record from flight 5 and a replay of
  the epoch-1 Finished record with its original record number; at the connected client, a
  retransmitted flight 4. *Oracle:* RFC 6347 §4.2.4 by hand. The server is `Connected`
  after flight 5. Each retransmitted flight 5 gets exactly one flight 6, with record numbers
  above the previous ones, and the client then connects. The replayed Certificate produces
  no datagram and counts `stale_epoch`; the replayed Finished produces none and counts
  `replayed`; the flight 4 at the client produces none and counts `stale_epoch`. Red on a
  server that answers flight 6 per epoch-0 record or per datagram.
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
- **`refusals`.** *Scenario:* hand-built peer messages, one handshake each: a ClientHello
  without EMS; one with a non-empty `srtp_mki`; one sharing no SRTP profile while
  `Config.srtp` is non-empty; one whose `signature_algorithms` lacks 0x0403; one whose body
  `client_version` is `{254, 255}`; a ServerHello choosing a suite the client did not offer;
  a ServerHello without `use_srtp` to a client that offered it; a ServerKeyExchange with a
  broken signature; a server flight 4 with no ServerKeyExchange; a client flight 5 with an
  empty Certificate; a client flight 5 whose Finished is sent in epoch 0, ahead of its
  ChangeCipherSpec (after it, DTLS-4 Replay discards it). *Oracle:* the
  DTLS-5 failure table and RFC 5246 §7.3, §7.4.1.2 and Appendix E.1 by hand: alerts 40, 47,
  40, 40, 70, 47, 40, 51, 10, 42 and 10.
- **`possession_before_keys`.** *Scenario:* a genuine handshake between client A and server
  S records A's Certificate message. A hand-built client C, with its own P-256 key, then
  runs against a fresh server that `expect`s A's fingerprint. C's flight 5 carries A's
  certificate and (a) no CertificateVerify, then ChangeCipherSpec and a Finished, which C
  numbers with the `message_seq` CertificateVerify would have had, and an
  application record that C seals under keys it derived itself; (b) a CertificateVerify
  signed by C's key instead. *Oracle:* RFC 5246 §7.4.8 and RFC 8122 §6.2 by hand: only
  CertificateVerify proves possession, and the fingerprint is public. The server closes as
  `Timeout` at the 60 s deadline in (a), its ChangeCipherSpec discarded as early, and
  `SentAlert(51)` in (b), and in both it never reports
  `Connected`, delivers 0 bytes of C's application record, and answers `take_srtp` with
  `NotConnected` before closing and `Closed(_)` after. Red on any state machine that treats
  CertificateVerify as optional or pins on the certificate alone.
- **`record_replay_and_discards`.** *Scenario:* after connecting, an application record
  delivered twice, one 70 records behind the newest, one with a flipped tag bit, and one in
  epoch 2. Separately, mid-handshake, after the client has processed flight 4: an injected
  epoch-0 record with sequence number `2^48 − 1` carrying a copy of ServerHelloDone (a
  `message_seq` already processed), then the genuine flight 6, whose ChangeCipherSpec
  record has a low epoch-0 number. *Oracle:* RFC 6347 §§4.1.2.6, 4.1.2.7 and §4.2.2 by hand.
  The four are discarded and counted as `replayed`, `replayed`, `tampered` and
  `stale_epoch`, no alert is sent, and the next genuine record is delivered. In the second
  part the client connects with no retransmission from either side and `replayed` stays 0.
  Red on an epoch-0 replay window.
- **`cookie_bounds_unproven_address`.** *Scenario:* `admit` alone, with one key: 10
  cookieless ClientHellos from 10 addresses; one with a cookie computed for another address;
  a 61-byte first fragment of a cookieless ClientHello (empty `session_id`, ending after the
  cookie length byte); a 60-byte one ending before that byte; the second fragment of a
  cookieless ClientHello; and the first fragment of a two-fragment ClientHello carrying the
  right cookie. A second server built on that `Admitted` receives only the first fragment at
  `t`: its `poll` answers `WakeAt(t + 60 s)`, and a `poll` at `t + 60 s` closes it as
  `Timeout`. Then a server built on that `Admitted` receives both fragments. Last, a
  `Cookie` server built without `admit` receives a cookieless ClientHello. *Oracle:* RFC 6347
  §4.2.1 by hand, with each expected cookie from Python's `hmac` over the hand-built address
  bytes and client random. The first 12 inputs each give `Reply(60)`, and the appended bytes
  equal a hand-built HelloVerifyRequest: `{254, 255}` in the record header and in
  `server_version`, the ClientHello record's sequence number, `message_seq` 0 and the Python
  cookie; no reply is longer than its input. The 60-byte input and the second fragment give
  `Discard` with `out` unchanged. The last `admit` gives `Admitted`, no `Endpoint` having
  been built before it; that server reassembles the ClientHello and sends flight 4, whose
  first record has the first fragment's record sequence number and whose ServerHello has
  `message_seq` 1. The server built without `admit` sends nothing and counts one
  `malformed`.
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
  `TooOld`. The forgery is `Tampered` and moves nothing, so 201 opens. Then, on a second
  SSRC with `ended_streams` 1: open RTP indices 0 to 5 and SRTCP indices 0 to 3, a forged
  SRTCP packet at index 10,000 with a bad tag (`Tampered`, and SRTCP 4 then opens),
  `end_receive`, then RTP index 3 (`TooOld`), SRTCP index 2 (`TooOld`), `receive_from` on
  that SSRC (`Retired`), RTP index 6, which opens, and then RTP index 4
  and SRTCP index 2 again, both `TooOld`. Last, a third SSRC's `end_receive`
  while the one kept entry is full is `Streams`, and that stream still answers a repeat of
  its last index with `Replayed`. *Oracle:* RFC 3711 §3.3.2 by hand: a key opens every packet
  it ever sealed, so only a kept floor refuses a recorded one.
- **`srtcp_trailer_order`.** *Scenario:* `protect_rtcp` of libSRTP `rtcp_plaintext_ref`
  under both validation keys; an `E = 0` compound; then a stream placed by
  `send_from(ssrc, 0, 0, 2^31 − 1)` sends one compound and one more, and a receiver placed
  by `receive_from(ssrc, 0, 0, 2^31 − 1)` opens the first. *Oracle:* libSRTP
  `test/srtp_driver.c` at `fd08747f` for the protected bytes (the tag before `E ‖ index`
  under AEAD, after it under CM), RFC 3711 §3.4 for `E = 0`, Python `cryptography` with the
  RFC 7714 §9.1 nonce for index `2^31 − 1` (the trailer is `0xFFFFFFFF`), and SRTP-3 for the
  last: it seals, and the next is `Exhausted`.
- **`directions_independent`.** *Scenario:* the two sessions of one in-process DTLS
  association. *Oracle:* TLS-8's direction rule by hand. A's protected packet unprotects at
  B. A's own `unprotect_rtp` of it is `Discarded(Tampered)`, because its opener holds B's key.
  `session_pair` with two `send` imports is `Direction`.
- **`sender_index_monotone`.** *Scenario:* send SEQ 10, then 9, then 11; on a second fresh
  stream send SEQ 5, then SEQ 40,000; on a stream placed by `send_from(ssrc, 2^32 − 1,
  65,535, 0)`, send SEQ 65,535, then SEQ 0; then `send_from` on the first SSRC. *Oracle:*
  RFC 3711 Appendix A's three cases computed by hand, unreduced, and RFC 7714 §10's `2^48`
  limit. 9 gives `v` 0 and index 9, below 10: `Reused` with `buf` unchanged, and 11 then
  seals. 40,000 against `s_l` 5 is more than `2^15` above, so `v` = ROC − 1 at ROC 0:
  `Reused`. 65,535 gives index `2^48 − 1`, which seals; 0 against `s_l` 65,535 gives `v` =
  `2^32`: `Exhausted`, where an estimate reduced modulo `2^32` would answer index 0 and
  `Reused`. The late `send_from` is `Retired`.
- **`streams_after_auth`.** *Scenario:* `receive_streams` 2. Forged packets on 5 new SSRCs,
  then genuine packets on 3 new SSRCs. Then `end_send` of an SSRC and a packet on it.
  *Oracle:* SRTP-3 and SRTP-4 by hand. The forgeries create no stream and no kept entry. The
  first two genuine SSRCs open, and the third is `Streams`. The ended SSRC is `Retired`.
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
| **S0** expose TLS-8 DTLS and SRTP schedule operations (landed: 034e6dcd, e07a09da) | M | — | `schedule.dtls12`, `schedule.srtp`, `schedule.finished`, `schedule.verify_finished`, `schedule.import_srtp` and `schedule.SrtpProfile`, `schedule.Suite12` in `core/core/schedule.hirsel`, over the TLS-8 T14 kernels | TLS-8 PC10 and PC2. The [source laws](../../core/std/tests/secret_agree.hirsel) check DTLS 1.2 records and Finished, SRTP import direction and libSRTP packets, and both sides of the DTLS-SRTP exporter on the evaluator, Cranelift and LLVM. |
| **S1** `std.x509_key` | S | — | SPKI reader for P-256 and P-384 points (DTLS-7) | one case in the std x509 laws: OpenSSL-generated P-256 and P-384 certificates give the points `openssl ec -pubout` prints; RSA and trailing bytes give `Invalid` |
| **S2** DTLS records | M | S0 | `core/programs/dtls/`<!-- doc-ref: planned --> package, record framing and versions, epochs, the epoch-1 replay window and no epoch-0 window, alerts, fragmentation and reassembly, the transcript (DTLS-4, DTLS-5 Fragmentation and Transcript, DTLS-6) | `record_replay_and_discards` (the connected part, hand-built records), `fragments_reassemble` (hand-built parts) |
| **S3** DTLS handshake | L | S1, S2 | `admit`, client and server flights, message order, versions, server choice, EMS, `use_srtp`, failures, timers and final flights, early data held to `Connected`, `take_srtp`, error mapping (DTLS-3, DTLS-5, DTLS-8) | `possession_before_keys`, `refusals`, `cookie_bounds_unproven_address`, `final_flight_resent`, `record_replay_and_discards` (the mid-handshake epoch-0 part), `flight_timer_schedule`, `offered_13_negotiates_12`, `exporter_once_after_pin`, `handshake_survives_loss`. Its attack must bound the flight 4 resends that repeated epoch-0 `message_seq` values trigger in a server waiting for flight 5 to one per timer period. |
| **S4** identity, fingerprints, roles | S | S1 | DTLS-7, `share_identity` | `fingerprint_pins_peer`, `setup_roles` |
| **S5** SRTP sessions | M | S0; `rtp.seq.estimate` once rtp.md's RTP-2 lands (until then S5 keeps the same unreduced Appendix A estimate and switches) | `core/programs/srtp/`<!-- doc-ref: planned --> package (SRTP-2 to SRTP-6), with `send_from`/`receive_from` SRTCP starts and kept floors after `end_receive` | `rtp_header_boundary`, `roc_rollover`, `replay_window`, `srtcp_trailer_order`, `directions_independent`, `sender_index_monotone`, `streams_after_auth`, `fan_out_into` |
| **S6** interop | M | S3, S4, S5 | `interop.py` against OpenSSL on loopback (including `s_server -listen`, whose `{254, 255}` HelloVerifyRequest record DTLS-4 Framing accepts), and against dimpl at `e341a90` through a small Rust driver built from a clone in `/tmp`, never vendored | `interop_openssl`, `interop_dimpl`, the live part of `fragments_reassemble` |
| **S7** SRTP cost | S | S5 | counted-allocator and instruction-count run (cost targets) | [`python3 bench/srtp/cost.py <scratch-dir>`](../../bench/srtp/cost.py) checks 0 allocations per packet and reports instructions per packet |

S1, S2 and S5 run in parallel after S0. S0 blocks S2 and S5. A Sol lane can take
S0, S1, S2, S4, S5 and S7 from this text. S3 is the security-bearing slice: PLAN row 1.2
asks for an Astra security attack on it (the handshake state machine and message order,
`admit`, fingerprint pinning and proof of possession, the exporter) before it lands. S6 closes the build.

**Deviation from PLAN row 1.2's verification.** PLAN asks for interop with Chromium, Firefox,
str0m and dimpl under `sim.udp` at 20% loss. This text covers OpenSSL and dimpl at wire level
(S6), and loss in process (`handshake_survives_loss`), because `std.sim.udp` has not landed
and a browser needs the SDP and ICE glue of PLAN row 1.6. Browser interop therefore moves to
row 1.6's headless Chromium end-to-end test, which drives this endpoint; str0m uses dimpl for
DTLS, so the dimpl law covers it. The loss law moves to `std.sim.udp` when that lands.
