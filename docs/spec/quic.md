# QUIC

*Specified 2026-11-26 (sfu-pool-media-w2-spec-2888, Horizon H3 part 2, Media Wave 2 package 1
of 4). Not built. Rules QUIC-1 to QUIC-10, laws Q1 to Q16, slices Q0 to Q8. Goes to design
review (astra) before any build; the open items are at the end.*

`quic` is a sans-IO QUIC version 1 endpoint, written in Hirsel under `core/programs/quic` <!-- doc-ref: planned -->.
It is the transport under [WebTransport](webtransport.md), [MoQ](moq.md) and
[sfu_cascade](sfu_cascade.md). It covers:

- [RFC 9000](https://www.rfc-editor.org/rfc/rfc9000) (transport) and
  [RFC 8999](https://www.rfc-editor.org/rfc/rfc8999) (the version-independent invariants);
- [RFC 9001](https://www.rfc-editor.org/rfc/rfc9001) (TLS 1.3 for QUIC), over TLS-8's
  schedules;
- [RFC 9002](https://www.rfc-editor.org/rfc/rfc9002) (loss detection and congestion control);
- [RFC 9221](https://www.rfc-editor.org/rfc/rfc9221) (unreliable datagrams), which
  WebTransport, MoQ and cascade media need;
- [RFC 9438](https://www.rfc-editor.org/rfc/rfc9438) (CUBIC) as the default controller.

Like every Wave 1 and Wave 2 media package it is pure Hirsel with no `Foreign` code in its
reach, so FFI-8 replay and `hirsel test --sim` cover it end to end.

## Wrap a reference stack, or write it in Hirsel

The question the brief asks, answered first because it decides the rest.

**The case for wrapping.** quinn-proto (/tmp/ref-quinn at `75d08d9`) is already sans-IO
(`Endpoint::handle_event`, `Connection::poll_transmit`, `handle_timeout`;
quinn-proto/src/endpoint.rs, connection/mod.rs), so its shape matches ours. It has years of
interop-runner results, fuzzing, and production use; quiche (/tmp/ref-quiche at `ac43c0d`)
and msquic (/tmp/ref-msquic at `aa31c3f`) have the same at Cloudflare and Microsoft scale.
QUIC's loss recovery and congestion control are where bugs hide for years, and a wrapped
stack inherits other people's fixes. A wrap could ship in weeks.

**Why Hirsel anyway.**

1. **Keys.** TLS-8's rule is that no protocol key is ever a `Secret` or a program value: it
   lives inside a `Schedule`, used once for one purpose. quinn, quiche and msquic each run
   their own TLS (rustls, BoringSSL, schannel or OpenSSL) and hold every traffic secret in
   their own memory. Wrapping one means either a second key path outside TLS-8 or patching
   its crypto layer onto ours, which is most of the hard part of a QUIC stack anyway.
2. **Replay and simulation.** A `Foreign` stack is outside FFI-8 replay and outside
   `hirsel test --sim`. QUIC's hardest laws (loss, reordering, migration, amplification)
   are exactly the ones `sim.udp` exists to drive deterministically.
3. **Memory.** Foreign allocations are outside MEM-19 accounting, so a connection's charges
   would be a guess, and RTC-10's exact room cost could not extend to a cascade link.
4. **Units and zero copy.** The SFU's fan-out shares one plaintext `Shared[Bytes]` across
   units (RTC-5). A wrapped stack copies into and out of its own buffers.
5. **The plan already ruled it.** hirsel-briefs research-1124/realtime-media/PLAN.md, line 10: every Wave 1
   and Wave 2 protocol library is pure Hirsel.

**Verdict: write it in Hirsel**, with quinn-proto's decomposition as the model (ADAPTED, see
Prior art) and quinn and quiche as interop peers and differential oracles in the laws, never
linked into the package.

## Scope

| Area | In Wave 2 | Not in Wave 2 (and why) |
| --- | --- | --- |
| Versions | QUIC v1 (`0x00000001`); Version Negotiation packets for any other version | v2 ([RFC 9369](https://www.rfc-editor.org/rfc/rfc9369)): no browser needs it; adding it later is a label and a version number |
| Roles | server and client | — |
| Handshake | TLS 1.3 in Hirsel over TLS-8 (QUIC-3); X25519 and P-256; the three TLS-8 suites; server signature P-256 ECDSA or Ed25519 | 0-RTT and resumption: TLS-8 has no PSK schedule, WebTransport forbids CONNECT in 0-RTT (webtrans-http3-17 §3.2), and 0-RTT replay is a cost every consumer here would refuse; RSA server keys: `secret.sign`'s `rs256` is PKCS#1 v1.5, which TLS 1.3 forbids for CertificateVerify |
| Peer authentication | server pinned by SPKI digest (client role); client certificates pinned by SPKI digest (mutual, for cascade) | Web PKI chain verification in the client role: no Hirsel chain verifier outside `tls.connect`'s provider; open item O4 |
| Address validation | Retry (RFC 9000 §8.1.2) with the RFC 9001 §5.8 integrity tag; NEW_TOKEN tokens; the 3× anti-amplification limit | — |
| Connection IDs | NET-16 `Steer.Quic` format, so a `udp_bind_group` steers to the owning member; NEW_CONNECTION_ID, RETIRE_CONNECTION_ID; stateless reset | QUIC-LB encrypted CIDs ([draft-ietf-quic-load-balancers](https://datatracker.ietf.org/doc/draft-ietf-quic-load-balancers/)): one host, no external load balancer |
| Migration | passive: path validation and NAT rebinding on the server; the client keeps its path | active client migration and `preferred_address`: no consumer needs them |
| Streams | bidirectional and unidirectional, stream and connection flow control, RESET_STREAM, STOP_SENDING, RESET_STREAM_AT ([draft-ietf-quic-reliable-stream-reset](https://datatracker.ietf.org/doc/draft-ietf-quic-reliable-stream-reset/)) | — (WebTransport requires RESET_STREAM_AT, webtrans-http3-17 §3.1) |
| Datagrams | RFC 9221 DATAGRAM frames, never retransmitted, counted for congestion control | — |
| Recovery | RFC 9002 loss detection, PTO, ACK frequency of every second ack-eliciting packet, `max_ack_delay` 25 ms; NewReno and CUBIC with HyStart++ ([RFC 9406](https://www.rfc-editor.org/rfc/rfc9406)); pacing | BBRv3: still an Internet-Draft; open item O2 |
| Path MTU | 1,200 bytes base; DPLPMTUD ([RFC 8899](https://www.rfc-editor.org/rfc/rfc8899)) probes up to the socket's `slot_bytes` | — |
| ECN | RFC 9000 §13.4 validation from NET-16's `ecn` field; CE reaction as loss | L4S: deferred with SCReAM (C3) |
| Key update | RFC 9001 §6 over TLS-8 `key_update` | — |
| Multipath | — | draft-ietf-quic-multipath: TLS-8 has the `QuicMultipath` profile, but no consumer needs it in Wave 2 |
| Batching | NET-16 GSO trains: equal-length packets to one peer in one `udp_send` | — |

## QUIC-1: Shape

```hirsel unbuilt
pub opaque type Endpoint
pub opaque type Connection
pub type ConnectionHandle = ConnectionHandle(u32)
pub type StreamId = StreamId(u64)
pub type Side = Client | Server
pub type EndpointConfig { member: u16, members: u16, cid_len: u8, retry: RetryPolicy, reset_key: Secret, token_key: Secret, limits: EndpointLimits }
pub type EndpointLimits { connections: u32, unvalidated: u32, datagram_bytes: u32 }
pub type TransportConfig { max_idle_ms: u32, initial_max_data: u64, initial_max_stream_data: u64, max_streams_bidi: u64, max_streams_uni: u64, max_datagram_frame: u16, cc: Controller, alpn: List[String] }
pub type Controller = NewReno | Cubic
pub type RetryPolicy = Never | Always | Above(u32)
pub type Identity = Identity(chain: List[Bytes], key: Secret, alg: SignAlg)
pub type Pin = Pin(Inline[u8, 32])

pub type Input = Datagram(from: Addr, to: Addr, ecn: u8, data: Bytes) | Timeout(now: u64)
pub type Output = Transmit(to: Addr, ecn: u8, segment: u16, at: u64, len: u64) | Wake(at: u64)
pub type Event = Connected(c: ConnectionHandle, alpn: String) | Opened(ConnectionHandle, StreamId) | Readable(ConnectionHandle, StreamId) | Writable(ConnectionHandle, StreamId) | Reset(ConnectionHandle, StreamId, u64) | Stopped(ConnectionHandle, StreamId, u64) | Datagram(ConnectionHandle, Bytes) | Closed(ConnectionHandle, CloseReason)
pub type CloseReason = Transport(code: u64, frame: u64) | Application(code: u64) | Idle | Reset | Refused(Refusal)
pub type Refusal = Version | Alpn | Pin | Limit(u32) | Token | Certificate

pub fn endpoint(r: Random, c: EndpointConfig, server: Option[(Identity, TransportConfig)]) -> Result[Endpoint, QuicError]
pub fn connect(mut e: Endpoint, now: u64, to: Addr, name: String, pin: Pin, client: Option[Identity], t: TransportConfig) -> Result[ConnectionHandle, QuicError]
pub fn receive(mut e: Endpoint, now: u64, take i: Input, mut events: List[Event])
pub fn poll(mut e: Endpoint, now: u64, mut out: Bytes, mut tx: List[Output]) -> Option[u64]
pub fn open(mut e: Endpoint, c: ConnectionHandle, bidi: bool) -> Result[StreamId, QuicError]
pub fn write(mut e: Endpoint, c: ConnectionHandle, s: StreamId, data: Bytes, fin: bool) -> Result[u64, QuicError]
pub fn write_shared(mut e: Endpoint, c: ConnectionHandle, s: StreamId, data: Shared[Bytes], fin: bool) -> Result[(), QuicError]
pub fn read(mut e: Endpoint, c: ConnectionHandle, s: StreamId, mut into: Bytes) -> Result[(u64, bool), QuicError]
pub fn reset(mut e: Endpoint, c: ConnectionHandle, s: StreamId, code: u64, reliable: u64) -> Result[(), QuicError]
pub fn stop(mut e: Endpoint, c: ConnectionHandle, s: StreamId, code: u64) -> Result[(), QuicError]
pub fn send_datagram(mut e: Endpoint, c: ConnectionHandle, data: Shared[Bytes]) -> Result[(), QuicError]
pub fn priority(mut e: Endpoint, c: ConnectionHandle, s: StreamId, urgency: u8, order: u64) -> Result[(), QuicError]
pub fn close(mut e: Endpoint, c: ConnectionHandle, code: u64, reason: Bytes)
pub fn peer_pin(e: Endpoint, c: ConnectionHandle) -> Option[Pin]
```

One `Endpoint` per socket or per group member, as quinn-proto's `Endpoint` is "the protocol
state for a single socket". It is sans-IO: the caller passes each received datagram and each
timeout to `receive`, calls `poll` to get datagrams into `out` (a GSO train is one
`Transmit` with `segment` set, NET-16) and the next deadline, and runs the socket. The
package never calls the host. A `Connection` is reached only through its handle, which the
endpoint looks up from a datagram's destination CID.

`write_shared` queues a `Shared[Bytes]` on a stream without copying it, and
`send_datagram` takes one too: a relay that fans one object out to many connections (MoQ,
cascade) holds one copy, and each connection copies only into its own sealed packet, which
TLS-8 `seal_into` does in the cipher.

`priority` sets a stream's urgency (0 to 7, lower first) and its order within that urgency
(lower first); among equal priorities streams take turns, as
[RFC 9218](https://www.rfc-editor.org/rfc/rfc9218)'s `u` and `i` do. MoQ's scheduling
(moq-transport-23 §5.1.2) maps onto it (MOQ-5).

## QUIC-2: Packets and frames

The codec reads and writes RFC 9000 §17 long and short headers, §16 variable-length integers
and every frame type of §19, plus DATAGRAM (RFC 9221 §4, types `0x30` and `0x31`) and
RESET_STREAM_AT (type `0x24`). Reading rules:

- A varint uses the shortest or any longer encoding on read (RFC 9000 §16) and the shortest
  on write. A frame type is read as a varint and must use its minimal encoding (§12.4).
- A frame type not allowed in its packet type (RFC 9000 Table 3) is
  `PROTOCOL_VIOLATION`. A frame of unknown type is `FRAME_ENCODING_ERROR`.
- A packet whose header protection or AEAD fails to remove is dropped and counted, never
  answered. Packet numbers are decoded by RFC 9000 Appendix A.3 against the space's largest
  acknowledged-or-received number.
- Coalesced packets (§12.2) in one datagram are each processed; a packet that cannot be
  removed does not stop the next.
- The fixed bit must be 1 on receipt, and is always written 1 (no RFC 9287 greasing).

Header protection, packet protection, nonces and key phases are TLS-8's `Quic` profile,
`quic_initial`, `header_mask` and `key_update`; this chapter never touches key bytes.

## QUIC-3: The TLS 1.3 handshake

QUIC carries TLS 1.3 handshake messages in CRYPTO frames (RFC 9001 §4), so the handshake must
run in the package: TLS-9's `tls.accept` and `tls.connect` drive a TCP socket and cannot
serve QUIC. The package holds a sans-IO TLS 1.3 handshake, module `quic.handshake`, with only
what QUIC needs:

- **Messages.** ClientHello, ServerHello, EncryptedExtensions, Certificate, CertificateVerify,
  Finished, and CertificateRequest for mutual authentication
  ([RFC 8446](https://www.rfc-editor.org/rfc/rfc8446) §4). No HelloRetryRequest from the
  server: it supports both groups and picks one the client sent a share for; a client that
  sent no usable share is refused with `handshake_failure`. A client role does accept
  HelloRetryRequest (§4.1.4). No NewSessionTicket is sent, and one received is ignored.
- **Extensions.** `supported_versions` (TLS 1.3 only), `key_share`, `supported_groups`,
  `signature_algorithms` (`ecdsa_secp256r1_sha256`, `ed25519`), `server_name`, ALPN
  ([RFC 7301](https://www.rfc-editor.org/rfc/rfc7301), required by RFC 9001 §8.1: no ALPN
  match is `no_application_protocol`, QUIC error `0x0178`), and `quic_transport_parameters`
  (`0x39`, RFC 9001 §8.2). An unknown extension in a ClientHello is ignored; one the client
  did not offer in a server message is `unsupported_extension`.
- **Keys.** `secret.key_share` makes the share. `schedule.tls13(Quic(suite), side, share,
  peer, hello_hash)` gives the Handshake space; `schedule.application(finished_hash)` the 1-RTT
  space; `schedule.finished` and `verify_finished` the Finished values. Initial keys are
  `schedule.quic_initial(side, dcid)`. The handshake never sees a key byte.
- **Signing.** The server signs the CertificateVerify content (RFC 8446 §4.4.3) with
  `secret.sign(key, alg)`, whose `key` is a `Secret` with a `sign(p256)` or `sign(ed25519)`
  use: minted with `secret.p256_key`, or a file-sourced key granted those uses (TLS-5).
- **Verifying.** The peer's CertificateVerify is checked with `std.p256.verify` or
  `std.ed25519.verify` against the leaf's subject public key (the two functions `std.jwt`
  already uses), and the leaf's SPKI digest (SHA-256 of the DER `SubjectPublicKeyInfo`) must
  equal the `Pin` the caller gave. There is no chain verification and no name check: the pin
  is the authentication, as WebRTC's DTLS fingerprint is (RFC 8827) and as WebTransport's
  `serverCertificateHashes` is in browsers. For WebTransport the server's certificate must
  then be ECDSA P-256 with a validity of at most 14 days, the W3C rule for hashed
  certificates; `std.x509_build.self_signed_p256` makes one.
- **Transport parameters** (RFC 9000 §18) are written and checked in full: a duplicate, a
  server-only parameter from a client, a bad `initial_source_connection_id` or
  `original_destination_connection_id` (§7.3), or `max_udp_payload_size` below 1,200 is
  `TRANSPORT_PARAMETER_ERROR`.

This module could later serve DTLS 1.3; it is written over `Profile13`, not over QUIC
framing, so a DTLS 1.3 record layer could drive it. That is not a Wave 2 deliverable.

## QUIC-4: Connections, CIDs and address validation

- **CIDs.** A server issues every CID in NET-16's `Steer.Quic` format: one first octet, the
  owning member's index as big-endian `u16`, then random bytes to `cid_len` (8 by default).
  So a group bound with `steer: Quic` hands each datagram to the member that owns its
  connection, and one connection's datagrams never cross units. A connection keeps at most
  `active_connection_id_limit` (8) CIDs issued, and retires as RFC 9000 §5.1.2 says.
- **Stateless reset.** The token of CID `c` is the first 16 bytes of
  `HMAC-SHA256(reset_key, c)` (RFC 9000 §10.3.2's recommended construction), through
  `secret.hmac_sha256` with a `mac` use. A datagram that matches no connection and is at least
  41 bytes (§10.3) gets a stateless reset of a random length no longer than it, at most one
  per received datagram. A reset is never sent for a datagram that is itself 41 bytes or
  shorter, so two endpoints cannot loop.
- **Anti-amplification.** Before a path is validated the server sends at most three times the
  bytes it received on it (§8.1), counted per path, including padding and coalesced packets.
  Law Q6 checks this over every prefix of a simulated handshake.
- **Retry.** `RetryPolicy` decides when the server answers an Initial with Retry: never,
  always, or above a count of unvalidated connections in progress. The Retry token is
  sealed with `secret.aead_seal` (TLS-5, `aead(chacha20-poly1305)`, under `token_key`) over
  the client address, the original DCID and the issue time, and is valid for 10 s. The
  Retry Integrity Tag (RFC 9001 §5.8) is AES-128-GCM under the fixed key the RFC publishes;
  TLS-8 gains one operation for it (slice Q0).
- **NEW_TOKEN.** A validated server connection may send one token for future connections,
  of the same construction with a lifetime of 24 hours. Since there is no 0-RTT, a token only
  saves the Retry round trip.
- **Path validation and rebinding.** A non-probing packet from a new address on an
  established connection starts PATH_CHALLENGE on it (§9.3); data goes to the new path only
  after PATH_RESPONSE, and the congestion state is reset for it unless only the port
  changed (§9.4). The anti-amplification limit applies to the new path until it is
  validated.
- **Limits.** `connections` bounds live connections per endpoint, `unvalidated` bounds
  connections still in the handshake. A new Initial past either is answered with a
  CONNECTION_CLOSE of `CONNECTION_REFUSED` (§20.1) in an Initial packet and keeps no state.

## QUIC-5: Streams and flow control

Streams follow RFC 9000 §2 to §4 exactly: ids by initiator and direction, send and receive
state machines (§3), connection and stream credit, MAX_STREAMS, and the `*_BLOCKED` frames,
which the package sends when blocked and reads only as information. Received stream data is
kept in a range buffer bounded by the credit the endpoint gave, so a peer can never make it
buffer more than `initial_max_stream_data` per stream and `initial_max_data` per
connection. Exceeding either is `FLOW_CONTROL_ERROR`; opening past MAX_STREAMS is
`STREAM_LIMIT_ERROR`. Credit is raised when the application reads half the window, as
quinn-proto does (`ReceiveWindow` updates in connection/streams), and never grows without a
read.

RESET_STREAM_AT sets a reliable size: bytes below it are still delivered and retransmitted;
bytes past it are dropped. WebTransport resets its streams this way so the stream header is
never lost (webtrans-http3-17 §4.4).

## QUIC-6: Loss detection and congestion control

RFC 9002 as written, with its Appendix A and B pseudocode as the oracle:

- packet- and time-threshold loss (`kPacketThreshold` 3, `kTimeThreshold` 9/8), PTO with
  exponential backoff, `kGranularity` 1 ms, `kInitialRtt` 333 ms;
- RTT estimation per §5 with `max_ack_delay` from the peer's transport parameters;
- persistent congestion (§7.6) resets the window to the minimum;
- the controller is `NewReno` (RFC 9002 §7) or `Cubic` (RFC 9438 with HyStart++, RFC 9406),
  `Cubic` by default, as quinn (quinn-proto/src/congestion/cubic.rs) and quiche
  (quiche/src/recovery/congestion/cubic.rs, hystart.rs) default to it;
- a pacer spreads each congestion window over the smoothed RTT with a burst of 10 packets, as
  RFC 9002 §7.7 recommends; `poll` returns the next send time as `Wake` when paced;
- DATAGRAM frames are ack-eliciting and congestion-controlled but never retransmitted (RFC
  9221 §5.2); a datagram that cannot be sent within its connection's window is dropped and
  counted, not queued past one RTT;
- ECN: counts from ACK_ECN validate the path (§13.4.2); a validated path that reports CE
  reacts once per round trip as a loss would.

## QUIC-7: Datagrams

A connection that agreed `max_datagram_frame_size` (RFC 9221 §3) may `send_datagram` a
payload no larger than the smaller of the peer's limit and what one packet holds after the
headers; a larger payload is `TooLarge(n)` and nothing is sent. Received datagrams are
delivered in arrival order as `Event.Datagram`, never reassembled and never acknowledged to
the application.

## QUIC-8: Timers, closing and idling

The idle timeout is the smaller of both endpoints' `max_idle_timeout` (§10.1), no shorter than
three PTOs. `close` sends CONNECTION_CLOSE and enters the closing state for three PTOs (§10.2),
answering any packet with at most one CONNECTION_CLOSE per received datagram and at no more
than the received bytes. Draining discards everything. All timers are deadlines the caller
learns from `poll`; nothing in the package waits.

## QUIC-9: Memory and bounds

Every per-connection table has a size fixed by `TransportConfig` at the connection's start,
charged then, so a connection's cost is exact like RTC-10's room cost:

- receive buffers: at most the credit given, `initial_max_data` per connection;
- send buffers: what the application has written and not yet seen acknowledged, bounded by
  the stream's credit plus the controller's window; `write` returns the bytes taken, and
  less than asked when the bound is reached (no hidden cap: the count says so);
- sent-packet records: at most the window in packets plus PTO probes;
- CIDs: `active_connection_id_limit` each way;
- the CRYPTO stream: 16 KiB per space, past which the handshake is `CRYPTO_BUFFER_EXCEEDED`.

## QUIC-10: The driver

`quic.serve` is the thin IO loop for the common case, so consumers do not each write one:

```hirsel unbuilt
pub fn drive(mut e: Endpoint, clock: Clock, mut sock: UdpSocket, until: u64, mut events: List[Event]) -> Result[(), NetError] waits
```

It does one `udp_recv` batch until the endpoint's next deadline or `until`, passes each datagram
and the time to `receive`, then `poll`s and sends the whole batch with one `udp_send`. A
service with many cores binds a group with `steer: Quic` and runs one endpoint per member,
each in its own unit, which is how an SFU unit owns its connections (QUIC-4).

## Laws

Each is red first in `core/programs/quic/laws.hirsel` <!-- doc-ref: planned --> unless named, with its oracle outside
the package. RFC 9001's Appendix A vectors are TLS-8's PC1 and are not repeated here.

- **Q1 `varint_and_packet_number_vectors`.** RFC 9000 Appendix A.1's varints
  (`c2197c5eff14e88c` is 151,288,809,941,952,652; `9d7f3e7d` is 494,878,333; `7bbd` is
  15,293; `25` and `4025` are both 37) and A.3's packet number example (largest
  `0xa82f30ea`, truncated `0x9b32` decodes to `0xa82f9b32`). *Oracle:* the RFC's text.
- **Q2 `rfc9001_client_initial_parses`.** RFC 9001 A.2's protected client Initial is opened
  and parsed: its CRYPTO frame's ClientHello, its transport parameters and its padding, with
  the fields written by hand from A.2's listing. A.3's server Initial and A.4's Retry
  likewise; A.4's integrity tag verifies and a one-bit change does not.
- **Q3 `frame_refusals`.** One packet per refusal of QUIC-2 and QUIC-5: a frame type in the
  wrong packet type (each row of RFC 9000 Table 3 that forbids one), a non-minimal frame
  type, a STREAM frame past the stream's credit, an ACK of an unsent packet number, a
  RESET_STREAM_AT with a reliable size past the final size. *Oracle:* the error code each RFC
  section names.
- **Q4 `handshake_against_quinn_and_quiche`.** Over loopback UDP, the Hirsel server accepts
  quinn's and quiche's example clients (quinn `examples/client.rs`, quiche
  `apps/src/bin/quiche-client.rs`) for each suite and group, and the Hirsel client connects
  to their servers, pinned. Each side transfers 10 MiB on one stream and 1,000 datagrams.
  *Oracle:* the other implementation's view: its handshake completes, its received bytes
  hash to the sender's SHA-256, and its reported ALPN and transport parameters equal ours.
- **Q5 `interop_runner_cases`.** The QUIC Interop Runner's server and client test cases that
  are in scope (`handshake`, `transfer`, `retry`, `keyupdate`, `chacha20`, `multiconnect`,
  `rebind-port`, `rebind-addr`, `ecn`, `amplificationlimit`, `blackhole`, `handshakeloss`,
  `transferloss`, `handshakecorruption`, `transfercorruption`) against quinn, quiche and
  msquic. Nightly on Boat. *Oracle:* the runner's own checks on its pcap.
- **Q6 `amplification_never_exceeds_three`.** Under `sim.udp`, a client's Initial reaches a
  server whose replies are all lost and whose certificate chain needs 4 KiB. For every prefix
  of the trace, the server's bytes sent to the unvalidated address are at most 3 × the bytes
  received from it. *Oracle:* RFC 9000 §8.1, as a check over the trace.
- **Q7 `retry_validates_address`.** With `RetryPolicy.Always`, a spoofed Initial gets one
  Retry and no state is kept (a seam counter); the real client's second Initial is accepted;
  a token replayed from a different address, or after 10 s, is refused with
  `INVALID_TOKEN`. *Oracle:* RFC 9000 §8.1.2 and §17.2.5, by hand.
- **Q8 `stateless_reset_closes_and_never_loops`.** A server restarted with the same
  `reset_key` answers a short-header packet of a lost connection with a reset the client
  recognizes; two endpoints that each reset the other stop after one exchange because each
  reset is shorter than what provoked it. *Oracle:* RFC 9000 §10.3, by hand.
- **Q9 `loss_recovery_matches_rfc9002`.** Scripted `sim.udp` traces (a single loss, a tail
  loss, three PTOs in a row, persistent congestion) against a hand trace of RFC 9002
  Appendix A and B's pseudocode: each packet's declared-lost time, each PTO time and the
  congestion window after each ACK. *Oracle:* the pseudocode, worked by hand in the law's
  comments.
- **Q10 `cubic_matches_rfc9438`.** A scripted ACK sequence through a loss: `W_max`, `K` and
  `W_cubic(t)` at chosen `t`, and HyStart++'s exit round. *Oracle:* RFC 9438 §4 and RFC 9406
  §4's formulas, by hand.
- **Q11 `flow_control_bounds_memory`.** A peer that sends at full credit and a reader that
  reads nothing: the receiver's charge never passes `initial_max_data` plus its fixed
  connection cost, and a frame past credit is `FLOW_CONTROL_ERROR`. *Oracle:* QUIC-9.
- **Q12 `rebind_validates_new_path`.** Under `sim.udp`, the client's NAT rebinds its port
  and then its address mid-transfer. *Oracle:* RFC 9000 §9.3. No stream data goes to the
  new address before its PATH_RESPONSE, the transfer completes with the same hash, and the
  controller resets only on the address change.
- **Q13 `key_update_both_directions`.** Each side initiates two key updates during a
  transfer; packets in flight across the update are still opened (one previous key kept for
  3 PTOs). *Oracle:* RFC 9001 §6, and quinn as the peer.
- **Q14 `steer_reaches_owner`.** A group of 4 members with `steer: Quic`: every packet of a
  connection, long and short header, after CID rotation and after rebinding, reaches the
  member that issued the CID. *Oracle:* NET-16's steering vectors and the CID format.
- **Q15 `datagram_never_retransmitted`.** Under 5 % loss, 10,000 datagrams: every one that
  arrives arrives once, none is sent twice (a seam counter), and they consume congestion
  window. *Oracle:* RFC 9221 §5.2.
- **Q16 `pin_mismatch_refuses`.** A server whose leaf SPKI differs from the pin by one bit is
  refused before any stream opens, with `Refused(Pin)` and TLS alert `bad_certificate`
  (QUIC error `0x012a`). *Oracle:* RFC 8446 §6.2 and RFC 9001 §4.8.

**Cost target** (S9-style, measured under host matched-comparison rules): bulk transfer of
one stream on loopback at ≥ 50 % of quinn's throughput per core with the same cipher, GSO
on both, on the same CPU; 0 allocations per packet in steady state. The PLAN's 8 Gbps/core
is the long-run bar; the first build is held to the quinn ratio, which is a matched
comparison.

## Build slices

| Slice | Content | Depends on | Laws |
| --- | --- | --- | --- |
| **Q0** TLS-8 Retry tag | `schedule.retry_tag(odcid, packet, mut out)`: AES-128-GCM under RFC 9001 §5.8's published key and nonce, no `Secret` (the key is public) | — | Q2's A.4 rows, in TLS-8's PC1 |
| **Q1** Codec | varints, headers, every frame, coalescing, packet-number decoding | — | Q1, Q3 (codec rows) |
| **Q2** Handshake | `quic.handshake`: TLS 1.3 messages and extensions, transport parameters, pins, signing and verifying | Q1, Q0 | Q2, Q16 |
| **Q3** Connection core | Initial, Handshake and 1-RTT spaces, CIDs, ACKs, CONNECTION_CLOSE, idle, the `Endpoint` | Q2 | Q3, Q4 (handshake only) |
| **Q4** Streams | stream states, flow control, RESET_STREAM_AT, priority | Q3 | Q4, Q11 |
| **Q5** Recovery | RFC 9002, NewReno, CUBIC with HyStart++, pacing, ECN | Q3 | Q9, Q10 |
| **Q6** Validation and resets | anti-amplification, Retry, NEW_TOKEN, stateless reset, path validation, key update | Q3, Q5 | Q6, Q7, Q8, Q12, Q13 |
| **Q7** Datagrams, driver, steering | RFC 9221, `drive`, GSO trains, `Steer.Quic` CIDs | Q4 | Q14, Q15 |
| **Q8** Interop and cost | the interop runner on Boat; the cost bench | Q6, Q7 | Q5, cost target |

Reds that may ride: Q5 and the cost target. Q6, Q7, Q8, Q11 and Q16 are soundness laws and
block their slices.

## Prior art

Goose (/tmp/ref-goose at `d5ae5bb`) has no UDP or QUIC code. References: quinn
(/tmp/ref-quinn at `75d08d9`), quiche (/tmp/ref-quiche at `ac43c0d`), msquic
(/tmp/ref-msquic at `aa31c3f`), moq-quic, moq-dev's fork of quinn-proto
(/tmp/ref-moq at `d791d54`, rs/moq-quic).

| Source | Mechanism | Verdict |
| --- | --- | --- |
| Goose | networking as library code over explicit costs | **ADOPTED**, as for every media package |
| quinn-proto | sans-IO `Endpoint` per socket dispatching to `Connection`s; `poll_transmit`, `handle_event`, `handle_timeout` (quinn-proto/src/endpoint.rs:102, connection/mod.rs:466, 1154, 1226) | **ADAPTED** as `receive` and `poll`; the endpoint also returns events in one list, as RTC-5's `turn` does |
| quinn-proto | pluggable crypto session over rustls (quinn-proto/src/crypto/rustls.rs) | **REPLACED** by `quic.handshake` over TLS-8: keys never leave a schedule |
| quinn-proto, quiche | CUBIC default with HyStart++ (quinn-proto/src/congestion/cubic.rs; quiche/src/recovery/congestion/cubic.rs, hystart.rs) | **ADOPTED** |
| quiche | BBR variants (quiche/src/recovery/gcongestion) | **NOT TAKEN** in Wave 2 (O2) |
| msquic | per-core partitions owning connections, with the RSS hash or CID picking the partition (src/core/partition.c, binding.c) | **ADOPTED** as NET-16's `Steer.Quic` CIDs and one endpoint per unit |
| msquic | Retry when handshake memory passes a fraction (`RetryMemoryLimit`, src/core/binding.c:1285, settings.c:45) | **ADAPTED** to `RetryPolicy.Above(n)`, a count of unvalidated connections, which is exact rather than a memory fraction |
| moq-quic | quinn-proto fork with a deterministic simulator (rs/moq-quic/src/sim) | **ADOPTED** in spirit: `sim.udp` drives every loss and migration law |
| quinn, quiche, msquic | 0-RTT and session tickets | **NOT TAKEN** (Scope) |
| quiche | built-in HTTP/3 in the transport crate (quiche/src/h3) | **REPLACED** by a separate `webtransport` package over `quic`'s streams |

## Open for design review

- **O1** Pin-only authentication. Is SPKI pinning enough for every Wave 2 consumer (browser
  WebTransport via `serverCertificateHashes`, cascade between our own nodes), or does a
  consumer need Web PKI server certificates in the client role in Wave 2?
- **O2** BBRv3 or CUBIC for media. CUBIC fills buffers; cascade and MoQ carry live media,
  where BBR's lower queueing helps. Is CUBIC the right first controller, and should a
  delay-based controller (GCC over QUIC, as moq-congestion's `gcc`) come before BBR?
- **O3** No 0-RTT. Correct for WebTransport; is there any cascade or MoQ case where the
  handshake round trip matters enough to add resumption and a PSK schedule to TLS-8?
- **O4** A Hirsel X.509 chain verifier. Without one the client role cannot talk to public
  QUIC servers. Is that a Wave 2 need, or deferred?
- **O5** RESET_STREAM_AT is still a draft. Track its final codepoint in the build, or hold
  WebTransport until it is an RFC?
- **O6** The handshake module's home: inside `quic`, or its own `tls13` package that a later
  DTLS 1.3 also uses?
