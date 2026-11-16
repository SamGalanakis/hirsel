# HTTP/1 heads

[API overview](../API.md). [Operations and contracts](../http1.hirsel).

`head` creates fixed storage for 64 header fields. `request` and `response`
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
upgrade rejects body framing before interpreting its tail as frames.

The request laws port httparse a0fa552e's 29 request vectors and the empty-input
control from revision 64d4d617. The Go ebe2feefc400 heads retain their raw
inputs and parsed fields; expectations for the old adapter's URL and body
policies are intentionally excluded. Response coverage includes the upstream
101 with a tab in its reason. References are in each test.
