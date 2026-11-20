# HTTP/1 heads

[API overview](../API.md). [Operations and contracts](../http1.hirsel).

`head` creates fixed storage for 64 header fields. `cursor` creates the fixed
request scan state. `resume(b, mut h, mut cur)` receives the growing buffer,
with its examined prefix unchanged, beside a head and cursor initialized once
per request. It retains partial token, UTF-8 and CRLF state and never rereads a
head byte. `cur.examined` counts actual byte reads. Complete, Bad and TooLarge
are sticky until the caller initializes a new head and cursor. `cur.line_end`
is zero until the request line ends, and `cur.line_start` excludes initial empty
lines. TooLarge maps to 414 only when the request line alone fills 8192 bytes
(`line_end == 0` and `offset - line_start == 8192`); every other head/field
overflow maps to 431.

`request` and `response`
reset that storage and parse one bounded head. `Complete` sets `end` to the
first byte after the head; bytes beyond it remain available to the next
protocol. `Partial` preserves initialized field offsets. `Bad` is malformed
input; `TooLarge` means more than 8192 head bytes or 64 fields. Neither parser
allocates, decodes a body, interprets a URL, or gives a request authority.

Strict httparse defaults are preserved: LF-only and CRLF lines, initial empty
lines, token header names, opaque field values, trimmed surrounding optional
whitespace, no folding, no leading header whitespace. Request targets must be
UTF-8. Offsets preserve field spelling, order and duplicates. Body framing and
URL checks in the old Go HTTP adapter are outside this head parser; the
upgrade rejects body framing before interpreting its tail as frames. The HTTP client selects response framing
after the status head parses. [HTTP client](http_client.md)

`std.http.framing` interprets request framing after the head parser completes.
It rejects a non-final `chunked` transfer coding with 400 and close
([RFC 9112 §6.3](https://www.rfc-editor.org/rfc/rfc9112#section-6.3)).
A syntactically valid unknown coding gives 501 and close when no earlier
framing error applies ([§6.1](https://www.rfc-editor.org/rfc/rfc9112#section-6.1)).
Repeated Transfer-Encoding fields retain their order for this check.

The request laws port httparse a0fa552e's 29 request vectors and the empty-input
control from revision 64d4d617. The Go ebe2feefc400 heads retain their raw
inputs and parsed fields; expectations for the old adapter's URL and body
policies are intentionally excluded. Response coverage includes the upstream
101 with a tab in its reason. References are in each test.

`token(c)` reports whether a byte is permitted in an HTTP field name.
