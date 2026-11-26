# QUIC

*Specified 2026-11-26 (sfu-pool-media-w2-spec-2888, Horizon H3 part 2, Media Wave 2 package 1
of 4), with design review media-w2-spec-fix-2918. QS0 to QS7 implement the endpoint
and UDP driver. QS8 adds the interop runner and cost benchmark. Rules QUIC-1 to QUIC-10
and laws Q1 to Q18 apply.*

`quic` is a sans-IO QUIC version 1 endpoint, written in Hirsel under `core/programs/quic`.
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
| Handshake | TLS 1.3 in Hirsel over TLS-8 (QUIC-3); X25519 and P-256; the three TLS-8 suites; server signature P-256 ECDSA or Ed25519 | 0-RTT and resumption: TLS-8 has no PSK schedule, 0-RTT replay is a cost every consumer here would refuse; RSA server keys: `secret.sign`'s `rs256` is PKCS#1 v1.5, which TLS 1.3 forbids for CertificateVerify |
| Peer authentication | server pinned by certificate or SPKI digest, any of a list (client role); client certificates required and pinned the same way when the server sets `client_pins` (mutual, for cascade) | Web PKI chain verification in the client role: no Hirsel chain verifier outside `tls.connect`'s provider; ruled out for Wave 2 (Design review, O4) |
| Address validation | Retry (RFC 9000 §8.1.2) with the RFC 9001 §5.8 integrity tag; NEW_TOKEN tokens; the 3× anti-amplification limit | — |
| Connection IDs | NET-16 `Steer.Quic` format, so a `udp_bind_group` steers to the owning member; NEW_CONNECTION_ID, RETIRE_CONNECTION_ID; stateless reset | QUIC-LB encrypted CIDs ([draft-ietf-quic-load-balancers](https://datatracker.ietf.org/doc/draft-ietf-quic-load-balancers/)): one host, no external load balancer |
| Migration | passive: path validation and NAT rebinding on the server; the client keeps its path | active client migration and `preferred_address`: no consumer needs them |
| Streams | bidirectional and unidirectional, stream and connection flow control, RESET_STREAM, STOP_SENDING, RESET_STREAM_AT ([draft-ietf-quic-reliable-stream-reset](https://datatracker.ietf.org/doc/draft-ietf-quic-reliable-stream-reset/)) | — (WebTransport requires RESET_STREAM_AT, webtrans-http3-17 §3.1) |
| Datagrams | RFC 9221 DATAGRAM frames, never retransmitted, counted for congestion control | — |
| Recovery | RFC 9002 loss detection, PTO, ACK frequency of every second ack-eliciting packet, `max_ack_delay` 25 ms; NewReno and CUBIC with HyStart++ ([RFC 9406](https://www.rfc-editor.org/rfc/rfc9406)) behind the `Controller` seam; pacing | BBRv3: still an Internet-Draft; later, behind the same seam (Design review, O2) |
| Path MTU | 1,200 bytes base; DPLPMTUD ([RFC 8899](https://www.rfc-editor.org/rfc/rfc8899)) probes up to the lesser of `EndpointConfig.max_mtu` and the peer's `max_udp_payload_size`, never the socket's `slot_bytes` (65,792 under `UDP_GRO`), and re-probes after `PMTU_RAISE_TIMER` (600 s, §5.1.1) when converged below the ceiling; packets are built to the size the path has acknowledged (QUIC-1, **Path MTU**) | — |
| ECN | RFC 9000 §13.4 validation from NET-16's `ecn` field; CE reaction as loss | L4S: deferred with SCReAM (C3) |
| Key update | RFC 9001 §6 over TLS-8 `key_update`, with a non-consuming next-phase open (QS0) and the §6.6 AEAD limits | — |
| Multipath | — | draft-ietf-quic-multipath: TLS-8 has the `QuicMultipath` profile, but no consumer needs it in Wave 2 |
| Batching | NET-16 GSO trains: equal-length packets to one peer in one `udp_send` | — |

## QUIC-1: Shape

```hirsel signatures quic/quic
pub opaque type Endpoint
pub opaque type Connection
pub type ConnectionHandle: key = ConnectionHandle(u32)
pub type StreamId: key = StreamId(u64)
pub type EndpointConfig {
    member: u16,
    members: u16,
    cid_len: u8,
    max_mtu: u16,
    retry: RetryPolicy,
    reset_key: Secret,
    token_key: Secret,
    limits: EndpointLimits
}
pub type EndpointLimits {
    connections: u32,
    unvalidated: u32,
    datagram_bytes: u32
}
pub type TransportConfig {
    max_idle_ms: u32,
    initial_max_data: u64,
    initial_max_stream_data: u64,
    max_streams_bidi: u64,
    max_streams_uni: u64,
    max_datagram_frame: u16,
    send_bytes: u64,
    max_gaps: u16,
    cc: Controller,
    alpn: List[String],
    client_pins: Option[List[handshake.Pin]]
}
pub type Controller = NewReno | Cubic
pub type RetryPolicy = Never | Always | Above(u32)
pub type Priority: key {
    urgency: u8,
    order: u64,
    then: u64
}
pub type DatagramTag: key {
    owner: u64,
    tag: u64
}
pub opaque type PayloadSlice: send + cross
pub opaque type FramedSlice: send + cross
pub const FRAMED_PREFIX_MAX: u64 = 64
pub type PeerParams {
    max_idle_ms: u64,
    max_udp_payload: u64,
    max_ack_delay_ms: u64,
    max_datagram_frame: u64,
    reset_stream_at: bool,
    active_cid_limit: u64
}

pub type Output = Transmit(to: Addr, at: u64, len: u64, ecn: u8, send_at: u64) | Wake(at: u64)
pub type Event = Connected(c: ConnectionHandle, alpn: String) | Opened(ConnectionHandle, StreamId) | Readable(ConnectionHandle, StreamId) | Writable(ConnectionHandle, StreamId) | Acked(ConnectionHandle, StreamId) | Reset(c: ConnectionHandle, s: StreamId, code: u64, final_size: u64) | Stopped(ConnectionHandle, StreamId, u64) | Datagram(ConnectionHandle, PayloadSlice) | Closed(ConnectionHandle, CloseReason)
pub type CloseReason = Transport(code: u64, frame: u64) | Application(code: u64) | Idle | Reset | Refused(Refusal)
pub type Refusal = Version | Alpn | Pin | Limit(u32) | Token | Certificate
pub type QuicError = Config | Limit | Handshake(handshake.Error) | UnknownConnection | UnknownStream | StreamState | StreamReset(u64) | TooLarge(u64) | Unsupported

pub fn endpoint(r: Random, take c: EndpointConfig, take server: Option[(handshake.Identity, TransportConfig)]) -> Result[Endpoint, QuicError]
pub fn connect(mut e: Endpoint, now: u64, to: Addr, take name: String, take pins: List[handshake.Pin], take identity: Option[handshake.Identity], take t: TransportConfig) -> Result[ConnectionHandle, QuicError]
pub fn receive_datagram(mut e: Endpoint, now: u64, from: Addr, ecn: u8, mut buf: Bytes, at: u64, len: u64, mut events: List[Event])
pub fn timeout(mut e: Endpoint, now: u64, mut events: List[Event])
pub fn poll(mut e: Endpoint, now: u64, mut out: Bytes, mut tx: List[Output]) -> Option[u64]
pub fn committed(mut e: Endpoint, now: u64, k: u64)
pub fn open(mut e: Endpoint, c: ConnectionHandle, bidi: bool) -> Result[StreamId, QuicError]
pub fn write(mut e: Endpoint, c: ConnectionHandle, s: StreamId, data: Bytes, fin: bool) -> Result[u64, QuicError]
pub fn write_shared(mut e: Endpoint, c: ConnectionHandle, s: StreamId, take data: PayloadSlice, fin: bool) -> Result[bool, QuicError]
pub fn read(mut e: Endpoint, c: ConnectionHandle, s: StreamId, mut into: Bytes) -> Result[(u64, bool), QuicError]
pub fn received(e: Endpoint, c: ConnectionHandle, s: StreamId) -> u64
pub fn reset(mut e: Endpoint, c: ConnectionHandle, s: StreamId, code: u64, reliable: u64) -> Result[(), QuicError]
pub fn stop(mut e: Endpoint, c: ConnectionHandle, s: StreamId, code: u64) -> Result[(), QuicError]
pub fn send_datagram(mut e: Endpoint, c: ConnectionHandle, take data: FramedSlice, p: Priority, tag: DatagramTag) -> Result[(), QuicError]
pub fn drop_datagrams(mut e: Endpoint, c: ConnectionHandle, tag: DatagramTag) -> u64
pub fn payload_bytes_copied(e: Endpoint, c: ConnectionHandle) -> u64
pub fn priority(mut e: Endpoint, c: ConnectionHandle, s: StreamId, p: Priority) -> Result[(), QuicError]
pub fn close(mut e: Endpoint, id: ConnectionHandle, code: u64, take reason: Bytes)
pub fn set_identity(mut e: Endpoint, take id: handshake.Identity)
pub fn set_client_pins(mut e: Endpoint, take pins: Option[List[handshake.Pin]])
pub fn peer_pin(e: Endpoint, id: ConnectionHandle) -> Option[handshake.Pin]
pub fn peer_params(e: Endpoint, id: ConnectionHandle) -> Option[PeerParams]
pub fn pto_ns(e: Endpoint, id: ConnectionHandle) -> u64
```

TLS identities and pins are declared in `quic.handshake`.

```hirsel signatures quic/handshake
pub type Identity = Identity(chain: List[Bytes], key: Secret, alg: SignAlg)
pub type Pin = Spki(Inline[u8, 32]) | Cert(Inline[u8, 32])
```

One `Endpoint` per socket or per group member, as quinn-proto's `Endpoint` is "the protocol
state for a single socket". It is sans-IO and the package never calls the host:

- **Receive.** The caller passes each received datagram to `receive_datagram` as a borrowed
  slice `buf[at, at + len)` of its NET-16 receive buffer, with the `UdpPacket`'s `peer` and
  `ecn`; nothing is copied before the packet is opened in place. NET-16 does not report a
  datagram's local address, so none is asked for. A `UdpPacket` with `truncated` set is never
  passed in: the driver drops and counts it (QUIC-10). `timeout` runs the timers due at
  `now`.
- **Transmit.** `poll` writes datagrams into `out` and appends one `Transmit` per datagram,
  in order: `at` and `len` are the datagram's place in `out`, and `send_at` is the pacer's
  earliest send time, in `host.mono_ns` nanoseconds (QUIC-6). Consecutive equal-length
  `Transmit`s to one `to` are a NET-16 GSO train; the endpoint makes no other batching claim.
  `poll` returns the next deadline.
- **Commit.** `udp_send` may commit only a prefix of a batch (NET-16). The caller reports the
  committed count with `committed(e, now, k)`, and only then does the endpoint record the
  first `k` datagrams' packets as sent, start their loss timers and charge them to the
  congestion window and the anti-amplification budget. The packets of the uncommitted suffix
  are abandoned: their packet numbers are skipped (RFC 9000 §12.3 allows gaps), their
  frames go back to their send queues as if never sent, with no congestion response, and a
  frame whose stream was reset or whose datagram was dropped since (QUIC-5, QUIC-7) is not
  requeued. A `poll` with no `committed` before the next `poll` abandons the whole batch.
- **Path MTU.** Each connection builds packets to its validated path MTU, which starts at
  1,200 bytes (RFC 9000 §14, RFC 8899 `BASE_PLPMTU`) and is RFC 9002 §7.2's
  `max_datagram_size`. Once the handshake is confirmed and the path validated, `poll` adds
  at most one probe in flight: a 1-RTT PING padded to the size under test (RFC 9000 §14.3),
  sent only when the window and pacer allow its size. The first probe is the ceiling, the
  lesser of `max_mtu` and the peer's `max_udp_payload_size`; an acknowledged probe makes its
  size the validated one, and three losses of one size (RFC 8899 `MAX_PROBES`) refute it
  and every size above, after which the search halves the range between the validated size
  and the largest one not refuted, to the byte. When the search converges below the ceiling,
  RFC 8899 §5.1.1's `PMTU_RAISE_TIMER` (600 s) is armed and reported through `poll`'s next
  deadline; on expiry the search re-enters above the validated size without lowering it, and
  stops arming once the ceiling is reached. A lost probe is no congestion signal: it
  leaves the window alone and starts no persistent congestion period (RFC 9000 §14.4). A
  new peer address (§14.2.1) and persistent congestion while above 1,200 bytes (RFC 8899
  §4.3's black hole) both return the connection to 1,200 bytes and start the search again.
  Packing and `send_datagram` size a packet's frames by one rule: the validated size less
  the header `poll` writes (a 1-RTT header is 1 byte, the peer's CID and a 4-byte packet
  number) and the 16-byte AEAD tag.

A `Connection` is reached only through its handle, which the endpoint looks up from a
datagram's destination CID.

**Payload slices.** Payloads cross between `quic`, `webtransport`, `moq` and
`webrtc_sfu/cascade` as two opaque owned types (media-seams-design-3072, Performancesmash
#6), so a relay that fans one object out to many connections holds one copy and no layer
copies to add or strip a header:

```hirsel signatures quic/quic
pub fn payload_whole(take backing: Shared[Bytes]) -> PayloadSlice
pub fn payload_slice(take backing: Shared[Bytes], at: u64, len: u64) -> Option[PayloadSlice]
pub fn payload_share(s: PayloadSlice) -> PayloadSlice
pub fn payload_trim_front(take s: PayloadSlice, n: u64) -> Option[PayloadSlice]
pub fn payload_trim_range(take s: PayloadSlice, offset: u64, len: u64) -> Option[PayloadSlice]
pub fn payload_len(s: PayloadSlice) -> u64
pub fn payload_bytes(s: PayloadSlice) -> View[Bytes] in s
pub fn payload_retained_bytes(s: PayloadSlice) -> u64
pub fn framed_from(take payload: PayloadSlice) -> FramedSlice
pub fn framed_prepend(mut f: FramedSlice, header: Bytes) -> bool
pub fn framed_room(f: FramedSlice) -> u64
pub fn framed_len(f: FramedSlice) -> u64
pub fn framed_retained_bytes(f: FramedSlice) -> u64
```

- **Ranges.** A `PayloadSlice` is `len` bytes at `at` of a `Shared[Bytes]` backing. Every
  range is checked as `at <= size && len <= size - at`, never by forming `at + len`, and
  refused with `None`. `payload_trim_front` and `payload_trim_range` measure from the
  slice's current start, not the backing's. Both types are opaque, so no range or prefix
  state the constructors did not check exists, and both are `send + cross`.
- **Inline prefix.** A `FramedSlice` holds a payload and the framing headers written in
  front of it in a right-aligned 64-byte inline prefix, so each layer (`cascade`, `moq`,
  `webtransport`) prepends its header in O(header length) without shifting or copying the
  payload. `framed_prepend` writes the whole header or, when it is longer than the room left
  (`framed_room`), returns `false` and changes nothing; the caller then coalesces into one
  `Shared[Bytes]`. `framed_len` is the wire length, prefix plus payload.
- **Direct-to-slot TX.** `poll` writes a 1-RTT packet's short header into `out`, then its
  frames straight after it: a STREAM frame's data and a DATAGRAM frame's prefix and payload
  are read from the backing into the slot, and the packet is sealed in place (TLS-8
  `seal_into`). A packet that fails to seal is truncated off `out` and its frames requeued.
  Initial and Handshake packets and the MTU probe still assemble their frames in scratch and
  copy them into the slot when sealed; they carry no application payload.
  `payload_bytes_copied` counts the application payload bytes a connection copied while
  assembling packets, one count per copy, so a byte staged in scratch and then sealed
  counts twice; the cost bench reads it.
- **RX.** `receive_datagram` opens a 1-RTT packet in place and copies each DATAGRAM frame's
  payload once into an exact-length `Shared[Bytes]`, never retaining the caller's receive
  slab, and emits `Event.Datagram(c, PayloadSlice)` over the whole of it. Upper layers strip
  their headers with `payload_trim_front`.

The queues hold only owned handles: a slice's backing lives as long as some queue, packet
record or application value still holds it, and each holder is charged as QUIC-9 says.

`priority` sets a stream's `Priority`: its urgency (0 to 7), then its `order` within that
urgency, then `then` within equal orders, each lower first; among equal priorities streams
take turns, as [RFC 9218](https://www.rfc-editor.org/rfc/rfc9218)'s `u` and `i` do. A
datagram carries a `Priority` too and competes in the same schedule. MoQ's scheduling
(moq-transport-23 §5.1.2) maps onto the two 64-bit keys exactly (MOQ-5).

`Acked` reports that every byte of a stream's send side, and its FIN, has been
acknowledged (RFC 9000 §3.1's "Data Recvd"), which MoQ's subgroup delivery timeout
waits for (MOQ-5).

`received` is the largest offset received on a stream so far, read or not, which
WebTransport's session flow control counts on arrival (WT-5).

`set_identity` replaces the server's certificate chain and key for connections that start
after it; live connections keep the identity they were made with. It is how a
WebTransport server rotates its 14-day certificate (WT-7). `set_client_pins` likewise
replaces the pins new connections are checked against. `peer_params` gives the peer's
transport parameters once the handshake has them, and `pto_ns` the connection's current
probe timeout (RFC 9002 §6.2.1), which WebTransport's session drain uses.

## QUIC-2: Packets and frames

The codec reads and writes RFC 9000 §17 long and short headers, §16 variable-length integers
and every frame type of §19, plus DATAGRAM (RFC 9221 §4, types `0x30` and `0x31`) and
RESET_STREAM_AT (type `0x24`, [draft-ietf-quic-reliable-stream-reset-11](https://datatracker.ietf.org/doc/draft-ietf-quic-reliable-stream-reset/11/) §4). Reading rules:

- A varint uses the shortest or any longer encoding on read (RFC 9000 §16) and the shortest
  on write. A frame type is read as a varint and must use its minimal encoding (§12.4).
- A frame type not allowed in its packet type (RFC 9000 Table 3) is
  `PROTOCOL_VIOLATION`. A frame of unknown type is `FRAME_ENCODING_ERROR`. A DATAGRAM frame
  received when the endpoint did not advertise `max_datagram_frame_size` is
  `PROTOCOL_VIOLATION` (RFC 9221 §3).
- Packet numbers are decoded by RFC 9000 Appendix A.3 against the largest packet number
  received *and authenticated* in that space (§17.1), never against a sent or acknowledged
  number.
- The fixed bit must be 1 on receipt, and is always written 1 (no RFC 9287 greasing).

**The receive classifier.** `receive_datagram` takes each datagram through one order of
checks, and the first that applies decides. Every drop is counted by reason and never
answered unless the step says so.

1. Fewer than 21 bytes: dropped (§10.3: no valid packet is shorter).
2. Long header with version 0: a Version Negotiation packet. A server drops it. A client
   handles it only before any packet from the server was authenticated, and only if it does
   not list the client's version (§6.2).
3. Long header with an unsupported version: a server answers with one Version Negotiation
   packet only if the datagram is at least 1,200 bytes (§6.1: "servers MUST drop smaller
   packets that specify unsupported versions"), and otherwise drops it. A client drops it.
4. Long header Initial in a datagram shorter than 1,200 bytes, at a server: dropped (§14.1).
   The size is the datagram's, so coalesced packets count.
5. Destination CID matches no connection: for an Initial at a server, the admission path of
   QUIC-4 (limits, then Retry); for a short header, the stateless-reset path of QUIC-4;
   anything else is dropped.
6. Header protection: the 16-byte sample at packet-number offset + 4 (RFC 9001 §5.4.2) must
   lie inside the packet; a packet too short for it is dropped *before* TLS-8 `packet.mask`
   is called, since `mask` faults with `Bounds` on a short sample.
7. Packet protection: a packet that fails to open is dropped and counted against the key's
   integrity limit (QUIC-6), never answered.
8. Frames, in order. A connection error closes the connection; a packet with an error is not
   acknowledged.

Coalesced packets (§12.2) in one datagram go through steps 5 to 8 each; a packet that is
dropped does not stop the next.

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
- **Signing.** The CertificateVerify content (RFC 8446 §4.4.3) is signed with
  `secret.sign(key, alg, r, msg, out)`, whose `key` is a `Secret` with a `sign(p256)` or
  `sign(ed25519)` use (minted with `secret.p256_key`, or a file-sourced key granted those
  uses, TLS-5) and whose `r` is the endpoint's `Random`. `secret.sign` gives a P-256
  signature as 64 bytes `r ‖ s`; TLS 1.3's `ecdsa_secp256r1_sha256` carries an ASN.1 DER
  `ECDSA-Sig-Value` (RFC 8446 §4.2.3), so the handshake DER-encodes it (minimal integers, a
  leading zero only where the high bit is set) before writing it. Ed25519's 64 bytes go as
  they are.
- **Reading the peer's key.** `std.x509_key` (DTLS-7, slice S1 of the DTLS chapter) reads the
  leaf's `subjectPublicKeyInfo`. QS2 adds two things to it: an `Ed25519(key)` answer for
  `id-Ed25519` keys ([RFC 8410](https://www.rfc-editor.org/rfc/rfc8410) §4), and the byte
  range of the whole DER `SubjectPublicKeyInfo`, which `Pin.Spki` digests.
- **Verifying.** The peer's CertificateVerify is checked with core `p256.verify_der` (strict
  DER, the wire form) or `ed25519.verify` against that key. A signature that does not verify
  is `decrypt_error` (RFC 8446 §4.4.3).
- **Pins.** A `Pin` is a SHA-256 digest: `Cert` of the leaf certificate's whole DER, as
  WebRTC's DTLS fingerprint (RFC 8122) and the W3C `serverCertificateHashes` are; `Spki` of
  its DER `SubjectPublicKeyInfo`, which survives re-issuing a certificate on the same key.
  The leaf passes if it matches *any* pin in the list, so a client given `[old, new]` rides
  across a server's certificate rotation. There is no chain verification and no name check:
  the pin is the authentication. For browser WebTransport the server's certificate must be
  ECDSA P-256 with a validity of at most 14 days, the W3C rule for hashed certificates;
  `std.x509_build.self_signed_p256` makes one.
- **Client authentication.** A server whose `TransportConfig.client_pins` is `Some(pins)`
  sends CertificateRequest and requires a client certificate: an empty client Certificate is
  `certificate_required` (RFC 8446 §4.4.2.4, QUIC error `0x0174`), and a leaf matching no pin
  is `bad_certificate` (`0x012a`). The check is done when the client's CertificateVerify and
  Finished arrive, before the server emits `Event.Connected`, accepts any 1-RTT frame or
  sends HANDSHAKE_DONE. With `None` (WebTransport) no client certificate is asked for.
  Cascade sets it (CAS-2).
- **Transport parameters** (RFC 9000 §18) are written and checked in full: a duplicate, a
  server-only parameter from a client, a bad `initial_source_connection_id` or
  `original_destination_connection_id` (§7.3), or `max_udp_payload_size` below 1,200 is
  `TRANSPORT_PARAMETER_ERROR`. The endpoint sends `max_datagram_frame_size` (RFC 9221 §3)
  only when `TransportConfig.max_datagram_frame` is nonzero, `reset_stream_at` (`0x1d`,
  empty, reliable-stream-reset-11 §3) always, and `max_udp_payload_size` as `max_mtu`. A
  `reset_stream_at` with a non-empty value is `TRANSPORT_PARAMETER_ERROR`. The endpoint
  sends DATAGRAM frames only to a peer that advertised `max_datagram_frame_size`, and
  RESET_STREAM_AT only to one that advertised `reset_stream_at`.

This module could later serve DTLS 1.3; it is written over `Profile13`, not over QUIC
framing, so a DTLS 1.3 record layer could drive it. That is not a Wave 2 deliverable, and the
module stays inside `quic` until one exists (Design review, O6).

## QUIC-4: Connections, CIDs and address validation

- **CIDs.** A server issues every CID in NET-16's `Steer.Quic` format: one first octet, the
  owning member's index as big-endian `u16`, then random bytes to `cid_len` (8 by default).
  So a group bound with `steer: Quic` hands each datagram to the member that owns its
  connection, and one connection's datagrams never cross units. A connection keeps at most
  `active_connection_id_limit` (8) CIDs issued, and retires as RFC 9000 §5.1.2 says. Each
  end tops the peer up to the smaller of the two limits with NEW_CONNECTION_ID once the
  handshake is confirmed, never in 0.5-RTT data; a CID the peer retires stops routing at
  once, and releasing the connection unmaps every CID it issued.
- **Group keys.** Every member of a `udp_bind_group` is given the same `reset_key` and
  `token_key` (`secret.share`): a token or reset made by one member must be checked by
  whichever member a later datagram reaches, and a restarted member must recognize its
  predecessor's resets.
- **Stateless reset.** The token of CID `c` is the first 16 bytes of
  `HMAC-SHA256(reset_key, c)` (RFC 9000 §10.3.2's recommended construction), through
  `secret.hmac_sha256` with a `mac` use. A reset is sent only in answer to a *short-header*
  datagram that matches no connection and whose length `n` is greater than `21 + cid_len`
  (anything shorter cannot be a packet to one of our CIDs, §10.3), at most one per received
  datagram, and is itself a short-header packet (§10.3: "Endpoints MUST send Stateless Resets
  formatted as a packet with a short header"). Its length is `n − 1` when `n` is 43 bytes or
  less (§10.3's SHOULD), and otherwise uniformly random in `[43, n − 1]`, so it is always at
  least 21 bytes and always shorter than its trigger (§10.3.3). Two endpoints that reset
  each other therefore stop within `n − 21 − cid_len` exchanges. Long-header datagrams that
  match no connection are never answered with a reset.
- **Anti-amplification.** Before a path is validated the server sends at most three times the
  bytes it received on it (§8.1), counted per path at `committed`, including padding and
  coalesced packets. Law Q6 checks this over every prefix of a simulated handshake.
- **Tokens.** Retry and NEW_TOKEN tokens share one construction and are kept apart by a type
  octet. A token is `type ‖ salt ‖ sealed`:

  | Type | Use | Binds (in AAD and plaintext) | Lifetime |
  | --- | --- | --- | --- |
  | `0x01` | Retry (RFC 9000 §8.1.2) | client IP and port, original DCID, the Retry's SCID | 10 s |
  | `0x02` | NEW_TOKEN (§8.1.3) | client IP | 10 min |

  `salt` is 16 fresh bytes from the endpoint's `Random`. The sealing key is
  `secret.derive_hkdf_sha256(token_key, salt, "hirsel quic token" ‖ type, 32,
  "aead(chacha20-poly1305)")`, and the nonce is zero: each key seals exactly one token, so
  no nonce is ever reused across members, restarts or rotations, and no seal counter is
  needed. The plaintext is the bound fields and the issue time; a token whose type does not
  match where it arrived (a NEW_TOKEN token where a Retry token is expected, or the
  reverse), whose fields differ, or that has expired is refused: a Retry token with
  `INVALID_TOKEN` (§8.1.2), a NEW_TOKEN token by ignoring it and proceeding as if none was
  sent (§8.1.3). A NEW_TOKEN token can validate an Initial through 10 min after issue,
  including on another endpoint sharing the token key. After that it cannot lift the
  anti-amplification limit. Since there is no 0-RTT, it only saves the Retry round trip.
- **Retry.** `RetryPolicy` decides when the server answers an Initial with Retry: never,
  always, or above a count of unvalidated connections in progress. The default is
  `Above(unvalidated / 2)`; `endpoint` refuses `Above(n)` with `n ≥ unvalidated`, which
  could never trigger before the hard limit. The Retry Integrity Tag (RFC 9001 §5.8) is
  AES-128-GCM under the key and nonce the RFC publishes (slice QS0).
- **Path validation and rebinding.** A non-probing packet from a new address on an
  established connection starts PATH_CHALLENGE on it (§9.3); data goes to the new path only
  after PATH_RESPONSE, and the congestion state is reset for it unless only the port
  changed (§9.4). The anti-amplification limit applies to the new path until it is
  validated.
- **Limits.** `connections` bounds live connections per endpoint, `unvalidated` bounds
  connections still in the handshake. A new Initial past either is answered with a
  CONNECTION_CLOSE of `CONNECTION_REFUSED` (§20.1) in an Initial packet and keeps no state.
  `datagram_bytes` bounds the bytes of received DATAGRAM payloads held across all of the
  endpoint's connections until the application takes their `Event.Datagram`; past it, new
  datagrams are dropped and counted (RFC 9221 §5 lets a receiver drop them).

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

**Gaps.** A stream's receive buffer holds at most `max_gaps` (default 32) disjoint
out-of-order ranges. A STREAM frame that would make one more is dropped unacknowledged, as
if lost; the peer retransmits it. Without the cap, a peer could make a credit-bounded buffer
cost one range record per byte.

**Send memory.** Local send memory is bounded locally, never by the peer's credit (which may
be up to 2^62). `send_bytes` bounds the bytes a connection holds queued and unacknowledged
across all its streams. `write` takes at most what fits under the smaller of the stream's
credit and `send_bytes`, and returns the count (no hidden cap: the count says so).
`write_shared` takes the whole slice or none of it: it returns `Ok(true)` when queued and
`Ok(false)` when its charge would pass `send_bytes`, and the connection later emits
`Writable` for that stream once acknowledgements free room. The charges are QUIC-9's
retained-memory charges, not the payload length: `write` charges the bytes it copies
rounded up to 8 (the bytes it takes are capped so the rounded charge fits), and
`write_shared` charges `payload_retained_bytes` of the slice.

**Resets.** `reset(e, c, s, code, reliable)` sends RESET_STREAM (`reliable` 0) or
RESET_STREAM_AT, and at once discards every queued, sent-unacknowledged and retransmittable
byte of the stream at or past `reliable`, including bytes in a polled but not yet
committed suffix; bytes below `reliable` are still delivered and retransmitted. Nothing past
`reliable` is ever sent again. A peer's RESET_STREAM_AT sets a reliable size the same way on
receipt, and `Event.Reset` carries the code and the stream's final size. WebTransport resets
its streams this way so the stream header is never lost (webtrans-http3-17 §4.4), and MoQ's
revocation fence relies on the discard (MOQ-8).

RESET_STREAM_AT is advertised by the `reset_stream_at` transport parameter (`0x1d`) and uses
frame type `0x24`, the codepoints of draft-ietf-quic-reliable-stream-reset-11; the build
tracks that draft's codepoints until it is an RFC (Design review, O5). To a peer that did
not advertise it, `reset` with `reliable > 0` is `Unsupported`, never a silent RESET_STREAM.

## QUIC-6: Loss detection and congestion control

RFC 9002 as written, with its Appendix A and B pseudocode as the oracle:

- packet- and time-threshold loss (`kPacketThreshold` 3, `kTimeThreshold` 9/8), PTO with
  exponential backoff, `kGranularity` 1 ms, `kInitialRtt` 333 ms;
- RTT estimation per §5 with `max_ack_delay` from the peer's transport parameters;
- persistent congestion (§7.6) resets the window to the minimum;
- the controller is `NewReno` (RFC 9002 §7) or `Cubic` (RFC 9438 with HyStart++, RFC 9406),
  `Cubic` by default, as quinn (quinn-proto/src/congestion/cubic.rs) and quiche
  (quiche/src/recovery/congestion/cubic.rs) default to CUBIC; HyStart++ follows quiche
  (quiche/src/recovery/congestion/hystart.rs), since quinn has none. `Controller` is the
  seam a later BBRv3 or delay-based controller is added behind (Design review, O2);
- a pacer spreads each congestion window over the smoothed RTT with a burst of 10 packets, as
  RFC 9002 §7.7 recommends; each `Transmit` carries its `send_at`, and `poll` returns the
  next send time as `Wake` when paced;
- an endpoint tracks at most 64 received packet-number ranges per space for its ACK frames;
  past that the oldest range is forgotten (RFC 9000 §13.2.3 lets an endpoint limit the
  ranges it reports), and an ACK frame is written with at most 64 ranges;
- DATAGRAM frames are ack-eliciting and congestion-controlled but never retransmitted (RFC
  9221 §5.2); a datagram that cannot be sent within its connection's window is dropped and
  counted, not queued past one RTT;
- ECN: counts from ACK_ECN validate the path (§13.4.2); a validated path that reports CE
  reacts once per round trip as a loss would.

**Key update and AEAD limits** (RFC 9001 §6). The endpoint initiates a key update only once
the handshake is confirmed and a packet of the current phase has been acknowledged (§6.1).
A received packet with a flipped key phase is opened with the *next* receive key through
`schedule.next_opener(s)`; only if that packet authenticates does the endpoint
commit `schedule.key_update(s, false)` and answer with its own update. A forged packet with
a flipped phase bit therefore cannot advance or destroy the receive key. One previous
receive key is kept for three PTOs (§6.5). Each key counts packets sealed and packets that
failed to open:

| Limit | AES-128-GCM, AES-256-GCM | ChaCha20-Poly1305 | Action |
| --- | --- | --- | --- |
| Confidentiality (seals per key) | 2^23 (§6.6) | — (§6.6: larger than the packet-number space) | key update well before the limit (at 2^22); if one cannot be made, close with `AEAD_LIMIT_REACHED` (`0x0f`) |
| Integrity (failed opens per key) | 2^52 | 2^36 | close with `AEAD_LIMIT_REACHED` |

Initial keys are discarded when the first Handshake packet is sent or received (§4.9.1),
Handshake keys when the handshake is confirmed (§4.9.2), through `schedule.retire`. The
server sends HANDSHAKE_DONE once its handshake is complete (§4.1.2).

## QUIC-7: Datagrams

A connection that agreed `max_datagram_frame_size` (RFC 9221 §3) may `send_datagram` a
payload whose DATAGRAM frame fits both the peer's limit and one packet. RFC 9221 §3 counts
the limit over the whole frame, so the payload bound is the peer's limit less the frame type
(one byte) and the length varint (1 to 8 bytes); a payload whose frame does not fit one
empty 1-RTT packet at the validated path MTU (QUIC-1, **Path MTU**) is `TooLarge(n)` too,
and nothing is sent. The size checks use `framed_len`. A queued datagram is charged
`framed_retained_bytes(f)` (its backing's storage at its capacity rounded up to 8, plus
64 bytes for its `Shared` node) plus `4 × bytes[QueuedDatagram]` (648 bytes): its record
in each of the two lists it passes through, queued then staged, each grown to at most twice
its peak length (budgets.md's growth rule). The record stores the charge it was admitted
with; the charge moves with it from queued to staged and back when an uncommitted suffix is
requeued, and the stored amount is released exactly once, when its packet is committed or
it is dropped or expires. Past `send_bytes`, `send_datagram` is `Limit`. A datagram the window has not let out within one
smoothed RTT of the first `poll` that saw it is dropped and counted as `DatagramLate`.

Each queued datagram carries a `Priority`, which schedules it among the streams (QUIC-1),
and a `DatagramTag`: a structured `owner` (the caller's session generation, so one
session's purge never reaches another's datagrams) and the caller's `tag`.
`drop_datagrams(e, c, tag)` removes every queued datagram whose `DatagramTag` equals
`tag`, including those in a polled but uncommitted suffix, and returns how
many it removed; a datagram already committed to the socket is not recalled. MoQ tags
datagrams by track so a revocation fence can purge them (MOQ-8).

Received datagrams are delivered in arrival order as `Event.Datagram`, never reassembled and
never acknowledged to the application, within `EndpointLimits.datagram_bytes` (QUIC-4); one
that would pass it is dropped and counted as `DatagramBuffer`. A received datagram is
charged its payload length against `datagram_bytes`. `drive` takes the room the held
`Event.Datagram`s leave once per `udp_recv` batch, and each admitted datagram then takes its
length from it, so the refusal point is the one a scan per datagram would give.

## QUIC-8: Timers, closing and idling

The idle timeout follows RFC 9000 §10.1: it is the smaller of the two endpoints'
`max_idle_timeout` values when both are nonzero, the one nonzero value when only one is, and
disabled when both are zero; when enabled it is never shorter than three PTOs. `close` sends
CONNECTION_CLOSE and enters the closing state for three PTOs (§10.2), answering any packet
with at most one CONNECTION_CLOSE per received datagram and at no more than the received
bytes. Draining discards everything. All timers are deadlines the caller learns from `poll`;
nothing in the package waits.

## QUIC-9: Memory and bounds

Every per-connection table has a size fixed by `TransportConfig` at the connection's start,
charged then, so a connection's cost is exact like RTC-10's room cost:

- receive buffers: at most the credit given, `initial_max_data` per connection, in at most
  `max_gaps` ranges per stream;
- send buffers: at most `send_bytes` per connection, queued and unacknowledged together
  (QUIC-5), charged by what they keep alive (below);
- sent-packet records: at most the window in packets plus PTO probes;
- received ranges: 64 per space (QUIC-6);
- CIDs: `active_connection_id_limit` each way;
- the CRYPTO stream: 16 KiB per space, past which the handshake is `CRYPTO_BUFFER_EXCEEDED`.
  It is a denial-of-service bound, not a capacity setting: it caps what an unauthenticated
  peer can make the endpoint hold per encryption level before the handshake completes, both
  the bytes ahead of the read position and one reassembled handshake message.

Per endpoint, received datagrams are bounded by `EndpointLimits.datagram_bytes`.

**Retained memory.** Wire checks (path MTU, `max_datagram_frame_size`) use a slice's
length; memory caps charge what a holder keeps alive, by budgets.md's MEM-24 recipes, so a
short slice never hides an uncharged backing. A held slice is charged
`payload_retained_bytes`: its backing's storage at its capacity rounded up to 8, plus its
`Shared[Bytes]` node, round8(`bytes[Bytes]` + 40) = 64, whatever range it covers; aliases
are not netted, so each holder in a budget pays for the whole backing. A copied `write` is
charged its length rounded up to 8. Each queued piece and datagram stores the charge it was
admitted with, and each way it leaves settles exactly that charge:

- **ACK.** A copied piece is released as its bytes are acknowledged, and its rounding
  remainder when the whole piece is freed. A held slice is released whole only when the
  acknowledged floor passes its last byte, since its backing lives until then.
- **Reset and cancel.** `reset` and `drop_datagrams` release the stored charges of the
  pieces and datagrams they discard; a piece that straddles `reliable` is kept, and stays
  charged.
- **Commit, expiry and close.** A datagram's charge is released when its packet is
  committed, it expires or the connection closes. Closing a connection retires everything
  it holds.

## QUIC-10: The driver

`quic.drive` is the thin IO loop for the common case, so consumers do not each write one:

```hirsel signatures quic/quic
pub fn drive(mut e: Endpoint, clock: Clock, mut sock: UdpSocket, until: u64, mut events: List[Event]) -> Result[(), NetError] waits
```

It calls `timeout`, then `poll`s and hands the whole batch to one `udp_send` with
`UdpTx(to, at, len, ecn)` per `Transmit`, waiting until the first `send_at` that is in the
future. It passes the committed count `udp_send` returns to `committed`; the uncommitted
suffix is abandoned as QUIC-1 says and is rebuilt by the next `poll`. Then it does one
`udp_recv` batch until the endpoint's next deadline (taken after `committed`, which arms the
loss timers) or `until`, and passes the batch to the receive path, each datagram as a
slice of the receive buffer (the datagram room is scanned once per batch, QUIC-7), dropping and counting any `UdpPacket` with `truncated` set as `Truncated`.
Sending first means data written between calls leaves on the next call, not after a
receive wait. Packets that fill to the limit are padded to it, so a run of full packets
reaches the host as equal lengths and leaves as one GSO train on Linux; on Darwin the host
sends them one by one. A service with many cores binds a group with `steer: Quic` and runs one endpoint per
member, each in its own unit, which is how an SFU unit owns its connections (QUIC-4).

`endpoint` refuses a `max_mtu` below 1,200. Under `UDP_GRO` `slot_bytes` is the coalesced
buffer (up to 65,792), not a packet size, which is why the MTU is its own setting; NET-16
gives no query for a socket's `slot_bytes`, so `drive` cannot check it and a too-small slot
shows as `Truncated` drops.

## Laws

Laws live in `core/programs/quic/laws.hirsel` and the adjacent `*_laws.hirsel` files, with each oracle outside
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
  RESET_STREAM_AT with a reliable size past the final size, a non-empty `reset_stream_at`
  parameter, a DATAGRAM frame on a connection that did not advertise it. Cases for each
  silent drop of the QUIC-2 classifier: a 1,199-byte datagram carrying a client Initial and
  one carrying an unsupported version both get no reply and keep no state; a short header
  too short for its header-protection sample is dropped without reaching `packet.mask`; a
  33rd out-of-order gap on one stream is dropped unacknowledged and arrives on
  retransmission. *Oracle:* the error code or drop each RFC section names.
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
  a Retry token replayed from a different address or port, with a different original DCID
  or Retry SCID, or after 10 s, is refused with `INVALID_TOKEN`; a NEW_TOKEN token presented
  as a Retry token, and the reverse, is refused; a token sealed by one member of a 4-member
  group is accepted by another. *Oracle:* RFC 9000 §8.1.2, §8.1.3 and §17.2.5, by hand.
- **Q8 `stateless_reset_closes_and_never_loops`.** A server restarted with the same
  `reset_key` answers a short-header packet of a lost connection with a short-header reset
  the client recognizes. For every trigger length `n` from 1 to 1,500 and `cid_len` 8: no
  reset for `n ≤ 21 + cid_len` or for any long-header trigger; otherwise exactly one reset,
  of length `n − 1` when `n ≤ 43` and in `[43, n − 1]` above, never below 21. Two endpoints
  that reset each other, started from a 1,500-byte trigger, exchange a bounded number of
  resets and stop, each one shorter than the last. *Oracle:* RFC 9000 §10.3 and §10.3.3, by
  hand.
- **Q9 `loss_recovery_matches_rfc9002`.** Traces (a single loss, a tail loss, three PTOs in
  a row, persistent congestion) against a hand trace of RFC 9002 Appendix A and B's
  pseudocode: each packet's declared-lost time, each PTO time and the congestion window
  after each ACK, first on the recovery module and then end to end through a stream over
  a 10 ms `sim.udp` path whose client loses the datagrams each case names. *Oracle:* the
  pseudocode, worked by hand in the law's comments.
- **Q10 `cubic_matches_rfc9438`.** A scripted ACK sequence through a loss: `W_max`, `K` and
  `W_cubic(t)` at chosen `t`, and HyStart++'s exit round. *Oracle:* RFC 9438 §4 and RFC 9406
  §4's formulas, by hand.
- **Q11 `flow_control_bounds_memory`.** A peer that sends at full credit and a reader that
  reads nothing: the receiver's charge never passes `initial_max_data` plus its fixed
  connection cost, and a frame past credit is `FLOW_CONTROL_ERROR`. The send side: a peer
  that grants 2^62 of credit and acknowledges nothing; the sender's charge never passes
  `send_bytes` plus its fixed cost, `write` returns short and `write_shared` returns
  `Ok(false)`, and a later acknowledgement brings `Writable`. *Oracle:* QUIC-9.
- **Q12 `rebind_validates_new_path`.** Under `sim.udp`, the client's NAT rebinds its port
  and then its address mid-transfer. *Oracle:* RFC 9000 §9.3. No stream data goes to the
  new address before its PATH_RESPONSE, the transfer completes with the same hash, and the
  controller resets only on the address change.
- **Q13 `key_update_both_directions`.** Each side initiates two key updates during a
  transfer; packets in flight across the update are still opened (one previous key kept for
  3 PTOs). A forged short-header packet with the key phase flipped and a bad tag leaves the
  receive key unchanged, and the next genuine packet opens. With the AES-GCM confidentiality
  limit scaled down by a seam, the sender updates its key before the limit; with updates
  blocked, it closes with `AEAD_LIMIT_REACHED`. *Oracle:* RFC 9001 §6 and §6.6, and quinn as
  the peer.
- **Q14 `steer_reaches_owner`.** A group of 4 members with `steer: Quic`: every packet of a
  connection, long and short header, after CID rotation and after rebinding, reaches the
  member that issued the CID. *Oracle:* NET-16's steering vectors and the CID format.
- **Q15 `datagram_never_retransmitted`.** Under 5 % loss, 10,000 datagrams: every one that
  arrives arrives once, none is sent twice (a seam counter), and they consume congestion
  window. *Oracle:* RFC 9221 §5.2.
- **Q17 `path_mtu_fills_admitted_size`.** Under `sim.udp`, both ends set `max_mtu` 1,600
  and each socket's slot truncates anything over 1,496 bytes, which `drive` drops. After the
  search, the client's `send_datagram` accepts a 1,464-byte payload, which arrives whole,
  and refuses 1,465 bytes as `TooLarge`. *Oracle:* 1,496 less the 1-RTT header (1 byte, the
  server's 8-byte CID and a 4-byte packet number), the 16-byte AEAD tag and the DATAGRAM
  frame's type (1 byte) and length (2 bytes), by hand: a packet filled to exactly 1,496.
- **Q18 `path_mtu_reprobes_after_raise_timer`.** Over a simulated 10 ms path under a 1,600
  ceiling, the path initially carries 1,350 bytes, then rises to 1,496 bytes and later to
  1,600 bytes. After converging at 1,350, `PMTU_RAISE_TIMER` (600 s) does not fire early;
  once it fires, the search re-probes above 1,350 and reaches 1,496 (admitting 1,464 bytes
  and refusing 1,465), re-arming the timer for 600 s; after the second expiry it reaches the
  1,600 ceiling (admitting 1,568 bytes) and disarms the timer. *Oracle:* RFC 8899 §5.1.1 and
  §5.2, plus exact 1-RTT packet room by hand.
- **Q16 `pin_mismatch_refuses`.** A server whose leaf matches no pin in the client's list
  (`Cert` and `Spki` each off by one bit) is refused before any stream opens, with
  `Refused(Pin)` and TLS alert `bad_certificate` (QUIC error `0x012a`); a list `[old, new]`
  accepts a server on either certificate. A server with `client_pins` set refuses a client
  that sends no certificate with `certificate_required` (`0x0174`) and one off its pins with
  `bad_certificate`, in both cases before `Event.Connected` and before any 1-RTT stream frame
  is delivered. The P-256 CertificateVerify is DER on the wire: quinn verifies ours, and ours
  verifies quinn's. *Oracle:* RFC 8446 §4.4.2.4, §4.4.3 and §6.2, RFC 9001 §4.8.

**Cost target** (S9-style, measured under host matched-comparison rules): bulk transfer of
one stream on loopback at ≥ 50 % of quinn's throughput per core with the same cipher, GSO
on both, on the same CPU; 0 allocations per packet in steady state. The PLAN's 8 Gbps/core
is the long-run bar; the first build is held to the quinn ratio, which is a matched
comparison.

## Build slices

| Slice | Content | Depends on | Laws |
| --- | --- | --- | --- |
| **QS0** TLS-8 additions | `schedule.next_opener(s)` opens the next receive phase through `packet.open` before `key_update(s, false)` commits it (QUIC-6). Retry uses `secret.aead_seal` with the published AES-128-GCM key and nonce, and empty plaintext. | — | Q2's A.4 rows in TLS-8's PC1 and Q13's forged-phase case |
| **QS1** Codec | varints, headers, every frame, coalescing, packet-number decoding | — | Q1, Q3 (codec rows) |
| **QS2** Handshake | `quic.handshake`: TLS 1.3 messages and extensions, transport parameters, `Pin.Cert` and `Pin.Spki` lists, DER ECDSA signing, `verify_der` and Ed25519 verifying, `std.x509_key`'s Ed25519 answer and SPKI range | QS1, QS0 | Q2, Q16 |
| **QS3** Connection core | Initial, Handshake and 1-RTT spaces, the receive classifier, CIDs, ACKs, CONNECTION_CLOSE, idle, `client_pins` (mutual TLS), the `Endpoint`, `committed` | QS2 | Q3, Q4 (handshake only), Q16 (client pins) |
| **QS4** Streams | stream states, flow control, `max_gaps`, `send_bytes` and `write_shared` backpressure, RESET_STREAM_AT and its discard, priority | QS3 | Q4, Q11 |
| **QS5** Recovery | RFC 9002, NewReno, CUBIC with HyStart++, pacing, ECN | QS3 | Q9, Q10 |
| **QS6** Validation and resets | anti-amplification, Retry and NEW_TOKEN tokens, stateless reset, path validation, key update and AEAD limits | QS3, QS5 | Q6, Q7, Q8, Q12, Q13 |
| **QS7** Datagrams, driver, steering | RFC 9221 with `Priority`, `tag` and `drop_datagrams`, `drive`, GSO trains, `Steer.Quic` CIDs, DPLPMTUD | QS4, QS5, QS6 | Q14, Q15, Q17 |
| **QS8** Interop and cost | the interop runner on Boat; the cost bench | QS6, QS7 | Q5, cost target |

Reds that may ride: law Q5 and the cost target. Laws Q6, Q7, Q8, Q11 and Q16 are soundness laws and
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
| quinn-proto, quiche | CUBIC as the default controller (quinn-proto/src/congestion/cubic.rs; quiche/src/recovery/congestion/cubic.rs) | **ADOPTED** |
| quiche | HyStart++ slow-start exit (quiche/src/recovery/congestion/hystart.rs; quinn has none) | **ADOPTED** |
| quiche | BBR variants (quiche/src/recovery/gcongestion) | **NOT TAKEN** in Wave 2 (O2) |
| msquic | per-core partitions owning connections, with the RSS hash or CID picking the partition (src/core/partition.c, binding.c) | **ADOPTED** as NET-16's `Steer.Quic` CIDs and one endpoint per unit |
| msquic | Retry when handshake memory passes a fraction (`RetryMemoryLimit`, src/core/binding.c:1285, settings.c:45) | **ADAPTED** to `RetryPolicy.Above(n)`, a count of unvalidated connections, which is exact rather than a memory fraction; default `Above(unvalidated / 2)` |
| moq-quic | quinn-proto fork with a deterministic simulator (rs/moq-quic/src/sim) | **ADOPTED** in spirit: `sim.udp` drives every loss and migration law |
| quinn, quiche, msquic | 0-RTT and session tickets | **NOT TAKEN** (Scope) |
| quiche | built-in HTTP/3 in the transport crate (quiche/src/h3) | **REPLACED** by a separate `webtransport` package over `quic`'s streams |

## Design review

Rulings of the media Wave 2 design review (VERDICT-MEDIA-W2, 2026-11-26), applied above:

- **O1, O4 (pin-only authentication, no X.509 chain verifier).** Pin-only is right for Wave 2:
  `Pin.Cert` and `Pin.Spki`, a list per connection, and server `client_pins` (QUIC-3). The
  client role does not talk to public QUIC servers in Wave 2.
- **O2 (controller).** CUBIC with HyStart++ first, behind the `Controller` seam; BBRv3 later
  (QUIC-6).
- **O3 (0-RTT).** Omitted in Wave 2 (Scope).

Not ruled by the review; the standing choice is kept as written:

- **O5 (RESET_STREAM_AT is a draft).** Track draft-ietf-quic-reliable-stream-reset-11's
  codepoints (`0x1d`, `0x24`) in the build; WebTransport requires the frame
  (webtrans-http3-17 §4.4), so holding for the RFC would hold WebTransport (QUIC-5).
- **O6 (the handshake module's home).** Inside `quic` until a DTLS 1.3 consumer exists
  (QUIC-3).

**Scaling note.** One cascade link per node pair is one QUIC connection, so one unit carries
it. That is acceptable for the first cascade (KS1 to KS5); the scaling path is one link per
`Steer.Quic` member pair (CAS-2).
