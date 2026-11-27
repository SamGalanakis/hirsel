# WebSocket

[API overview](../API.md). [Operations and contracts](../websocket.hirsel).

The module is pure RFC 6455 protocol logic over bytes. It performs no I/O,
chooses no deadline, and generates no entropy. The socket owner supplies
random client nonces and a fresh four-byte mask for each encoded client frame.

`accept_head` checks a parsed opening head and returns the key span. A route
can pass a required subprotocol. `server` parses a bounded upgrade and returns
the response and consumed byte offset. It selects `hirsel.4` wherever it appears in the offer. A missing
header or an offer without `hirsel.4` gives HTTP 400. `client` checks the 101 challenge and
requires the selected `hirsel.4` protocol. A 4xx or 5xx answer fails with the
server's status, so a full place's refusal reads as 503; any other answer that
is not a valid 101 fails with 400. Preserve `input[end..]` as the
initial frame input. `request` constructs the fixed
`/hirsel` request and returns `Result[Bytes, u16]`. Refused 426 requires a
`Sec-WebSocket-Version: 13` response header. Allocation refusal raises Capacity.

`decoder` and `next` consume arbitrarily split input, stop at one event, and
advance a caller-owned cursor. A Ping is immediate even inside a fragmented
message; its inline payload must go to the sole writer as a Pong. Text and
binary messages are distinct events. Text UTF-8 state spans fragments and
control frames. A Close event contains a validated payload to echo. Reject and
Close terminate decoding and release partial assembly.

`encode` validates and appends one wire frame. `binary_head` appends the header
of an unmasked binary frame. Both return `Result[(), u16]` and raise Capacity
on refused allocation. The encoder tracks
fragmentation and close state. The owner retains that frame and its send
cursor until it is fully written, and flushes any Close under one absolute
close deadline. The driver also owns the composite upgrade deadline and idle
Ping/Pong timers. The runner applies its binary-only policy above this module.

Frames and assembled messages share the configured payload cap. The decoder
reserves nothing from a declared length. Its inline header is at most 14 bytes;
control payloads use 125 inline bytes. Arriving data grows one assembly buffer
by powers of two starting at four, capped at max_frame. The configured cap
must be a nonzero power of two. New storage is admitted before old
storage is refunded; each allocation rounds to eight bytes. For the default
power-of-two cap, growth peaks at 1.5 times that cap, plus rounding. With receive scratch S = 8192, the peak bound is
`g(k) = S + round8(cap(k)) + round8(cap(k)/2)`, where
`cap(k) = min(next_pow2(max(k, 4)), max_frame)`. Thus g(3) is 8208 bytes
and g(262144) is 401408 bytes. Wsock must fund its receive scratch before recv and account for
assembly growth, retained messages and decoding separately. Sending does not
transfer their charge.

The shape adapts tungstenite 89576c6's handshake and frame machines and
Goose's hypothetical sans-I/O server sketch in `docs/design/coroutines.md`.
The change from tungstenite is growth from received bytes rather than its
declared-length reservation. RFC and upstream hand vectors are the oracles.

`frame_count` reports completed physical frames, including non-final data
fragments when next returns Need. It allocates nothing and keeps the machine
pure. The socket driver uses this progress to renew idle on frames rather than
on trickled bytes or only on a completed message. The count uses checked u64
arithmetic, like other source counters.
