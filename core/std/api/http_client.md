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
preserves the next message's bytes. The connection-level operations above leave DNS,
redirects, decoding and pooling to callers. [Fetch](#fetch) below adds them in one
call. Cookies stay with callers.

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

## Fetch

The normative rules are [HTTP-18](../../../docs/spec/http.md#http-18-client-fetch-redirects-decoding-and-reuse).

`fetch` sends one request to an `http` or `https` URL. It resolves the name, reuses an idle
connection or opens one, follows redirects, decodes a gzip, deflate or zstd body, and
returns the final response. It is built from the connection operations above, which stay
the tool for streaming uploads, `Upgrade` and anything else `fetch` does not do.

```
pub type Redirects: key + send = Follow(u8) | Return
pub type Limits: send {
    redirects: Redirects,
    body_max: u64,            // decoded body bytes
    max_window: u64,          // zstd window a response may demand
    idle_max: u16,            // idle connections kept, in total
    idle_per_origin: u16,     // idle connections kept, per origin
    idle_ns: u64,             // how long an idle connection may be reused
    decode: bool              // offer and remove gzip, deflate and zstd
}
pub fn limits(body_max: u64) -> Limits
pub opaque type Client: send
pub fn client(n: Net, c: Clock, take trust: Trust, cfg: Limits) -> Client
pub type Fetch: send { method: String, url: String, headers: List[Header], body: List[Segment] }
pub type ContentCoding: key + send = Gzip | Deflate | Zstd
pub type CodingError: send = Unsupported | Flate(flate.FlateError) | Zstd(zstd.ZstdError)
pub type FetchError: send = Http(Error) | Url(uri.Error) | Scheme | UserInfo | Resolve(NetError) | NoAddress | Location | TooManyRedirects | CrossOriginSecret | Coding(CodingError)
pub opaque type Response: send
pub fn fetch(mut cl: Client, req: Fetch, deadline_ns: u64) -> Result[Response, FetchError] waits
pub fn fetch_head(r: Response) -> View[ResponseHead] in r
pub fn fetch_url(r: Response) -> View[String] in r
pub fn fetch_coding(r: Response) -> Option[ContentCoding]
pub fn fetch_read(mut r: Response, mut into: Bytes) -> Result[u64, FetchError] waits
pub fn fetch_done(mut cl: Client, take r: Response)
```

```hirsel
let mut cl = std.http_client.client(n, c, System, std.http_client.limits(1 << 20))
let req = Fetch(method: "GET", url: "https://api.example.com/v1/items", headers: [Header(name: "Authorization", value: Protected(string.as_bytes("Bearer "), token))], body: list.new())
let mut r = std.http_client.fetch(mut cl, req, host.mono_ns(c) + 10_000_000_000)?
let mut buf: Bytes = list.with_capacity(65536)
while std.http_client.fetch_read(mut r, mut buf)? != 0 { if list.len(buf) == list.capacity(buf) { list.reserve(mut buf, list.capacity(buf)) } }
std.http_client.fetch_done(mut cl, take r)
```

**Limits.** `limits(body_max)` gives the derived defaults and the caller's body bound,
which has no universal value:

| Field | Default | Derivation |
|---|---|---|
| `redirects` | `Follow(10)` | Go (`stopped after 10 redirects`), reqwest (`Policy::limited(10)`) and Python (`max_redirections = 10`) all stop at 10. RFC 9110 §15.4 asks only that a client detect loops, and 10 is the common bound. |
| `max_window` | 8,388,608 (2^23) | RFC 9659 §3: for the zstd content coding, decoders MUST support windows up to 8 MB and encoders MUST NOT exceed it. 2^23 covers 8 MB under either reading, so no conforming response is refused. |
| `idle_max` | 100 | Go's `DefaultTransport.MaxIdleConns`. |
| `idle_per_origin` | 2 | Go's `DefaultMaxIdleConnsPerHost`. |
| `idle_ns` | 90 s | Go's `IdleConnTimeout` and reqwest's `pool_idle_timeout`. |
| `decode` | `true` | |

**Requests.** `url` is an absolute RFC 3986 URI ([uri](uri.md)) with scheme `http` or
`https` (`Scheme` otherwise) and no userinfo (`UserInfo`). Credentials go in `Protected`
headers, never in a URL that a redirect, a render or a log could copy. The fragment is
not sent. The target is the path (`/` when empty) and the query. `body` is the whole body
as segments ([HTTP-17](../../../docs/spec/http.md#http-17-client-request-bodies)), so
`fetch` can send it again on a 307 or 308 redirect or a retry. An empty body sends no body
for GET, HEAD, DELETE, OPTIONS and TRACE, and `Content-Length: 0` for any other method
(RFC 9110 §8.6). The header rules are `start`'s.

**Reading.** `fetch` returns once the final response head is read. `fetch_head` borrows it,
with the wire's fields unchanged, including `Content-Encoding` when a coding was removed.
`fetch_coding` says which coding was removed. `fetch_url` is the URL of the last hop.
`fetch_read` is `read`'s contract (spare capacity, `NoSpace`, `Ok(0)` at the end) over the
decoded body. `fetch_done` returns the connection to the pool when the body was read to
its end and the connection is reusable, and closes it otherwise. Dropping a `Response`
closes its connection.

- **Authority.** `n` with a `Resolve` rule covering every host name the request and its
  redirects reach, and a `Connect` rule covering every address and port
  ([net.md](../../../docs/spec/net.md)). A redirect to a host the grant does not cover is
  `Resolve(Denied)`, or `Http(Connect(Denied))` for an address. `c` reads deadlines and
  idle ages. `trust` verifies every `https` hop. A `Protected` header or `Sealed` segment
  still passes TLS-5's sink check on each hop's verified name.
- **Failures.** Each `FetchError` names one cause. `Http(e)` is the connection's own error
  on any hop, including `TooLarge` for either body bound and `Deadline`. `NoAddress` means
  the name resolved to no address. `Location` is a redirect whose `Location` field is
  repeated, is not a URI reference, or resolves to a URL `fetch` refuses.
  `TooManyRedirects` and `CrossOriginSecret` are HTTP-18's. `Coding(e)` is a body that
  cannot be decoded. A refused allocation raises `Capacity`. Cancellation and the
  deadline end the fetch as on a connection, and the response's connection is closed.
- **Charging.** `client` takes ownership of its trust value and admits nothing. A
  new connection admits three 8192-byte buffers for wire data, the response head and
  request encoding, plus its authority string. Each hop admits its URL and rebuilt
  headers. A decoded response admits an 8192-byte encoded-input buffer and decoder
  storage. Flate uses a 32 KiB window and fixed tables. Zstd uses at most `max_window`
  of history, fixed tables and buffers for blocks of at most 128 KiB (RFC 8878
  §3.1.1.2.4). Idle list storage and retained origin keys are admitted under the limit
  current when they are allocated.

  For example, an inner `bound bytes` scope can fetch and return a connection to an
  outer client. Its surviving allocations stay charged when the scope exits, with
  their records leading to the parent under MEM-20. A sibling bound can use the
  connection. The charges are refunded when the storage is freed.

  A hop takes O(header bytes + URL bytes) steps of its own, plus host calls. The
  pool compares the origins of at most `idle_max` entries per selection or return.
  Selection compacts removed entries in one linear pass. Decoding is O(encoded input
  + decoded output).
- **Replay.** `fetch` passes no replay point of its own. Each resolve, connect, send,
  receive and Clock read passes its own, and decoding is recomputed. Pool choices depend
  only on those values, so a replay reuses the same connections.

### Prior art

- Go `net/http` (`/tmp/ref-go`, go 8eaf19e): `redirectBehavior`
  (`src/net/http/client.go:496`), the stop after 10 (`client.go:839`), dropping content
  headers when the body is dropped (`client.go:822-823`), draining at most `2 << 10` bytes
  of a redirect body (`client.go:717`), sensitive headers kept for subdomains
  (`shouldCopyHeaderOnRedirect`, `client.go:1009`), pool defaults
  (`src/net/http/transport.go:54-55,62`), gzip only and never for HEAD or ranges
  (`transport.go:2395-2406`), and retry of replayable requests on a reused connection
  (`transport.go:847-852`, `isReplayable` at `src/net/http/request.go:1562`).
  ADOPTED: the limit of 10, the drain bound, the pool defaults, the HEAD and range
  exceptions, and the retry rule. REPLACED: Go turns every non-GET/HEAD method into GET on
  301 and 302. Here only POST changes, as RFC 9110 §15.4 describes. REPLACED: Go forwards
  `Authorization` and `Cookie` to subdomains. Here any change of origin strips them.
- reqwest (`/tmp/ref-reqwest`, reqwest 11ea959): `Policy::limited(10)`
  (`src/redirect.rs:163`), `remove_sensitive_headers` on a change of host, port or scheme
  (`redirect.rs:239-252`), `pool_idle_timeout` 90 s (`src/async_impl/client.rs:306`), and
  gzip, zstd and deflate decoding behind features (`async_impl/client.rs:64-67`).
  ADOPTED: the exact-origin rule and the three codings. ADAPTED: reqwest strips a fixed
  list. Here every `Protected` header is stripped too, because its type says it holds a
  credential.
- Python `urllib.request` (`/tmp/ref-cpython`, cpython 32b51e3,
  `Lib/urllib/request.py:621-624`): `max_repeats = 4` and `max_redirections = 10`, and
  content headers dropped on a redirect (`request.py:652`). NOT TAKEN: the per-URL repeat
  count, since the hop limit already ends a loop.
- Goose (`/tmp/ref-goose`, goose d5ae5bb) has no HTTP client.

### Open

- **One owner per client.** `Client` is `send` and used through `mut`, so one task owns a
  pool. A server whose many tasks fetch either gives each task a client or runs a client
  task behind a channel. A shared pool would need a core primitive, and that decision
  belongs to the concurrency spec.
- **Encoded-bytes allowance.** The fixed 131,072-byte framing allowance (HTTP-18) refuses a
  gzip response with larger optional header fields, or with many tiny members. No server
  in common use sends one. Raising the allowance is reversible.
- **`x-gzip` and raw deflate.** `x-gzip` is accepted as `gzip` (RFC 9110 §8.4.1.3), and
  `deflate` must be zlib-wrapped (RFC 9110 §8.4.1.2), so raw deflate is refused as
  `Coding`. Some old servers send raw deflate, and browsers fall back to it. Never
  offering `deflate` in `Accept-Encoding` would avoid the issue, and that change is
  reversible.
