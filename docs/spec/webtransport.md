# WebTransport over HTTP/3

*Specified 2026-11-26 (sfu-pool-media-w2-spec-2888, Media Wave 2 package 2 of 4); design
review media-w2-spec-fix-2918. TS1 to TS5 implement static QPACK, HTTP/3 CONNECT,
sessions, flow control across pooled sessions and browser certificate rotation.
T4 peer interoperability and T5 browser qualification have their own gates.
Rules WT-1 to WT-7 and laws T1 to T10 apply.*

`webtransport` is a sans-IO HTTP/3 server and client with only what WebTransport needs. It is
written in Hirsel under `core/programs/webtransport` and runs over
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
| QPACK | static table, literal fields, Huffman decode and encode; `SETTINGS_QPACK_MAX_TABLE_CAPACITY` 0 | dynamic table: a WebTransport session has one CONNECT, so a dynamic table saves nothing (Design review, O3) |
| CONNECT | extended CONNECT with `:protocol` `webtransport-h3` (draft-17 §3.2) | other `:protocol` values (`connect-udp`, `websocket`) answer `501` |
| Sessions | several per connection when both sides give flow-control limits (draft-17 §5); one otherwise | — |
| Streams | unidirectional (type `0x54`) and bidirectional (signal `0x41`), each tagged with its session id | — |
| Datagrams | HTTP Datagrams on the session's quarter stream id (RFC 9297 §2.1) over QUIC DATAGRAM | — |
| Capsules | WT_CLOSE_SESSION, WT_DRAIN_SESSION, WT_MAX_DATA, WT_MAX_STREAMS, WT_DATA_BLOCKED, WT_STREAMS_BLOCKED | DATAGRAM capsules: QUIC DATAGRAM is mandatory here |
| Browsers | Chromium and Firefox `new WebTransport(url, {serverCertificateHashes})` | Web PKI server certificates: ruled out for Wave 2 ([quic](quic.md) Design review, O4) |
| Versions | draft-17 codepoints only | earlier drafts' `SETTINGS_ENABLE_WEBTRANSPORT` (`0x2b603742`) and pre-17 `SETTINGS_WT_ENABLED` values: a second draft is a compatibility path (Design review, O1) |

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
| `WT_FLOW_CONTROL_ERROR` | `0x045d4487` | session close |
| `WT_ALPN_ERROR` | `0x0817b3dd` | session close |
| `WT_REQUIREMENTS_NOT_MET` | `0x212c0d48` | connection error |
| application errors | `0x52e4a40fa8db` to `0x52e5ac983162`, a 32-bit code `n` maps to `0x52e4a40fa8db + n + n / 0x1e` (draft-17 §4.4), which skips the reserved values `0x1f·N + 0x21` | RESET_STREAM, STOP_SENDING |

The QUIC connection must have agreed `max_datagram_frame_size` > 0 and `reset_stream_at`
(draft-17 §3.1), which the package reads from `quic.peer_params`. A client closes a server
whose SETTINGS or transport parameters lack any requirement with `WT_REQUIREMENTS_NOT_MET`
before any session opens; a server resets each CONNECT of such a client with
`H3_MESSAGE_ERROR` (WT-3 step 7, law T8). The ALPN is `h3`.

The HTTP/3 and QPACK codes the package uses are RFC 9114 §8.1's and RFC 9204 §6's:
`H3_REQUEST_REJECTED` (`0x010b`), `H3_CLOSED_CRITICAL_STREAM` (`0x0104`),
`H3_MESSAGE_ERROR` (`0x010e`), `H3_ID_ERROR` (`0x0108`), `H3_FRAME_UNEXPECTED` (`0x0105`),
`QPACK_DECOMPRESSION_FAILED` (`0x0200`) and `QPACK_ENCODER_STREAM_ERROR` (`0x0201`). The
package also sends `SETTINGS_MAX_FIELD_SECTION_SIZE` (`0x06`, RFC 9114 §4.2.2) as
`Limits.header_bytes`.

Because the draft is in Last Call, the IANA values may change at RFC publication. WT-1 is the
only place they live, and law T1 checks each against the draft's text, so a change of
codepoint is one table edit and one law update.

## WT-2: Shape

```hirsel signatures webtransport/webtransport
pub opaque type Endpoint
pub type SessionHandle: key = SessionHandle(conn: quic.ConnectionHandle, id: quic.StreamId)
pub type Limits {
    authorities: List[String],
    origins: List[String],
    require_origin: bool,
    sessions: u16,
    sessions_per_connection: u16,
    pending_connects: u16,
    streams_bidi: u64,
    streams_uni: u64,
    data: u64,
    buffered: u16,
    buffered_datagrams: u16,
    header_bytes: u32
}
pub type Request {
    authority: String,
    path: String,
    origin: Option[String],
    protocols: List[String],
    headers: List[(String, String)]
}
pub type Verdict = Accept(protocol: Option[String], headers: List[(String, String)]) | Refuse(status: u16)
pub type Refusal = Status(u16) | Pin
pub type Event = Requested(SessionHandle, Request) | Ready(s: SessionHandle, protocol: Option[String]) | Refused(s: SessionHandle, reason: Refusal) | Stream(s: SessionHandle, stream: quic.StreamId, bidi: bool) | Readable(SessionHandle, quic.StreamId) | Writable(SessionHandle, quic.StreamId) | Reset(s: SessionHandle, stream: quic.StreamId, code: Option[u32], final_size: u64) | Stopped(s: SessionHandle, stream: quic.StreamId, code: Option[u32]) | Acked(SessionHandle, quic.StreamId) | Datagram(SessionHandle, quic.PayloadSlice) | Draining(SessionHandle) | Closed(s: SessionHandle, code: u32, reason: Bytes)
pub type WtError = UnknownSession | State | Url | Blocked | Quic(quic.QuicError)

pub fn endpoint(take limits: Limits) -> Endpoint
pub fn transport(limits: Limits) -> quic.TransportConfig
pub fn receive(mut w: Endpoint, mut e: quic.Endpoint, now: u64, events: List[quic.Event], mut out: List[Event])
pub fn poll(mut w: Endpoint, mut e: quic.Endpoint, now: u64, mut out: List[Event]) -> Option[u64]
pub fn answer(mut w: Endpoint, mut e: quic.Endpoint, s: SessionHandle, take v: Verdict) -> Result[(), WtError]
pub fn connect(mut w: Endpoint, mut e: quic.Endpoint, now: u64, to: Addr, url: String, take pins: List[quic.handshake.Pin], take protocols: List[String]) -> Result[SessionHandle, WtError]
pub fn open(mut w: Endpoint, mut e: quic.Endpoint, s: SessionHandle, bidi: bool) -> Result[quic.StreamId, WtError]
pub fn read(mut w: Endpoint, mut e: quic.Endpoint, s: SessionHandle, stream: quic.StreamId, mut into: Bytes) -> Result[(u64, bool), WtError]
pub fn write(mut w: Endpoint, mut e: quic.Endpoint, s: SessionHandle, stream: quic.StreamId, data: Bytes, fin: bool) -> Result[u64, WtError]
pub fn write_shared(mut w: Endpoint, mut e: quic.Endpoint, s: SessionHandle, stream: quic.StreamId, take data: quic.PayloadSlice, fin: bool) -> Result[bool, WtError]
pub fn reset(mut w: Endpoint, mut e: quic.Endpoint, s: SessionHandle, stream: quic.StreamId, code: u32) -> Result[(), WtError]
pub fn reset_at(mut w: Endpoint, mut e: quic.Endpoint, s: SessionHandle, stream: quic.StreamId, code: u32, reliable: u64) -> Result[(), WtError]
pub fn priority(w: Endpoint, mut e: quic.Endpoint, s: SessionHandle, stream: quic.StreamId, p: quic.Priority) -> Result[(), WtError]
pub fn stop(mut w: Endpoint, mut e: quic.Endpoint, s: SessionHandle, stream: quic.StreamId, code: u32) -> Result[(), WtError]
pub fn send_datagram(mut w: Endpoint, mut e: quic.Endpoint, s: SessionHandle, take data: quic.FramedSlice, p: quic.Priority, tag: u64) -> Result[(), WtError]
pub fn drop_datagrams(w: Endpoint, mut e: quic.Endpoint, s: SessionHandle, tag: u64) -> u64
pub fn drain(mut w: Endpoint, mut e: quic.Endpoint, s: SessionHandle)
pub fn close(mut w: Endpoint, mut e: quic.Endpoint, s: SessionHandle, code: u32, reason: Bytes)
pub fn dropped_datagrams(w: Endpoint) -> u64

pub opaque type BrowserIdentity
pub type CertificateError = Lifetime | Key(KeyError) | Certificate(std.x509_build.BuildError) | State | Quic(quic.QuicError)
pub fn browser_identity(r: Random, serial: u64, name: String, not_before: std.time.Timestamp, not_after: std.time.Timestamp) -> Result[BrowserIdentity, CertificateError]
pub fn browser_server(mut w: Endpoint, r: Random, take config: quic.EndpointConfig, take current: BrowserIdentity, take next: BrowserIdentity) -> Result[quic.Endpoint, CertificateError]
pub fn rotate(mut w: Endpoint, mut e: quic.Endpoint, take next: BrowserIdentity) -> Result[(), CertificateError]
pub fn certificate_rotation_at(w: Endpoint) -> Option[i64]
pub fn certificate_hashes(w: Endpoint) -> List[quic.handshake.Pin]
```

An accepted CONNECT emits `Ready` with the selected protocol on each endpoint. On the server,
`answer` queues it before the stream and datagram events from the accepted CONNECT backlog.

One `Endpoint` serves both roles over one `quic.Endpoint`, as `quic` does: a server's
sessions arrive as `Requested`, a client's start with `connect`. A session is named by
`SessionHandle(conn, id)`, its QUIC connection and its CONNECT stream's id, so two clients
that each open their first session on stream 0 hold different handles. A handle is never
reused: QUIC never reuses a connection handle while the endpoint lives, and never reuses a
stream id within a connection.

`connect` reuses an eligible connection to `to` with the same authority and pin list, or starts
a QUIC connection with the pins (QUIC-3),
and returns the session's handle at once, pending. `Ready` follows once the QUIC handshake
has completed, both SETTINGS have arrived, and the server has answered `200`; `Refused`
reports a non-success status as `Status(n)` or a TLS pin mismatch as `Pin`. Other
QUIC or HTTP/3 failures first emit `Closed`.

The package sits between `quic` and the application: it consumes the QUIC events of
HTTP/3 streams, reads and writes the HTTP/3 control and QPACK streams itself, and hands the
application only session events. The application reads and writes a session's streams
through `read`, `write`, `write_shared`, `reset`, `reset_at`, `priority` and `stop`, never
`quic`'s own, so an upper protocol such as `moq` never calls `quic` on a session's
connection. Each is a thin wrapper over the `quic` call of the same name (`quic.reset` for
both resets) that first checks the stream belongs to the session, and then does the
session's accounting (WT-5): it charges `WT_MAX_DATA`, raises the session's windows as the
application reads, and maps a 32-bit application code to the draft-17 §4.4 range (WT-1). A
stream of another session, even one on the same connection, is `State`, and nothing about it
changes. `reset_at(…, code, reliable)` resets with `RESET_STREAM_AT` at the stream header's
length plus the first `reliable` bytes the application wrote, so the header and that prefix
always arrive (draft-17 §4.4); `reset` is `reset_at` with `reliable` 0. `reliable` past what
was written is `Quic(StreamState)`, and a peer without `reset_stream_at` makes it
`Quic(Unsupported)`. `priority` sets the stream's `quic.Priority` (QUIC-1). Stream data is
never copied through this layer: the wrappers pass the caller's buffers and
`quic.PayloadSlice` straight to `quic`. `Acked(s, stream)` reports `quic`'s acknowledgement
of a stream's last byte to the session that owns it, before the package forgets the
stream.

`Reset` and `Stopped` carry the peer's code mapped back to 32 bits, or `None` for a code
outside the `WT_APPLICATION_ERROR` range, such as `WT_SESSION_GONE` (draft-17 §4.4: "a stream
reset with no application error code"). A stream `open` returns, or a peer's stream, starts
with its header (WT-4); `write` returns 0 and `write_shared` `false` while the header still
waits for QUIC credit, and `Writable` follows once it is out. `send_datagram` writes the
quarter stream id into the `quic.FramedSlice` inline prefix (`quic.framed_prepend`), in
front of any header the caller put there, so the payload is not copied here, and `receive`
trims the quarter stream id off a received datagram with `quic.payload_trim_front`, so
`Datagram(s, payload)` holds the received bytes without a copy. A client may open streams
and send datagrams while its CONNECT awaits the response (draft-17 §4).
`dropped_datagrams` counts the received datagrams dropped (WT-4).

`answer` is the application's verdict on a CONNECT: `Accept` sends `200` with the protocol
it picked from the client's `WT-Available-Protocols` (draft-17 §3.3) in
`WT-Protocol`, and `Refuse(n)` sends `n`. No session data flows before `Accept`.

`transport(limits)` is the `quic.TransportConfig` both roles use: QUIC datagrams on, ALPN
`h3`, and stream credit for the three critical streams, the CONNECT streams and each
session's WT streams. A server passes it to `quic.endpoint`; `connect` uses it. `WtError` is
`UnknownSession` for a handle the endpoint does not hold, `State` for a call the session's
phase does not allow (an `answer` to a session already answered, a `Refuse` with a status
below 300 or above 599), `Url` for a `connect` URL that is not `https://authority/path`, and
`Quic` for the `quic` call's own error.

`receive` takes the QUIC events of one `quic.receive_datagram` or `quic.timeout`. The events
`answer` causes (the buffered streams and datagrams it hands over, a close the client sent
ahead) come out of the next `receive` or `poll`. `poll`
runs the package's own timers (a buffered stream's 3-PTO expiry, WT-4) and returns the next
deadline, which the caller folds into the `quic` driver's wait.

## WT-3: Session establishment

1. Both sides send SETTINGS on their control stream with WT-1's values. A CONNECT received
   before the peer's SETTINGS is held, as RFC 9114 §7.2.4.2 allows; at most
   `pending_connects` CONNECTs per connection are held unanswered, whether waiting for
   SETTINGS or for `answer`, and one past that is reset with `H3_REQUEST_REJECTED`
   (draft-17 §5.2). Closing the control stream or either QPACK stream is
   `H3_CLOSED_CRITICAL_STREAM` (RFC 9114 §6.2.1, RFC 9204 §4.2).
2. The client sends HEADERS with `:method CONNECT`, `:protocol webtransport-h3`, `:scheme
   https`, `:authority`, `:path`, and `Origin` from a browser. The CONNECT stream's id is the
   session's `id`.
3. The server checks that each pseudo-header is present once (RFC 9114 §4.3.1) and that
   `:authority` is one of `Limits.authorities`, answering `404` otherwise. When the request
   carries `Origin`, the package checks it against `Limits.origins` and answers `403` on a
   mismatch, as draft-17 §3.2 requires of the server. With `require_origin` set, a request
   without `Origin` is also answered `403`: draft-17 §3.2 makes `Origin` mandatory only for
   browsers, and a server that serves only browsers can demand it. The application never
   sees a refused request; one that passes is emitted as `Requested`.
4. On `Accept`, the server sends `200` and the session is live; the CONNECT stream then
   carries only capsules (RFC 9297 §3) in both directions.
5. A session ends with a WT_CLOSE_SESSION capsule (a 32-bit code and a reason of at most
   1,024 bytes) followed by FIN, or by the CONNECT stream's reset. Either way all its streams
   are reset with `WT_SESSION_GONE` and its datagrams are dropped (draft-17 §6). The capsules
   travel in DATA frames (RFC 9297 §3); a received close is answered with FIN and
   `STOP_SENDING` `WT_SESSION_GONE` and reported as `Closed(code, reason)`, a FIN or reset
   without one as `Closed(0, "")`, and a reason past 1,024 bytes or not UTF-8 resets the
   CONNECT stream with `H3_MESSAGE_ERROR`. `close` sends the capsule and FIN, truncating the
   reason at a UTF-8 boundary; on a session not yet live it resets the CONNECT stream with
   `H3_REQUEST_CANCELLED`. The application hears no `Closed` for its own `close`. A server
   reads nothing after the request's HEADERS until it accepts (draft-17 §3.2): capsules the
   client sent ahead are read on `Accept` and dropped with a refused request. Capsules other
   than WT_CLOSE_SESSION and WT_DRAIN_SESSION, the DATAGRAM capsule among them, are skipped;
   the flow-control capsules are TS4's (WT-5).
6. WT_DRAIN_SESSION and an HTTP/3 GOAWAY ask the peer to finish; `Draining` reports them.
   A client's sessions at or past a GOAWAY's stream id, and those it has not sent yet, were
   never processed (RFC 9114 §5.2) and are `Closed`; a GOAWAY id that grows, or from a server
   one that is not a client bidirectional stream id, is `H3_ID_ERROR`.
7. The server reads requests before the client's SETTINGS but answers none, holding each
   (draft-17 §3.1). A well-formed request that is not an extended CONNECT, and a plain
   CONNECT, are answered `404` with FIN, then `STOP_SENDING` `H3_NO_ERROR` (RFC 9114 §4.1.1);
   a malformed one (RFC 9114 §4.1.2, §4.3.1: an uppercase or connection-specific field, a
   repeated, unknown or late pseudo-header, a missing required one) is reset with
   `H3_MESSAGE_ERROR`. A client sends its CONNECT only once the server's SETTINGS carry every
   requirement; an interim `1xx` response is skipped.

## WT-4: Streams, datagrams and buffering

- A uni stream starts `0x54` then the session id; a bidi stream starts with the signal value
  `0x41` then the session id (draft-17 §4.2, §4.3). After those varints the stream belongs to
  the session.
- A stream or datagram may arrive before its session's CONNECT is processed (draft-17 §4.6).
  The server keeps at most `buffered` such streams across the connection, holding their
  header only, and at most `buffered_datagrams` such datagrams across the connection; past
  either it resets the newest stream with `WT_BUFFERED_STREAM_REJECTED`, or drops the newest
  datagram and counts it. A buffered stream whose session is refused or never arrives is
  reset with the same code when the session's CONNECT stream closes, or after 3 PTOs
  (`quic.pto_ns`); buffered datagrams go with it.
- A stream naming a session id that is not a client-initiated bidirectional stream id is
  `H3_ID_ERROR`. A stream naming a closed session is reset with `WT_SESSION_GONE`. The
  package reads a peer stream's leading varints exactly, never past them, so the bytes after
  a WT stream's header stay in `quic` for the application. A server's client bidirectional
  stream is a request unless its first varint is `0x41`; `0x41` anywhere else is
  `H3_FRAME_UNEXPECTED` (draft-17 §4.3). A server waits on a session id of a client
  bidirectional stream not yet opened or not yet read; a client waits on its CONNECT's
  response.
- A datagram's payload is the quarter stream id of the CONNECT stream (a varint) then the
  data (RFC 9297 §2.1). One naming no session is dropped and counted, and a malformed quarter
  stream id, or one past 2^60 − 1, is `H3_DATAGRAM_ERROR`. `send_datagram` passes its
  `quic.Priority` and `tag` to `quic.send_datagram` (QUIC-7) as `quic.DatagramTag(owner,
  tag)`, where `owner` is the session's CONNECT stream id plus one. That owner is never 0,
  the owner raw `quic` callers use, and never repeats on a connection, since QUIC never
  reuses a stream id, so it needs no counter and cannot run out. `drop_datagrams(s, tag)`
  removes the session's own queued datagrams tagged `tag`, those in a polled but uncommitted
  packet included, and never another session's or a raw caller's, whatever their tags. The
  session's end drops every datagram it queued and has not committed
  (`quic.drop_owner_datagrams`), and the ones it buffered or receives; a datagram that
  arrives later names no session and is dropped and counted.

## WT-5: Flow control and session limits

draft-17 §5's session-level limits are enforced at both ends: `data` bounds the bytes of all a
session's streams together, excluding stream headers (§5.4), and `streams_bidi` and
`streams_uni` bound the streams the peer may open in it (§5.3). They are sent as SETTINGS
initial values and raised with WT_MAX_DATA and WT_MAX_STREAMS capsules as the application
reads through `read` and peer-initiated streams close, at half the window as `quic` does (QUIC-5).

The receive side counts, per session, the largest offset received on each of its streams
(`quic.received`), less the header, and for a reset stream its final size from
`quic.Event.Reset` (§5.4 requires the final size so both ends agree on the credit
consumed). The count is checked on every `quic.Event.Readable` and `Reset` the package
consumes, so a peer that writes past `data` is caught when its bytes arrive, not when the
application reads them; that, or opening past `streams_bidi` or `streams_uni`, closes that
session (not the connection) with `WT_FLOW_CONTROL_ERROR`. A received WT_MAX_STREAM_DATA or
WT_STREAM_DATA_BLOCKED capsule is a session error (§5.4). The send side does the same
accounting in `write` and `write_shared`: they take no more than the peer's WT_MAX_DATA
allows and send WT_DATA_BLOCKED when it stops them, and `open` past the peer's
WT_MAX_STREAMS is `Blocked` and sends WT_STREAMS_BLOCKED. A reset refunds any
accepted bytes that are outside its QUIC final size.

Sessions:

- **Without flow control** (draft-17 §5.1: either side sent no nonzero
  `SETTINGS_WT_INITIAL_MAX_*`), a connection holds one session. The server resets a second
  CONNECT stream with `H3_REQUEST_REJECTED`, as §5.1 requires, and the client never sends
  one. Flow-control capsules are ignored (§5.1).
- **With flow control**, a connection holds at most `sessions_per_connection` sessions, and
  the endpoint at most `sessions` across all connections. A CONNECT past either is answered
  `429` (§5.2's rate-limit signal, which reaches the application), since the request was
  read and refused, not unprocessed.

## WT-6: QPACK

The encoder writes only static-table references and literals with a static-table name or a
literal name, Huffman-coded when shorter; it never inserts. The decoder advertises
`SETTINGS_QPACK_MAX_TABLE_CAPACITY` 0 and accepts the same. On the encoder stream, a Set
Dynamic Table Capacity above 0 or any insert instruction is the connection error
`QPACK_ENCODER_STREAM_ERROR` (`0x0201`, RFC 9204 §4.3.1 and §3.2.2: the capacity exceeds the
limit, or the entry cannot fit). In a field section, a dynamic reference or a non-zero
Required Insert Count is `QPACK_DECOMPRESSION_FAILED` (`0x0200`, §4.5.1). A field section
whose decoded size passes `header_bytes`, the `SETTINGS_MAX_FIELD_SECTION_SIZE` the package
advertised, is malformed (RFC 9114 §4.2.2): decoding stops at the bound and the stream is
reset with `H3_MESSAGE_ERROR`, a bound checked as it is decoded, never after. Before decoding,
a HEADERS frame longer than 4 × `header_bytes` + 64 is refused the same way: HPACK's longest
code is 30 bits (RFC 7541 Appendix B), so n coded bytes decode to at least 8n/30, and such a
frame would decode past the bound; the frame is never buffered whole.

## WT-7: Certificates for browsers

A browser reaches a server that has no Web PKI certificate through
`serverCertificateHashes`: SHA-256 of the DER certificate, and the certificate must be ECDSA
P-256 and valid for at most 14 days (W3C WebTransport,
[§ 6.4 serverCertificateHashes](https://w3c.github.io/webtransport/#dom-webtransportoptions-servercertificatehashes)).
That hash is exactly `quic.handshake.Pin.Cert` (QUIC-3).

`browser_identity` rejects validity intervals outside (0, 14 days] before minting with
a key from `secret.p256_key`. It signs a P-256 server leaf with its own key and
puts the DNS name or IP address in subjectAltName. Chromium requires the
X.509 extensions field when validating certificate hashes.
`browser_server` starts QUIC with the *current* identity and keeps the *next* one.
`certificate_hashes(w)` is `[Cert(current), Cert(next)]`, and whatever page or signaling
serves the client publishes it, so a client fetched at any time holds a list that contains
the certificate served now and the one served after the next rotation. `rotate(w, e, next)`
makes the old next identity current through `quic.set_identity` (live connections keep
theirs) and takes `next` as the new next. The server rotates before the current certificate
is 13 days old, or before expiration for a shorter-lived identity.
`certificate_rotation_at` returns that Unix second for the server's scheduler.
A new connection pinned only to a certificate that has rotated out is refused with
`Refused(Pin)`.

## Laws

Red first in `core/programs/webtransport/laws.hirsel` unless named.

- **T1 `codepoints_match_draft`.** Each row of WT-1 against draft-17's IANA section and
  RFC 9114, 9220 and 9297's registries, copied by hand into the law with section numbers.
  *Oracle:* the documents.
- **T2 `qpack_static_vectors`.** RFC 9204 Appendix A's static table, and the field section of
  RFC 9204 Appendix B.1, the only one that uses no dynamic table, decoded byte for byte. B.1
  sends `/index.html` raw, and WT-6's encoder Huffman-codes it (8 bytes against 11), so the
  encode vectors are derived by hand from Appendix A and RFC 7541's code; a
  dynamic reference in any position is `QPACK_DECOMPRESSION_FAILED`, and a Set Dynamic Table
  Capacity of 1 or an insert on the encoder stream is `QPACK_ENCODER_STREAM_ERROR`.
  *Oracle:* the RFC.
- **T3 `huffman_round_trip_and_vectors`.** RFC 7541 Appendix C.4's Huffman strings (QPACK
  uses HPACK's code, RFC 9204 §4.1.2), then every byte string up to length 3 round-trips,
  and EOS or padding over 7 bits is an error. *Oracle:* RFC 7541.
- **T4 `session_against_draft17_peer`.** The Hirsel server accepts a session from a
  draft-17 WebTransport implementation's client, and the Hirsel client opens one on its
  server. Each side echoes 100 uni streams, 100 bidi streams and 10,000 datagrams. The peer
  must send draft-17's `SETTINGS_WT_ENABLED` (`0x20b8933b`); quiche (`ac43c0d`) has no
  WebTransport, and on 2026-11-26 neither webtransport-go (`d0c9357`, which sends
  `0x2c7cf000` and the draft-06 `0x2b603742`) nor `wtransport`
  ([github.com/BiagioFesta/wtransport](https://github.com/BiagioFesta/wtransport), `b16674e`,
  `0x2b603742` only) does, so T4 names the first of them to ship draft-17 and rides red until
  one does, as MoQ's M2 does. No law body exists yet: without a draft-17 peer there is no
  harness to write against. *Oracle:* the other side's received bytes hash to the
  sender's.
- **T5 `browser_echo`.** Headless Chromium and Firefox, through `serverCertificateHashes`,
  open a session, echo a bidi stream of 10 MiB and 1,000 datagrams, then close with code 7 and
  reason `"bye"`, which the server reports as `Closed(_, 7, "bye")`. Nightly on Boat.
  *Oracle:* the browser's `WebTransportCloseInfo` and the hashes. Run T5 and T10 with
  `python3 core/programs/webtransport/browser/run.py --compiler HIRSEL --runtime PIC_ARCHIVE --linker LD_LLD --work DIR`.
  Boat mints the echo identities in the VM. Chromium 153 and Firefox 155
  require older WebTransport SETTINGS and leave T5 red until they support draft-17.
- **T6 `buffered_streams_bounded`.** Under `sim.udp`, a client whose CONNECT is delayed
  opens `buffered` + 5 streams: the first `buffered` are delivered once the session is
  accepted, the rest are reset with `WT_BUFFERED_STREAM_REJECTED`, and a refused session's
  buffered streams are reset too. *Oracle:* draft-17 §4.6.
- **T7 `session_close_resets_its_streams_only`.** Two sessions on one connection; closing one
  resets each of its streams with `WT_SESSION_GONE` and drops its datagrams while the
  other's streams complete. *Oracle:* draft-17 §6.
- **T8 `refusals`.** A CONNECT with `:protocol` other than `webtransport-h3` (`501`), a
  missing `:authority` (stream error `H3_MESSAGE_ERROR`), an `:authority` outside
  `authorities` (`404`), an Origin outside `origins` (`403`, and no session), no Origin with
  `require_origin` (`403`), a second session without flow-control SETTINGS (CONNECT stream
  reset with `H3_REQUEST_REJECTED`), a session past `sessions_per_connection` with flow
  control (`429`), CONNECTs past `pending_connects` (`H3_REQUEST_REJECTED`), a field section
  past `header_bytes` (`H3_MESSAGE_ERROR`, decoding stopped at the bound), a closed control
  stream (`H3_CLOSED_CRITICAL_STREAM`), a server without `H3_DATAGRAM` (the client closes
  with `WT_REQUIREMENTS_NOT_MET`), a client without `H3_DATAGRAM` (its CONNECT is reset with
  `H3_MESSAGE_ERROR`: draft-17 §3.1 makes such a client's sessions malformed and does not close
  the connection), and a request that is not a WebTransport CONNECT (`404`). *Oracle:* the sections cited in WT-1, WT-3, WT-5 and WT-6.
- **T9 `session_flow_control`.** A peer that writes past `data` while the application reads
  nothing, or resets a stream with a final size past it, or opens past `streams_uni`, gets
  `WT_FLOW_CONTROL_ERROR` on that session only, and the connection's other session carries
  on; an honest peer that reads slowly never stalls for more than one window. Two clients
  whose first sessions are both on stream 0 get distinct handles, and one's `close` leaves
  the other's session live. *Oracle:* draft-17 §5.
- **T10 `certificate_rotation_accepts_either`.** A client given `certificate_hashes` before a
  rotation (`[old, new]`) connects both before and after `rotate`; a client pinned to `[old]`
  only connects before and is refused with `Refused(Pin)` after; a connection made before
  the rotation stays up across it. A certificate valid for 15 days is refused by
  `browser_identity` before it can be served. *Oracle:* the W3C rule, and
  QUIC-3's any-pin rule.

## Build slices

| Slice | Content | Depends on | Laws |
| --- | --- | --- | --- |
| **TS1** QPACK and frames | HTTP/3 frames, stream types, static QPACK, Huffman | — | T1, T2, T3 |
| **TS2** HTTP/3 connection | control streams, SETTINGS, GOAWAY, the CONNECT exchange, 404 for others | quic QS4 | T8 (H3 rows) |
| **TS3** Sessions | `SessionHandle`, the unified `Endpoint`, the stream wrappers, WT streams and datagrams, buffering, capsules, close and drain | TS2, quic QS7 | T4, T6, T7 |
| **TS4** Flow control and limits | WT-5, pooled sessions, `Limits` | TS3 | T8, T9 |
| **TS5** Browsers | WT-7 rotation; browser laws on Boat | TS4, quic QS8 | T5, T10 |

Reds that may ride: laws T4 and T5 (interop and browsers, nightly). Laws T6, T8 and T9 bound
memory and block.

## Prior art

Goose (/tmp/ref-goose at `d5ae5bb`) has no HTTP/3. References: quiche (/tmp/ref-quiche at
`ac43c0d`, quiche/src/h3), moq-rs (/tmp/ref-moq at `d791d54`, its `web-transport` dependency in
rs/moq-relay and rs/moq-tokio), quinn (/tmp/ref-quinn at `75d08d9`, no HTTP/3 of its own).

| Source | Mechanism | Verdict |
| --- | --- | --- |
| quiche h3 | HTTP/3 as a layer over the transport's stream API, polled for events (quiche/src/h3/mod.rs `Connection::poll`) | **ADOPTED** as `receive` over `quic.Event`s |
| quiche h3 | full QPACK with dynamic table (quiche/src/h3/qpack) | **NOT TAKEN**: static only (WT-6, O3) |
| quiche h3 | generic request/response HTTP/3 server | **NOT TAKEN**: `std.http` is the HTTP server; this package refuses non-WebTransport requests |
| moq-rs | WebTransport as one of two transports under MoQ, native QUIC the other (rs/moq-relay, rs/moq-tokio) | **ADOPTED** in [MoQ](moq.md) MOQ-1 |
| Goose | one obvious path per capability | **ADOPTED**: one WebTransport draft, one QPACK mode |

## Design review

Rulings of the media Wave 2 design review (VERDICT-MEDIA-W2, 2026-11-26), applied above:

- **O1 (draft-17 only).** Draft-17 codepoints only; no earlier draft's settings. Law T4 targets
  a draft-17 peer instead of quiche's h3, which has no WebTransport.

Not ruled by the review; the standing choice is kept as written:

- **O2 (a WebTransport browser client of the SFU).** Not specified in Wave 2: MoQ over
  WebTransport is the browser QUIC path, and a second media framing over WebTransport
  datagrams would be a second path for one capability.
- **O3 (static-only QPACK).** Static only. Every browser must accept a peer advertising table
  capacity 0 (RFC 9204 §3.2.3), and law T5 exercises it against Chromium and Firefox.
