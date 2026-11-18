# HTTP

[API overview](../API.md). [Specification](../../../docs/spec/http.md).

`serve(take listeners, clock, tasks, take config, take routes, take state)`
serves HTTP/1.1 and HTTP/1.0 on listeners from `Net`. The supplied
`Tasks` capability owns the acceptors, connection tasks and refusal
workers. The supervisor reserves the refusal workers at startup, admits
each connection at spawn, and keeps its reservation through the last
request and linger. It counts accepted peers before reading a head and
moves the per-IP count after validating a trusted forwarding field.

Build `routes` with [router](router.md). A raw handler receives an anchored
`Request`, a scratch `Region`, a mutable `Responder` and a read-only
view of shared state. The region's allocations
count against `handler_bytes` and the region closes when the handler exits.
Copy data into owned values to keep it in a reply. The handler returns
`Reply(Response)` or an error.
One task parses, routes, handles and writes each request in turn.

`method`, `target`, `path`, `query` and `field` borrow request bytes.
`segments` borrows the decoded path segments. `capture` borrows a named
single segment. `capture_tail` borrows a named final capture as a slice of
decoded segments. `copy_bytes` makes an owned
copy charged to the current request. `handler_deadline` gives the absolute
monotonic deadline used by the dispatcher.

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
through 599. `header(mut response, take name, take value)` appends a field
after checking HTTP field syntax. It refuses Content-Length, Transfer-Encoding
and Connection case-insensitively with `HeaderReserved`. Invalid status and
field syntax return `StatusInvalid` and `HeaderInvalid` respectively.

`framing(bytes, head, body_max)` reads the parsed field offsets once and returns
`Empty`, `ContentLength(n)` or `Chunked`. It rejects TE+CL, repeated or listed
lengths, invalid decimal lengths, HTTP/1.0 TE and unsupported codings with a
`BodyError { status, close: true }`. Framing syntax refusals are 400, a valid
length above the body cap is 413, and unsupported HTTP/1.1 codings are 501.
The selector allocates nothing and performs no I/O.

`decoder(frame, head, body_max)` starts fixed storage after successful framing.
`decode(mut decoder, input)` consumes a fresh input fragment and returns one
`Data(Span)` into it, `More`, `Done` or `Failed(BodyError)`. `decoder.used` is the
consumed prefix of that call's input, including framing bytes. Pass the
unconsumed suffix to the next call. A `Data` result can leave framing bytes for
the next call. `More` consumes the input and needs more bytes; `Done` preserves
the pipelined tail. `body_eof(mut decoder)` refuses an incomplete body.

Chunk lines include CRLF in their 1024-byte bound. Chunk extensions follow RFC
9112 section 7.1.1's token and quoted-string syntax, then are discarded. Trailer
fields follow strict head-field syntax with CRLF and share the original head's
8192-byte and 64-field bounds. Excess trailers are 431 and close. The decoder
retains counts, not the Head or trailer values. A failure remains sticky across
further reads and EOF, so a handler catching it cannot restore reuse. All of
these operations allocate nothing and perform no I/O.
