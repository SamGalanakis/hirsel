# Graceful TCP completion

`finish(take write_half, take read_half, clock, deadline_ns)` composes the
existing half releases and recv. It first releases the WriteHalf, which sends
SHUT_WR without closing the descriptor while the ReadHalf is alive. It then
drains raw TCP bytes with an 8192-byte scratch buffer to EOF, Reset or the
unchanged absolute deadline, and finally releases the ReadHalf.

The result reports Allocation(Capacity) or Network(NetError). Deadline is
Network(TimedOut), including a continuously ready peer that sends forever.
The clock is checked between successful reads, so try-first core operations
cannot extend the composite deadline. Cancelled task cleanup still releases
the remaining half. Finishing sends no application protocol frame and does
not acknowledge application delivery. The protocol writer must flush its Close
before calling finish.

This is NET-7's specified ownership composition, with no provider ABI change.
It follows TCP half-close/drain practice in Go and Tokio; ordinary drop alone
cannot prevent unread input from making final close reset the connection.
Goose's hypothetical socket sketch has no graceful completion operation.
