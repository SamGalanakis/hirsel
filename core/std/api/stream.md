# HTTP streams

[HTTP](http.md). [Specification](../../../docs/spec/http.md#2-stream-ownership).

`spec(request, status, bounds, state, producer)` encodes the initial state
and reserves a prefix for the response head in one block. `header` adds a
response field. Refusal to enter the byte bound returns Allocation. A refused
allocation in the body raises Capacity. Return `Stream(spec)` from a route with a `stream_bytes`
allowance. The supervisor admits the group before the first response byte.

The producer is a function that takes a `Seed` and calls
`produce(take seed, next)`. The `next` function mutates its decoded state
and returns one nonempty event, `None` at EOF, or an error. The producer
decodes the state once inside its own task and takes one of two credits
before entering the event's byte and step bounds. The writer releases a
credit after sending the complete event.

HTTP/1.1 uses one chunk per event and a terminating chunk. HTTP/1.0
delimits with a close. HEAD sends just the head. A committed
`text/event-stream` gets heartbeat comments while the writer and queue
are idle, and `Cache-Control: no-store` and `X-Accel-Buffering: no` unless
its fields set them. A producer that fails, or returns an empty or oversized
event, ends the stream without the terminating chunk. The lease begins at the first response byte and covers the whole
stream. The group cancels and joins its children on exit.

`spec_owned` moves a sendable owning callback into the admitted producer.
The callback receives the stream's clock and the caller's optional directory
grant. It can call `produce_owned` with already-owned state. Each item follows
the same credit, byte and step bounds as `produce`, and a disconnected peer
cancels the producer. The callback may capture plain functions selected by
the route configuration.

`spec_relay(request, status, bounds, take fields, take upstream, idle_ms)`
relays an upstream response whose head the handler has read with
[`http_client`](http_client.md): it moves the live `Connection` into the
stream and writes the handler's status and `fields`, each checked as
`header` checks one. Pass `http_client.relay_fields(upstream)` to forward the
upstream's end-to-end fields; [`http_object`](http_object.md) passes the few
it chose. The producer reads at most `event_max`
rounded down to 8 bytes per credit, each read under a deadline `idle_ms`
from its start, and writes each read as one chunk: the client's decoded body
equals the upstream's, with no heartbeat. Only upstream's clean end sends the
terminating chunk; a read error, a cut body, the idle deadline, a client
close, the lease or a stop close without it, and every end drops the
connection, closing upstream. It refuses `InvalidStream` unless the
connection is at a fresh response head and `event_max` is at least 8. On an
upstream failure before its head, answer `http_client.gateway_status(error)`
(502, or 504 for `Deadline`) instead
([HTTP response outcomes](../../../docs/spec/http.md#http-10-outcomes)).
