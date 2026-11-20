# HTTP/1.1 client

[API overview](../API.md). [Operations and contracts](../http_client.hirsel).

`Request(method, target, headers, body)` describes one origin request. The
target is an origin-form path and optional query, or `*` for OPTIONS.
Each `Header(name, value)` has a token name and an opaque byte value. The
connection writes Host from its explicit authority, including a port when
supplied. The body mode is `Fixed(length)` or `Chunked`. The writer owns the
framing fields and refuses caller-supplied Host, Content-Length,
Transfer-Encoding, Trailer, Expect and Upgrade.

`dial(net, address, authority, clock, request, deadline_ns)` connects to a
numeric `Addr` under the caller's Net grant and writes the first request
head. `attach` takes an already connected socket and writes the same head.
`start(mut connection, request, deadline_ns)` writes another request on a
reusable connection. `write(mut connection, bytes)` sends a body fragment.
`finish_request(mut connection, body_max)` completes the upload and reads
the final response head. A fixed-length upload must send exactly its
declared length. A chunked upload sends one chunk per nonempty write and
ends with a zero chunk.

`head(connection)` borrows the current `ResponseHead`. Its status, HTTP
minor version, reason and ordered fields remain available during body reads.
Field names and values retain their wire spelling. The parser skips interim
1xx responses and refuses protocol switching. HEAD, 204 and 304 have no
body. Other responses use Content-Length, chunked coding, or TCP EOF.
The client refuses other transfer codings.

`read(mut connection, mut into)` appends up to the supplied buffer's spare
capacity and returns the number of bytes appended. Zero means body EOF.
`body_max` bounds the total decoded bytes, including for close-delimited
bodies. The reader stores at most 8192 wire bytes and 8192 bytes of head,
with up to 64 fields in one head. It consumes chunk extensions and trailers
without returning them as body data. Read to EOF before starting another
request. The client keeps bytes read ahead for the next exchange and
reports whether `reusable(connection)` permits it. Close-delimited bodies
and `Connection: close` end reuse.

The absolute monotonic deadline covers connect, upload, the response head
and every body read. The owning task can be cancelled with `task.cancel`.
`close(take connection)` releases both socket halves and may abandon a
partial request or response. Errors distinguish invalid requests, connection
state, insufficient output space, allocation, connect, write, read,
malformed head, protocol switching, body framing, size limits, deadline and
cancellation. A wire or deadline failure makes the connection unusable.
Writes can have committed a prefix on failure.

The transport-free `Reader` accepts bounded fragments with `feed` and an
EOF signal with `end_input`. `parse_head` reports when a final head is
available. `read_buffered` reports `NeedInput`, `Data(n)` or `End` and
preserves the next message's bytes. The same parser and writer will be used
with TLS through a [host provider](../../../docs/spec/tls.md). The TCP client
uses a supplied address and authority and leaves DNS, redirects, cookies and
pooling to callers.

For an event-stream response, feed the decoded body bytes to the
[server-sent events decoder](sse.md).
