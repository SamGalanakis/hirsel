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

## Decoding

`decoder(max_bytes)` creates a bounded EventSource parser.
`feed(mut decoder, fragment, emit)` accepts arbitrary byte fragments,
including splits inside a UTF-8 scalar, a BOM or a CRLF. It calls `emit`
with borrowed UTF-8 bytes for the event name, folded data and last event
ID each time a blank line dispatches an event. The default name is
`message`. Invalid UTF-8 units become U+FFFD. Each line, folded data
buffer, event name and ID is bounded by `max_bytes`. A bound failure
returns `TooLarge` and ends parsing.

`last_event_id(decoder)` borrows the ID committed by the latest blank
line. `retry_ms(decoder)` returns the latest ASCII-digit retry value in
milliseconds, if one was supplied. A digit value outside u64 returns
`TooLarge`. `finish(mut decoder)` discards an unfinished block at EOF
and keeps the committed ID and retry value. The caller decides when to
reconnect and what to send as Last-Event-ID.
