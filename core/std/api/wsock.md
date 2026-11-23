# WebSocket socket driver

`source` selects the normalized IP key used by admission. `Prefix` is trusted
ingress policy, separate from Net's direction and port authority. Untrusted
peers' forwarded fields are ignored. Trusted peers must supply exactly one
X-Forwarded-For field with one canonical IP literal of at most 256 bytes.
Missing, repeated, list-valued or malformed fields return HTTP 400. IPv4
mapped IPv6 addresses share their IPv4 key. IPv6 spelling follows RFC 5952.
The parser uses eight inline words and allocates nothing.

This adapts Go's reverse proxy replacement policy and Rust's typed IP and
prefix comparisons. Goose's hypothetical socket sketch supplies no ingress
policy. Deployment examples in deploy/wire replace client-provided XFF.

`defaults` supplies max_frame 262144, pending_max 1024 and upgrade/Hello/
close/idle durations 5000/10000/5000/30000 milliseconds. `pending` allocates
one bounded token channel. `acquire` returns a Pending resource or None for
HTTP 503. The resource returns its token on timeout, refusal, allocation
failure or cancellation. The runner calls `hello_accepted` only after decoding
and admitting Hello; `welcomed` clears the separate Hello deadline.

`server` consumes a Socket and pending lease, validates the bounded request
and source, writes the 101 response and returns `Upgraded`. Its reader keeps
only the request's frame read-ahead, copied into storage of exactly its length
and charged to the connection; the 8192-byte request buffer is released. Its timers start Hello at the completed 101. `server_with` calls a
source policy after sanitization and before 101, under the original accept
deadline; refusal is 503. `hello_deadline` exposes the fixed Hello deadline to
the runner owner. `refuse_socket` sends a bounded HTTP refusal while keeping
its socket halves inside wsock. `client` uses sixteen
bytes from host.random for its challenge, requires hirsel.3, rejects extensions,
and keeps response read-ahead the same way. It returns `Connected` and a client encoder.

`read` drives the pure decoder over the ReadHalf. An idle reader holds no
receive buffer: `refill` returns its input, waits with `host.recv_ready`
([NET-1](../../../docs/spec/net.md#net-1-socket-operation-declarations)), and
only then checks out an 8192-byte loan for one `recv_bounded`. The loan is
charged to the limit current in the calling task (MEM-19, MEM-20), so a
connection pays for a loan only while it holds one; there is no shared buffer
pool. A receive that times out before the deadline is stale readiness: the
loan returns and the reader waits again. A refused loan is `Allocation`.
`settle` trades a loan that still holds unread read-ahead for storage of only
the unread tail, for an owner about to park the reader.
An incomplete message has a 30-second deadline from its first partial frame.
Arriving bytes and interleaved controls leave that deadline fixed. Expiry
returns `Protocol(1001)`.
`decode_buffered` and `refill` separate parsing from socket waits. Assembly
allocates in the task calling `decode_buffered`. The runner moves the reader to
its shared transport allocation task for this step, refills in the reader
child, and settles there before waiting for read credit, so loans and tails
are charged to the reader child. Moving the resulting message preserves its allocation charge.

`outbound` and `encode` run at the allocating owner. The client encoder requests
four fresh host.random bytes on every frame, including controls. An encoded
buffer reserves exactly payload+14 capacity, at most round8(max_frame+14)
charged bytes. `try_encode_with` is the server's encode for a codec value of known
exact length: `websocket.binary_head` writes the header and the codec writes the
payload behind it in the same buffer, so the payload is never copied. The initial reservation returns Allocation
on refusal. A codec
that writes any other length fails closed with 1011. `stage` moves one such buffer to the sole Writer. Busy returns
the offered bytes unchanged. `flush` commits send prefixes and retains its
cursor across TimedOut. Completion frees the buffer before the next encode.
`pong` records an inline control debt, even while a frame is blocked. It drains
after that frame and before accepting new data. Repeated Pings may coalesce to
the latest payload as RFC 6455 5.5.2 permits. The WriteBuffer operations expose
the same machine without sockets for the shared protocol-law harness.

`timers`, `activity`, `closing`, `close_sent`, `due` and `until` keep the owner's
protocol state. until chooses the earliest protocol or caller deadline. Activity
renews idle without changing Hello or Close. due emits an idle Ping, then Close
1001 after another silent interval. closing fixes its first absolute deadline;
further Close events never extend it. The owner gives that deadline to both
children, flushes Close through the writer, then cancels and joins the children
at expiry. A validated peer Close plus a flushed Close completes immediately.
After the reader returns ownership, wsock.finish calls net.finish with the same absolute deadline. It shuts down output and drains raw input before final TCP close, preserving the flushed Close when input remains unread. At deadline, cancellation and joins release any children still holding halves.

`core/programs/wire/echo.hirsel` is the text/binary protocol endpoint on loopback
18089. Its Autobahn configuration uses a 16 MiB frame cap and a 96 MiB reader reservation, bounds its notice channel at one item,
and cancels and joins its reader and close timer. Its client_probe entry is an
independent entropy/handshake/masking fixture against port 18090. The echo's
protocol upgrade releases pending admission immediately; it has no runner Hello.
The runner default remains 256 KiB, including its 1009-above-cap law. The protocol suite uses the explicitly configured 16 MiB cap.

Prior art: ADAPTED Goose coroutines.md:975-1012's parser/I/O separation, replacing
its hypothetical fixed pool with reader-funded buffers. ADAPTED tungstenite
0.29 protocol/mod.rs:668-673's additional Pong, keeping it behind committed
prefixes and ahead of new data. Its unbounded close lifetime becomes one absolute
deadline. Go reverseproxy.go:63-91's replace/append distinction and Rust typed IP
comparison supply ingress policy; canonical spelling and mapped-address keys
prevent separate admission keys for one source. Tests use RFC 6455 hand bytes,
the shared law walker and independent Autobahn verdicts.

`read` can return Need when one or more non-final frames completed without a
complete message. The reader records this as activity and continues assembly;
it does not decode or publish an application message. A partial frame alone
never renews idle. Timers use checked absolute timestamp arithmetic; an
unrepresentable deadline raises Overflow rather than silently shortening the
configured duration. The idle_probe fixture uses a 250 ms interval to qualify
fragment activity against real sockets.

`source_for_head(bytes, head, peer, ingress)` applies the same source policy to a parsed head. It borrows the existing head and performs no I/O.
