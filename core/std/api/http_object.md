# Serving an object over HTTP

[HTTP](http.md). [Declarations](../http_object.hirsel). [Specification](../../../docs/spec/http.md#http-10-outcomes).

`serve(request, opener, take access, limits)` answers one GET for one stored
object under RFC 9110 range semantics and relays its body from the store's
connection, so a large object never sits in memory. The route needs a
`stream_bytes` allowance.

The route decides access first and passes it as a value:
`Granted(key)` names the object the caller may read, and `Denied` is
answered 403 before anything is sent to the store. A route that tells an
unauthenticated caller 401 answers that itself and does not call `serve`.

A GET with exactly one `Range` field holding one `bytes=` range
(`first-last`, `first-` or `-suffix`) is ranged. Any other Range, including
several ranges, is ignored and the whole object is served. `If-Range` with
the object's strong ETag keeps the range; with another tag, a weak tag or a
date, the whole object is served.

| Case | Answer |
| --- | --- |
| A satisfiable range | 206 with `Content-Range: bytes first-last/size` |
| No range, or one ignored | 200 with the whole object |
| A range past the object | 416 with `Content-Range: bytes */size` |
| No such key | 404 |
| The store fails before its head | 504 for a deadline, else 502 |
| `Denied` | 403, with no store request |

A 200 or 206 carries `Accept-Ranges: bytes`, the store's ETag and its
`Content-Type` and `Last-Modified`, and no other store field.
`Limits.room` bounds the relayed stream; `open_ms` bounds opening the
object and should end before the route's handler deadline; `idle_ms`
bounds each body read.

`parse_range(value)` reads a Range field value into an
[`object.Want`](object.md), or `None` when the field is to be ignored.

```hirsel
type Blobs: send {
    opener: std.object.Opener,
    token: String
}

fn blob(req: std.http.Request, scratch: Region, mut out: std.http.Responder, s: Blobs) -> Result[std.http.Outcome, std.http.HandlerError] waits {
    let access: std.http_object.Access = match (std.http.field(req, b"authorization"), std.http.capture(req, "key")) {
        (Some(token), Some(key)) if token == string.as_bytes(s.token) => Granted(string.from(key)),
        _ => Denied,
    }
    let room = std.stream.Bounds(event_max: 65536, producer_bytes: 262144, producer_steps: 1000000)
    std.http_object.serve(req, s.opener, take access, std.http_object.Limits(room, open_ms: 2000, idle_ms: 10000))
}
```

## Serving a local body source

`serve_source(request, take source, room, random, expires_at_ms, take guard)`
serves an authorized, opened representation. Supply its length, strong ETag,
optional filename and pull endpoint in `Source`. Keep the same representation
available for the lifetime of the endpoint. A pull requests a byte offset and
a maximum length. Reply with a nonempty prefix or a failure. Treat closure of
the endpoint as the end of the transfer.

A matching strong `If-Match` permits the request. A false one returns 412.
A matching `If-None-Match`, using weak comparison, returns 304. Both fields
accept a tag list or `*`. A single satisfiable byte range returns 206 with
`Content-Range`. Several satisfiable ranges return a fixed-length
`multipart/byteranges` body in request order. A valid range set with no
satisfiable bytes returns 416 with the representation size. Invalid ranges
and ranges with a false `If-Range` receive the full representation. HEAD
sends the full representation's metadata without requesting bytes.

Body-bearing responses carry `Content-Length`. Every response about the
opened representation carries its ETag, `Accept-Ranges: bytes`,
`X-Content-Type-Options: nosniff`, `Content-Disposition: attachment` and
`Content-Security-Policy: default-src 'none'; sandbox`. The optional
disposition filename is limited to 128 ASCII bytes. The helper strips leading
dots and spaces and replaces path separators, quotes, controls and other
unsafe bytes with underscores.

The stream admits one read per credit. It closes on read failure, expiry or
revocation. When a stream stops early, the bytes sent fall short of its
advertised length.
