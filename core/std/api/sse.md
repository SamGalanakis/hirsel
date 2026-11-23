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

`decoder(max_bytes)` creates a bounded EventSource parser whose buffers grow
as input arrives.
`feed(mut decoder, fragment, emit)` accepts arbitrary byte fragments,
including splits inside a UTF-8 scalar, a BOM or a CRLF. It calls `emit`
with borrowed UTF-8 bytes for the event name, folded data and last event
ID each time a blank line dispatches an event. The default name is
`message`. Invalid UTF-8 units become U+FFFD. Each line, folded data
buffer, event name and ID is bounded by `max_bytes`. A bound failure
returns `Error(Decode(TooLarge, bytes))` and ends parsing.

The callback returns a `FeedControl[E]`. Use `Continue` to keep parsing,
`Stop` to return `Ok(Stopped(bytes))`, or `Fail(error)` to return
`Error(Callback(error, bytes))` with the original error. When the decoder
reads the whole fragment, `feed` returns `Ok(Consumed(bytes))`.

Each byte count measures the consumed prefix of the fragment passed to
that call. On `Stop` or `Fail` it includes the event's blank line, including
both bytes of a CRLF present in that fragment. A CR at the end of a
fragment dispatches immediately, and the decoder skips a following LF if
fed again. After stopping or failing from the callback, the caller can
resume at the reported offset or discard the decoder.

`last_event_id(decoder)` borrows the ID committed by the latest blank
line. `retry_ms(decoder)` returns the latest ASCII-digit retry value in
milliseconds, if one was supplied. A retry value outside u64 ends
parsing with `TooLarge`. `finish(mut decoder)` discards an unfinished block at EOF
and keeps the committed ID and retry value. The caller decides when to
reconnect and what to send as Last-Event-ID.
