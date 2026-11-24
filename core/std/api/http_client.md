# HTTP/1.1 client

[API overview](../API.md). [Operations and contracts](../http_client.hirsel).

`Request(method, target, headers, body)` describes one origin request. The
target is an origin-form path and optional query, or `*` for OPTIONS.
Each `Header(name, value)` has a token name. Use `Public(bytes)` for an
ordinary field and `Protected(prefix, credential)` for a field ending in a
`Secret`. For example, the public prefix for bearer authorization is
`string.as_bytes("Bearer ")`. The credential's source must supply bytes
suitable for an HTTP field value. The connection writes Host from its
explicit authority, including a port when supplied. The body mode is
`Fixed(length)`, `Chunked` or `Segments(list)`, a whole body given with the
request. A `Segment` is `Plain(bytes)` or `Sealed(credential, coding)`, whose
`Secret` the runtime writes `Raw` or `Form`-urlencoded. An OAuth refresh body is
`Segments([Plain("grant_type=refresh_token&refresh_token="), Sealed(token, Form)])`
with a `Content-Type: application/x-www-form-urlencoded` field
([HTTP-17](../../../docs/spec/http.md#http-17-client-request-bodies)). The writer owns the
framing fields and refuses caller-supplied Host, Content-Length,
Transfer-Encoding, Trailer, Expect and Upgrade. A protected Connection field
ends reuse because the client cannot tell whether it asks the peer to close.

`idempotency_key(mut headers, key)` adds a quoted hexadecimal `Idempotency-Key`
field for binary keys. It refuses an empty key or a duplicate field.

`dial(net, address, authority, clock, request, deadline_ns)` connects to a
numeric `Addr` under the caller's Net grant and writes the first request
head. `attach` takes an already connected socket and writes the same head.
`https(net, address, authority, name, trust, clock, request, deadline_ns)`
also verifies the TLS peer for `name` under the supplied `trust` policy and
offers HTTP/1.1 through ALPN. The address, Host authority and TLS verification
name are explicit. Both transports return the same `Connection`.
`open(net, address, authority, clock, deadline_ns)` and
`open_tls(net, address, authority, name, trust, clock, deadline_ns)`
return an idle connection before sending a request.
`start(mut connection, request, deadline_ns)` writes another request on a
reusable connection. `write(mut connection, bytes)` sends a body fragment.
`write_segments(mut connection, segments)` sends one chunk of a `Chunked`
upload made of segments; a chunk with no plain byte is `InvalidRequest`,
and so is `write_segments` on a `Fixed` upload. A `Segments` body is sent by
`start`, so `write` on it is `State`.
`finish_request(mut connection, body_max)` completes the upload and reads
the final response head. A fixed-length upload must send exactly its
declared length. A chunked upload sends one chunk per nonempty write and
ends with a zero chunk.

`head(connection)` borrows the current `ResponseHead`. Its status, HTTP
minor version, reason and ordered fields remain available during body reads.
Field names and values retain their wire spelling. The parser skips interim
1xx responses and refuses protocol switching. HEAD, 204 and 304 have no
body. Other responses use Content-Length, chunked coding, or a clean EOF
from the transport.
The client refuses other transfer codings.

`read(mut connection, mut into)` appends up to the supplied buffer's spare
capacity and returns the number of bytes appended. Zero means body EOF.
A buffer with no spare capacity, such as a fresh `list.new()`, returns
`NoSpace` without reading; reserve room first with `list.with_capacity`.
After `NoSpace`, supply another buffer or clear the current one to resume.
Pass a buffer with spare capacity on the call that checks for EOF.
`body_max` bounds the total decoded bytes, including for close-delimited
bodies. The connection copies payload already buffered during the head read
once. It receives subsequent payload directly into the caller's buffer,
limited by the current payload span and spare capacity. Body reads allocate
no storage. The reader has 8192 bytes of wire storage and 8192 bytes of head
storage, with up to 64 fields in one head. It consumes chunk extensions and
trailers without returning them as body data. Read to EOF before starting
another request. The client keeps bytes read ahead for the next exchange and
reports whether `reusable(connection)` permits it. Close-delimited bodies
and `Connection: close` end reuse.

The absolute monotonic deadline covers connect, the TLS handshake, upload,
the response head
and every body read. The owning task can be cancelled with `task.cancel`.
`close(take connection)` releases both socket halves and may abandon a
partial request or response. Errors distinguish invalid requests, connection
state, insufficient output space, connect, write, read,
malformed head, protocol switching, body framing, size limits, deadline and
cancellation. A refused allocation raises `Capacity`. A wire or deadline
failure makes the connection unusable. Writes can have committed a prefix on failure.
`request_started(connection)` reports whether the last `start` committed
any request bytes. It also reports true if a send carrying a Secret, in a
protected header or a protected body segment, failed: the client cannot
distinguish zero from a partly committed send.

The transport-free `Reader` accepts bounded fragments with `feed` and an
EOF signal with `end_input`. `parse_head` reports when a final head is
available. `read_buffered` reports `NeedInput`, `Data(n)` or `End` and
preserves the next message's bytes. The client leaves DNS, redirects, cookies
and pooling to callers.

The writer sends a request with protected fields or protected body segments
as one [`tls.send_secret`](tls.md) message, so the runtime writes the
`Content-Length` or chunk size that counts the secrets. Every secret must cover
the stream's name before any byte is sent; otherwise, and on plain TCP, the
request gives `Write(Denied)` with nothing sent. `encode_request` writes public
heads into a caller-supplied buffer and also refuses protected fields and
segments; for an all-plain `Segments` body it writes the head and the caller
sends the body. The 8192-byte head buffer holds the public bytes of the head
and of a body that holds a secret. Secret bytes go directly to the TLS provider.

`render_request` writes the method, target and supplied fields to a
caller-supplied byte buffer. `render_error` writes the HTTP error category
followed by the same request description. Both write `[secret]` in place
of credential bytes.

For an event-stream response, feed the decoded body bytes to the
[server-sent events decoder](sse.md).
