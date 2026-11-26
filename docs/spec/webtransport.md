# WebTransport over HTTP/3

*Specified 2026-11-26 (sfu-pool-media-w2-spec-2888, Media Wave 2 package 2 of 4). Not built.
Rules WT-1 to WT-7, laws T1 to T10, slices T1 to T5. Goes to design review (astra) before
any build.*

`webtransport` is a sans-IO HTTP/3 server and client with only what WebTransport needs. It is
written in Hirsel under `core/programs/webtransport` <!-- doc-ref: planned --> and runs over
[QUIC](quic.md). It carries [MoQ](moq.md) to browsers, and gives a browser client of
the SFU a QUIC path beside WebRTC. It follows:

- [RFC 9114](https://www.rfc-editor.org/rfc/rfc9114) HTTP/3, and
  [RFC 9204](https://www.rfc-editor.org/rfc/rfc9204) QPACK;
- [RFC 9220](https://www.rfc-editor.org/rfc/rfc9220) extended CONNECT over HTTP/3, and
  [RFC 9297](https://www.rfc-editor.org/rfc/rfc9297) HTTP Datagrams and the Capsule Protocol;
- [draft-ietf-webtrans-http3-17](https://datatracker.ietf.org/doc/draft-ietf-webtrans-http3/17/)
  (2026-11-02, in Working Group Last Call), and its overview
  [draft-ietf-webtrans-overview](https://datatracker.ietf.org/doc/draft-ietf-webtrans-overview/).

It is not a general HTTP/3 server. A request that is not a WebTransport CONNECT gets `404`
with no body, so `std.http` stays the one HTTP server and no second request router appears.

## Scope

| Area | In Wave 2 | Not in Wave 2 (and why) |
| --- | --- | --- |
| HTTP/3 | control streams, SETTINGS, GOAWAY, HEADERS, DATA, the frame and stream-type registries, unknown-type skipping (RFC 9114 §9) | server push, PRIORITY_UPDATE: no consumer uses them; a received PUSH_PROMISE is `H3_FRAME_UNEXPECTED` (a server never receives one; a client never enables push) |
| QPACK | static table, literal fields, Huffman decode and encode; `SETTINGS_QPACK_MAX_TABLE_CAPACITY` 0 | dynamic table: a WebTransport session has one CONNECT, so a dynamic table saves nothing; open item O3 |
| CONNECT | extended CONNECT with `:protocol` `webtransport-h3` (draft-17 §3.2) | other `:protocol` values (`connect-udp`, `websocket`) answer `501` |
| Sessions | several per connection when both sides give flow-control limits (draft-17 §5); one otherwise | — |
| Streams | unidirectional (type `0x54`) and bidirectional (signal `0x41`), each tagged with its session id | — |
| Datagrams | HTTP Datagrams on the session's quarter stream id (RFC 9297 §2.1) over QUIC DATAGRAM | — |
| Capsules | WT_CLOSE_SESSION, WT_DRAIN_SESSION, WT_MAX_DATA, WT_MAX_STREAMS, WT_DATA_BLOCKED, WT_STREAMS_BLOCKED | DATAGRAM capsules: QUIC DATAGRAM is mandatory here |
| Browsers | Chromium and Firefox `new WebTransport(url, {serverCertificateHashes})` | Web PKI server certificates: [quic](quic.md) open item O4 |
| Versions | draft-17 codepoints only | earlier drafts' `SETTINGS_ENABLE_WEBTRANSPORT` (`0x2b603742`): no supported browser needs it after Chromium's and Firefox's current releases; open item O1 |

## WT-1: Codepoints

From draft-17 §9 and the RFCs above. These are the only values the package writes.

| Name | Value | Where |
| --- | --- | --- |
| `SETTINGS_WT_ENABLED` | `0x20b8933b` (draft-17 codepoint) | SETTINGS, both sides |
| `SETTINGS_WT_INITIAL_MAX_STREAMS_UNI` | `0x2b64` | SETTINGS |
| `SETTINGS_WT_INITIAL_MAX_STREAMS_BIDI` | `0x2b65` | SETTINGS |
| `SETTINGS_WT_INITIAL_MAX_DATA` | `0x2b61` | SETTINGS |
| `SETTINGS_ENABLE_CONNECT_PROTOCOL` | `0x08` = 1 | SETTINGS, server (RFC 9220 §3) |
| `SETTINGS_H3_DATAGRAM` | `0x33` = 1 | SETTINGS, both (RFC 9297 §2.1.1) |
| `WT_STREAM` (uni stream type) | `0x54` | first varint of a uni stream |
| `WT_STREAM` (bidi signal) | `0x41` | first varint of a bidi stream |
| `WT_CLOSE_SESSION` | `0x2843` | capsule |
| `WT_DRAIN_SESSION` | `0x78ae` | capsule |
| `WT_MAX_DATA` | `0x190B4D3D` | capsule |
| `WT_MAX_STREAMS` | `0x190B4D3F` bidi, `0x190B4D40` uni | capsule |
| `WT_DATA_BLOCKED` | `0x190B4D41` | capsule |
| `WT_STREAMS_BLOCKED` | `0x190B4D43` bidi, `0x190B4D44` uni | capsule |
| `WT_BUFFERED_STREAM_REJECTED` | `0x3994bd84` | stream error |
| `WT_SESSION_GONE` | `0x170d7b68` | stream error |
| `WT_FLOW_CONTROL_ERROR` | `0x045d4487` | connection error |
| `WT_ALPN_ERROR` | `0x0817b3dd` | session close |
| `WT_REQUIREMENTS_NOT_MET` | `0x212c0d48` | connection error |
| application errors | `0x52e4a40fa8db` to `0x52e5ac983162`, a 32-bit code `n` maps to `0x52e4a40fa8db + n + n / 0x1e` (draft-17 §4.4), which skips the reserved values `0x1f·N + 0x21` | RESET_STREAM, STOP_SENDING |

The QUIC connection must have agreed `max_datagram_frame_size` > 0 and `reset_stream_at`
(draft-17 §3.1). A peer whose SETTINGS or transport parameters lack any requirement is
closed with `WT_REQUIREMENTS_NOT_MET` before any session opens. The ALPN is `h3`.

Because the draft is in Last Call, the IANA values may change at RFC publication. WT-1 is the
only place they live, and law T1 checks each against the draft's text, so a change of
codepoint is one table edit and one law update.

## WT-2: Shape

```hirsel unbuilt
pub opaque type Server
pub opaque type Client
pub type SessionId = SessionId(u64)
pub type Limits { sessions: u16, streams_bidi: u64, streams_uni: u64, data: u64, buffered: u16, header_bytes: u32 }
pub type Request { authority: String, path: String, origin: Option[String], protocols: List[String], headers: List[(String, String)] }
pub type Verdict = Accept(protocol: Option[String], headers: List[(String, String)]) | Refuse(status: u16)
pub type Event = Requested(SessionId, Request) | Ready(id: SessionId, protocol: Option[String]) | Stream(id: SessionId, stream: quic.StreamId, bidi: bool) | Datagram(SessionId, Bytes) | Draining(SessionId) | Closed(id: SessionId, code: u32, reason: Bytes)

pub fn server(limits: Limits) -> Server
pub fn receive(mut s: Server, mut e: quic.Endpoint, events: List[quic.Event], mut out: List[Event])
pub fn answer(mut s: Server, mut e: quic.Endpoint, id: SessionId, v: Verdict) -> Result[(), WtError]
pub fn open(mut s: Server, mut e: quic.Endpoint, id: SessionId, bidi: bool) -> Result[quic.StreamId, WtError]
pub fn send_datagram(mut s: Server, mut e: quic.Endpoint, id: SessionId, data: Shared[Bytes]) -> Result[(), WtError]
pub fn drain(mut s: Server, mut e: quic.Endpoint, id: SessionId)
pub fn close(mut s: Server, mut e: quic.Endpoint, id: SessionId, code: u32, reason: Bytes)
pub fn connect(mut e: quic.Endpoint, now: u64, to: Addr, url: String, pin: quic.Pin, protocols: List[String], limits: Limits) -> Result[(Client, SessionId), WtError]
```

The package sits between `quic.receive` and the application: it consumes the QUIC events of
HTTP/3 streams, reads and writes the HTTP/3 control and QPACK streams itself, and hands the
application only session events. Streams of a session stay QUIC streams: the application
reads and writes them with `quic.read`, `quic.write` and `quic.write_shared` after the
package has consumed their `0x54` or `0x41` header and session id, so stream data is
never copied through this layer.

`answer` is the application's verdict on a CONNECT: `Accept` sends `200` with the protocol
it picked from the client's `WT-Available-Protocols` (draft-17 §3.3) in
`WT-Protocol`, and `Refuse(n)` sends `n`. No session data flows before `Accept`.

## WT-3: Session establishment

1. Both sides send SETTINGS on their control stream with WT-1's values. A CONNECT received
   before the peer's SETTINGS is held, as RFC 9114 §7.2.4.2 allows, up to `header_bytes`.
2. The client sends HEADERS with `:method CONNECT`, `:protocol webtransport-h3`, `:scheme
   https`, `:authority`, `:path`, and `Origin` from a browser. The CONNECT stream's id is the
   session id.
3. The server checks that each pseudo-header is present once (RFC 9114 §4.3.1), that
   `:authority` matches a configured authority, and emits `Requested`. Origin checking is the
   application's (draft-17 §6 requires the server to check it; the request carries it, and
   law T8 requires a refusal when the application rejects it).
4. On `Accept`, the server sends `200` and the session is live; the CONNECT stream then
   carries only capsules (RFC 9297 §3) in both directions.
5. A session ends with a WT_CLOSE_SESSION capsule (a 32-bit code and a reason of at most
   1,024 bytes) followed by FIN, or by the CONNECT stream's reset. Either way all its streams
   are reset with `WT_SESSION_GONE` and its datagrams are dropped (draft-17 §6).
6. WT_DRAIN_SESSION and an HTTP/3 GOAWAY ask the peer to finish; `Draining` reports them.

## WT-4: Streams, datagrams and buffering

- A uni stream starts `0x54` then the session id; a bidi stream starts with the signal value
  `0x41` then the session id (draft-17 §4.2, §4.3). After those varints the stream belongs to
  the session.
- A stream or datagram may arrive before its session's CONNECT is processed (draft-17 §4.6).
  The server keeps at most `buffered` such streams across the connection, holding their
  header only, and at most one datagram each; past that it resets the newest with
  `WT_BUFFERED_STREAM_REJECTED`. A buffered stream whose session is refused or never
  arrives is reset with the same code when the session's CONNECT stream closes, or after 3
  PTOs.
- A stream naming a session id that is not a client-initiated bidirectional stream id is
  `H3_ID_ERROR`. A stream naming a closed session is reset with `WT_SESSION_GONE`.
- A datagram's payload is the quarter stream id of the CONNECT stream (a varint) then the
  data (RFC 9297 §2.1). One naming no session is dropped and counted.

## WT-5: Flow control and session limits

draft-17 §5's session-level limits are enforced at both ends: `data` bounds the bytes of all a
session's streams together, `streams_bidi` and `streams_uni` bound the streams it may open.
They are sent as SETTINGS initial values and raised with WT_MAX_DATA and WT_MAX_STREAMS
capsules as the application reads, at half the window as `quic` does (QUIC-5). Exceeding a
limit closes the connection with `WT_FLOW_CONTROL_ERROR`. A client that sent no
WT flow-control SETTINGS can hold one session only (draft-17 §5.1), so the server refuses its second CONNECT with
`429`; the status is this package's choice, the refusal is the draft's.

## WT-6: QPACK

The encoder writes only static-table references and literals with a static-table name or a
literal name, Huffman-coded when shorter; it never inserts. The decoder accepts the same,
and since it advertised a table capacity of 0, any dynamic reference, any encoder-stream
instruction past `Set Dynamic Table Capacity 0`, or a non-zero Required Insert Count is
`QPACK_DECOMPRESSION_FAILED` (RFC 9204 §4.5.1). A field section over `header_bytes`
decoded is `H3_EXCESSIVE_LOAD`, a bound checked as it is decoded, never after.

## WT-7: Certificates for browsers

A browser reaches a server that has no Web PKI certificate through
`serverCertificateHashes`: SHA-256 of the DER certificate, and the certificate must be ECDSA
P-256 and valid for at most 14 days (W3C WebTransport,
[§ 6.4 serverCertificateHashes](https://w3c.github.io/webtransport/#dom-webtransportoptions-servercertificatehashes)).
The server mints one with `std.x509_build.self_signed_p256` and a key minted by
`secret.p256_key`, rotates it before 13 days, and publishes both the current and the next
hash so a client fetched near a rotation accepts either. The hash is published by whatever
page or signaling serves the client; the package exposes `certificate_hashes(s)` and nothing
else about it.

## Laws

Red first in `core/programs/webtransport/laws.hirsel` <!-- doc-ref: planned --> unless named.

- **T1 `codepoints_match_draft`.** Each row of WT-1 against draft-17's IANA section and
  RFC 9114, 9220 and 9297's registries, copied by hand into the law with section numbers.
  *Oracle:* the documents.
- **T2 `qpack_static_vectors`.** RFC 9204 Appendix A's static table, and the field sections of
  RFC 9204 Appendix B.1 that use no dynamic table, encoded and decoded byte for byte; a
  dynamic reference in any position is `QPACK_DECOMPRESSION_FAILED`. *Oracle:* the RFC.
- **T3 `huffman_round_trip_and_vectors`.** RFC 7541 Appendix C.4's Huffman strings (QPACK
  uses HPACK's code, RFC 9204 §4.1.2), then every byte string up to length 3 round-trips,
  and EOS or padding over 7 bits is an error. *Oracle:* RFC 7541.
- **T4 `session_against_quiche_and_wtransport`.** The Hirsel server accepts a session from
  quiche's h3 client and from the `wtransport` crate's client
  ([github.com/BiagioFesta/wtransport](https://github.com/BiagioFesta/wtransport)), and the
  Hirsel client opens one on each's server. Each side echoes 100 uni streams, 100 bidi
  streams and 10,000 datagrams. *Oracle:* the other side's received bytes hash to the
  sender's.
- **T5 `browser_echo`.** Headless Chromium and Firefox, through `serverCertificateHashes`,
  open a session, echo a bidi stream of 10 MiB and 1,000 datagrams, then close with code 7 and
  reason `"bye"`, which the server reports as `Closed(_, 7, "bye")`. Nightly on Boat.
  *Oracle:* the browser's `WebTransportCloseInfo` and the hashes.
- **T6 `buffered_streams_bounded`.** Under `sim.udp`, a client whose CONNECT is delayed
  opens `buffered` + 5 streams: the first `buffered` are delivered once the session is
  accepted, the rest are reset with `WT_BUFFERED_STREAM_REJECTED`, and a refused session's
  buffered streams are reset too. *Oracle:* draft-17 §4.6.
- **T7 `session_close_resets_its_streams_only`.** Two sessions on one connection; closing one
  resets each of its streams with `WT_SESSION_GONE` and drops its datagrams while the
  other's streams complete. *Oracle:* draft-17 §6.
- **T8 `refusals`.** A CONNECT with `:protocol` other than `webtransport-h3` (`501`), a
  missing `:authority` (stream error `H3_MESSAGE_ERROR`), an Origin the application rejects
  (the status it chose, and no session), a second session without flow-control SETTINGS
  (`429`), a peer without `H3_DATAGRAM` (`WT_REQUIREMENTS_NOT_MET`). *Oracle:* the
  sections cited in WT-3 and WT-5.
- **T9 `session_flow_control`.** A peer that writes past `data`, or opens past
  `streams_uni`, gets `WT_FLOW_CONTROL_ERROR`; an honest peer that reads slowly never stalls
  for more than one window. *Oracle:* draft-17 §5.
- **T10 `certificate_rotation_accepts_either`.** Across a rotation, a client pinned to the old
  hash and one pinned to the new both connect; a certificate valid for 15 days is refused by
  `self_signed_p256`'s caller check before it can be served. *Oracle:* the W3C rule.

## Build slices

| Slice | Content | Depends on | Laws |
| --- | --- | --- | --- |
| **T1** QPACK and frames | HTTP/3 frames, stream types, static QPACK, Huffman | — | T1, T2, T3 |
| **T2** HTTP/3 connection | control streams, SETTINGS, GOAWAY, the CONNECT exchange, 404 for others | quic Q4 | T8 (H3 rows) |
| **T3** Sessions | WT streams and datagrams, buffering, capsules, close and drain | T2, quic Q7 | T4, T6, T7 |
| **T4** Flow control and limits | WT-5 | T3 | T8, T9 |
| **T5** Browsers | WT-7 rotation; browser laws on Boat | T3, quic Q8 | T5, T10 |

Reds that may ride: T4 and T5 (interop and browsers, nightly). T6, T8 and T9 bound memory
and block.

## Prior art

Goose (/tmp/ref-goose at `d5ae5bb`) has no HTTP/3. References: quiche (/tmp/ref-quiche at
`ac43c0d`, quiche/src/h3), moq-rs (/tmp/ref-moq at `d791d54`, its `web-transport` use in
rs/moq-native), quinn (/tmp/ref-quinn at `75d08d9`, no HTTP/3 of its own).

| Source | Mechanism | Verdict |
| --- | --- | --- |
| quiche h3 | HTTP/3 as a layer over the transport's stream API, polled for events (quiche/src/h3/mod.rs `Connection::poll`) | **ADOPTED** as `receive` over `quic.Event`s |
| quiche h3 | full QPACK with dynamic table (quiche/src/h3/qpack) | **NOT TAKEN**: static only (WT-6, O3) |
| quiche h3 | generic request/response HTTP/3 server | **NOT TAKEN**: `std.http` is the HTTP server; this package refuses non-WebTransport requests |
| moq-rs | WebTransport as one of two transports under MoQ, native QUIC the other (rs/moq-native) | **ADOPTED** in [MoQ](moq.md) MOQ-1 |
| Goose | one obvious path per capability | **ADOPTED**: one WebTransport draft, one QPACK mode |

## Open for design review

- **O1** Draft-17 only, or also the `SETTINGS_ENABLE_WEBTRANSPORT` (draft-02 era) values that
  older Chromium releases still send? Supporting two drafts is a compatibility path; the cost
  is browsers a few releases old.
- **O2** Should a WebTransport browser client of the SFU (media over datagrams instead of
  SRTP) be specified now, or wait for MoQ to be the browser path?
- **O3** Static-only QPACK. Cheaper and has no state attacks; it costs a few hundred bytes per
  CONNECT. Confirm the browsers accept a peer advertising table capacity 0 (they must, by
  RFC 9204, but it is worth a law).
