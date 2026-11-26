# HTTP

[API overview](../API.md). [Specification](../../../docs/spec/http.md).
[Outbound HTTP/1.1](http_client.md).

`defaults()` sets ceilings of 512 sockets, HTTP connections, first-phase
connections and sockets per source IP. It sets 8 KiB head and output buffers,
64 fields, a 256 KiB handler budget, 16 refusal workers and a 64-event
supervisor inbox. Upgrades and event streams are disabled.

`serve(take listeners, clock, tasks, take config, take routes, take state)`
serves HTTP/1.1 and HTTP/1.0 on listeners from `Net`. The supplied
`Tasks` capability owns the acceptors, connection tasks and refusal
workers. The supervisor reserves the refusal workers at startup, admits
each connection at spawn, and keeps its reservation through the last
request and linger. It counts accepted peers before reading a head and
moves the per-IP count after validating a trusted forwarding field.

`serve_with_stop` also takes a receiver for one absolute monotonic
deadline. The supervisor commits the stop in the same inbox that commits
idle/active transitions. It closes admission, cancels idle connections and
shortens active exchanges to that deadline. A promoted stream finishes
its current output and closes. An upgraded protocol can read the deadline
from its socket. At the deadline any remaining socket task is cancelled.

`serve_https` also takes an `https.Server` and an optional stop receiver,
and serves HTTP/1.1 over TLS 1.3: each connection runs the handshake under
its first-head deadline, choosing the identity by the client's server name.
It requires `refuse_max` to be zero, since a refusal would be plaintext; a
refused socket is closed. See [HTTPS identities](https.md).

`serve_with_factory` uses the same supervisor and creates a sendable state
value for each admitted connection. The supervisor calls the factory within
`state_bytes`. Live state allocations stay charged there until they drop.
`serve` and `serve_with_stop` share one state allocation.

`serve_with_connection_plan` calls `request_done` after handling and writing a
request. The supervisor keeps a private retirement value for each admitted
connection. After the owner ends, the supervisor checks that value and, if
retirement is pending, runs it in a task that keeps the connection slot.
`serve_with_stream_factory` accepts the same connection lifecycle callbacks
alongside its stream factory.

Build `routes` with [router](router.md). A raw handler receives an anchored
`Request`, a scratch `Region`, a mutable `Responder` and a read-only
view of shared state. The region's allocations
count against `handler_bytes` and the region closes when the handler exits.
Copy data into owned values to keep it in a reply. The handler returns
`Reply(Response)`, `Stream(Spec)`, `Upgrade(Spec)` or an error.
Streams and upgrades transfer the socket to a task admitted by the
supervisor. [Streams](stream.md) · [Upgrade](upgrade.md)
One task parses, routes, handles and writes each request in turn.

`method`, `target`, `path`, `query` and `field` borrow request bytes.
`segments` borrows the decoded path segments. `capture` borrows a named
single segment. `capture_tail` borrows a named final capture as a slice of
decoded segments. `copy_bytes` makes an owned
copy charged to the current request. `handler_deadline` gives the absolute
monotonic deadline used by the dispatcher.

`query_param(request, name)` looks up a decoded name and returns an owned
UTF-8 value. An absent name gives `Ok(None)`. A bare name or `name=` gives
an empty string. It splits pairs before decoding, keeps `+` and `;` literal,
and returns `Repeated` for a second occurrence of the decoded name. It
reports malformed escapes and invalid decoded text as `Escape` and `Utf8`.
Values of unrelated parameters are skipped.

`header_at(request, index)` borrows the actual header name and value at a
zero-based index in receipt order. Repeated fields retain their own positions.

`inline_body` borrows a short fixed-length body already in the read buffer.
Otherwise `read(mut body, mut buffer)` copies decoded body bytes into the
buffer's spare capacity and returns zero at EOF. The first read of a pending
`Expect: 100-continue` sends 100 before receiving more body data. The
connection remembers body framing errors even when the handler catches them.

The writer owns one fixed buffer and advances through an output unit by the
number of bytes the socket accepted. Write failures and expired deadlines
close the connection. Unread body bytes are drained under the request's drain
budget and deadline before the next head is parsed. A response without a
body still sends a complete head.

`response(status, take body, close)` builds a final response for status 200
through 599. `response_typed(status, format, value)` sends
`application/json` using a prepared [JSON format](json.md). It encodes the
value once and measures the exact length before the connection streams the
JSON through its output buffer. `header(mut response, take name, take value)` appends a field
after checking HTTP field syntax. It refuses Content-Length, Transfer-Encoding
Connection, Idempotent-Outcome and Hirsel-Refused case-insensitively with
`HeaderReserved`. Invalid status and field syntax return `StatusInvalid` and
`HeaderInvalid` respectively. Tagged runner routes use `mark_committed` to
report a committed reply and `mark_refused` to echo the request's
Idempotency-Key with a terminal refusal kind.
While encoding the response head, HTTP returns `DateInvalid` at the Date
field when the clock falls outside `std.time.from_unix_ms`. The refusal
occurs before the writer appends any part of that field.

`framing(bytes, head, body_max)` reads the parsed field offsets once and returns
`Empty`, `ContentLength(n)` or `Chunked`. It rejects TE+CL, repeated or listed
lengths, invalid decimal lengths, HTTP/1.0 TE and invalid transfer-coding
lists with a `BodyError { status, close: true }`. It reads transfer codings in
field order. A non-final `chunked` gives 400 and close (RFC 9112 §6.3), as
do empty or malformed coding fields and parameters on `chunked`. A valid
unknown coding gives 501 and close when no earlier framing error applies
(§6.1). A single `chunked` selects chunked framing. A valid length above the
body cap gives 413 and close. The selector allocates nothing and performs no I/O.

`decoder(frame, head, body_max)` starts fixed storage after successful framing.
`decode(mut decoder, input)` consumes a fresh input fragment and returns one
`Data(Span)` into it, `More`, `Done` or `Failed(BodyError)`. `decoder.used` is the
consumed prefix of that call's input, including framing bytes. Pass the
unconsumed suffix to the next call. A `Data` result can leave framing bytes for
the next call. `More` consumes the input and needs more bytes; `Done` preserves
the pipelined tail. `body_eof(mut decoder)` refuses an incomplete body.

`body_payload(decoder)` reports the current consecutive payload window.
It gives `None` when framing or completion needs `decode`, and `Some(0)`
when a close-delimited body has reached its limit and requires EOF.
`advance_body_payload(mut decoder, n)` accounts for payload delivered
directly into output. Call it only with a byte count within that window.

Chunk lines include CRLF in their 1024-byte bound. Chunk extensions follow RFC
9112 section 7.1.1's token and quoted-string syntax, then are discarded. Trailer
fields follow strict head-field syntax with CRLF and share the original head's
8192-byte and 64-field bounds. Excess trailers are 431 and close. The decoder
retains counts, not the Head or trailer values. A failure remains sticky across
further reads and EOF, so a handler catching it cannot restore reuse. All of
these operations allocate nothing and perform no I/O.

## Multipart form data

Call `std.http.multipart` with the request's Content-Type value and
`MultipartLimits`. Pass each decoded body block to `multipart_feed`.
For a streamed HTTP body, feed the bytes returned by `http.read`.
Call `multipart_finish` at EOF. It requires the closing delimiter, with
optional CRLF at EOF.

The boundary is case-sensitive, may be quoted and must satisfy RFC 2046
section 5.1.1. The parser discards preamble and epilogue. Each part needs
one `Content-Disposition: form-data` header with a `name` parameter.
The callback borrows ordered header fields, the name, an optional filename
and a binary body slice. Names and filenames retain their bytes after
quoted-pair decoding. The slice belongs to the parser until the callback
returns. The parser keeps the current part and any pending delimiter bytes.
It leaves content and transfer encodings for the caller.

`MultipartLimits.total` includes all decoded body bytes, even preamble,
delimiters and epilogue. `headers` counts each part's raw header block
including its terminating empty line. `part` counts each part's payload
bytes. Missing, oversized and malformed boundaries, a missing final
boundary, malformed headers and the three limit overflows have typed
errors. Each feed checks its total byte contribution before passing bytes
to callbacks. An error is sticky. Parts delivered before an error have
already been observed by the caller.
