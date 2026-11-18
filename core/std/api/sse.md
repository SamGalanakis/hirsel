# Server-sent events

[HTTP](http.md). [Specification](../../../docs/spec/http.md#http-13-the-sse-encoder-whatwg-event-stream).

`buffer(bytes)` allocates a fixed-capacity event buffer. `event(mut out,
name, id, data)` appends one complete event. It refuses CR and LF in the
name and id, NUL in the id, and invalid UTF-8 data. Each CRLF, CR or LF
in the data starts a new data line.

`comment` and `retry` append complete control items. Each operation
validates and measures the whole item before writing its first byte.
An error leaves the buffer byte-identical. `into_bytes` moves the
encoded buffer to a stream producer's output without copying.
