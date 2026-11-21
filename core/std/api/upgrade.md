# HTTP upgrade

[HTTP](http.md). [Specification](../../../docs/spec/http.md#http-10-outcomes).

`spec(request, protocol, bytes, argument, run)` checks the client's
protocol offer and encodes the argument under `state_max`. The route's
`upgrade_bytes` limits the new task's reservation. To add protocol
headers to the 101, put field pairs in the spec's `fields`. Refusal to enter
the byte bound returns Allocation. A refused allocation in the body raises
Capacity.

Return `Upgrade(spec)` from the raw handler. The supervisor admits the
protocol task before the 101 is written. A refusal returns both halves
to HTTP for a 503. The accepted protocol task owns the socket.

The protocol's first call is `start(take handover)`. It decodes the argument
inside the new task's reservation, writes the full 101 and returns the
protocol socket and decoded argument. Malformed argument bytes return Decode.
A refused allocation raises Capacity before the 101 is written. `read`
consumes already buffered
tail bytes first. `write` reports the number of bytes accepted by the socket.
Call `established(mut socket)` when protocol setup completes to release the
first-phase slot. The admitted socket slot stays in use until its task ends.
When the server stops, `stop_deadline(socket)` exposes the original deadline.
Socket reads and writes issued after stop use the earlier deadline.
