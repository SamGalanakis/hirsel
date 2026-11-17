# HTTP

[API overview](../API.md). [Specification](../../../docs/spec/http.md).

The pure HTTP-4 request framing and body decoder are built. The server is not
built yet. Its rules are HTTP-1 to HTTP-15 and the stream
ownership rules of the [HTTP specification](../../../docs/spec/http.md).

`serve(take listeners, clock, cfg, router, state)` serves HTTP/1.1 and
HTTP/1.0 on listeners from `Net`. One supervisor task owns every socket. Each
connection has one task that holds the only parser, framing and writer, and it
runs at most one handler at a time. `Config` is a plain record with every
bound required. Every population (connections, stream groups, upgraded
protocols and refusals) has its own cap and is admitted at spawn, so no live
connection meets `Capacity` from an admitted workload. A socket refused for
capacity gets a static 503 while a refusal slot is free, and otherwise a bare
close.

Heads are parsed once and resumably under fixed bounds. Framing follows RFC
9112: a request with both `Content-Length` and `Transfer-Encoding` gets 400.
Every phase has one absolute deadline, computed once from a recorded clock read
and never moved by progress.
A handler returns an outcome value: a complete response, a `Stream` that the
supervisor hands to a stream group with its encoded state, or an `Upgrade` that
hands the socket and its unread bytes to a protocol task. Errors are values.
The SSE encoder writes whole events only; a failed or partial append publishes
nothing.

Runner bindings (one transport-neutral ingress, `HttpBinding` values, JSON
forms, statuses, the HTTP relay and SSE subscriptions) are rules B-1 to B-12.
Keyed operations (K-1 to K-10) are a separate, unbuilt part.

`framing(bytes, head, body_max)` reads the parsed field offsets once and returns
`Empty`, `ContentLength(n)` or `Chunked`. It rejects TE+CL, repeated or listed
lengths, invalid decimal lengths, HTTP/1.0 TE and unsupported codings with a
`BodyError { status, close: true }`. Framing syntax refusals are 400, a valid
length above the body cap is 413, and unsupported HTTP/1.1 codings are 501.

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
