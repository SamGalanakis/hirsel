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
are idle. The lease begins at the first response byte and covers the whole
stream. The group cancels and joins its children on exit.
