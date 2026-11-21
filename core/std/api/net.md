# Graceful TCP completion

`finish(take write_half, take read_half, clock, deadline_ns)` composes the
existing half releases and recv. It first releases the WriteHalf, which sends
SHUT_WR without closing the descriptor while the ReadHalf is alive. It then
drains raw TCP bytes with an 8192-byte scratch buffer to EOF, Reset or the
unchanged absolute deadline, and finally releases the ReadHalf.

`FinishError` reports `Network(NetError)`. If the deadline expires, the
result is `Network(TimedOut)`, even if the peer keeps sending. A refused
scratch allocation raises `Capacity` and cleanup releases the read half.

The clock is checked between successful reads, so try-first core operations
cannot extend the composite deadline. Cancelled task cleanup still releases
the remaining half. Finishing sends no application protocol frame and does
not acknowledge application delivery. The protocol writer must flush its Close
before calling finish.

This is NET-7's specified ownership composition, with no provider ABI change.
It follows TCP half-close/drain practice in Go and Tokio; ordinary drop alone
cannot prevent unread input from making final close reset the connection.
Goose's hypothetical socket sketch has no graceful completion operation.

## Name lookup

`std.net.lookup(n: Net, name: String, until: Until) -> Result[List[Ip], LookupError] waits`
reserves room for 16 addresses under the current limit, calls `host.resolve`
([NET-11](../../../docs/spec/net.md#net-11-name-resolution)) and returns the addresses in
resolver order. `LookupError = Network(NetError) | NoAddress`.
`NoAddress` is a name with no address, which `host.resolve` reports as `Ok(0)`, so callers
of `lookup` do not mistake an empty list for success. That is how Go's `LookupIP` reports
"no such host". Call `host.resolve` directly to choose the bound or to reuse a list.

- **Authority.** A `Resolve` rule of `n` must cover `name`. Otherwise the result is
  `Network(Denied)` and no query is sent. Connecting to a result still needs a `Connect`
  rule.
- **Charging.** The list admits room for 16 `Ip` values once, and refunds it on
  failure. The host operation admits nothing.
- **Failures.** `Network(Aborted)` on cancellation while waiting, `Network(TimedOut)` at
  `until`, `Network(Unreachable)` for a temporary resolver failure, and
  `Network(Other(code))`. A refused list allocation raises `Capacity`.
- **Replay.** One `RESOLVE` issue and delivery. A replay returns the recorded addresses
  without a query.
