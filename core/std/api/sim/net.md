# Simulation network

Mount a network with `provide.net` and pass the server end to `serve`.
The network uses a bounded pipe in each direction. A seeded plan chooses
the prefix committed by each send and receive. A positive latency delays
transmission on the simulated link using its clock. Run the world clock
driver alongside the server.

`interrupt_at` scripts a partition of both directions from a virtual
instant, and `interrupt_one_way_at` one of only the bytes sent from one
address to another. A stalled established connection waits for its call's
deadline or cancellation. A broken link reports Reset. A send checks the
direction from its own address to the peer's, and a receive or readiness
wait the reverse. A new connection needs both directions, so either one
being interrupted resets it. A listener can accept only queued connections,
and closing a write half lets its peer drain the pipe before EOF.

`resolve_as` and `handshake_as` configure name-specific replies. The server
counts successful opaque secret sends without reading their bytes. Virtual
TLS supplies no trust authority for entry-granted secrets.

`local_as` names addresses as the simulated host's own: the server answers
a public-only `Connect` to one of them `Denied`
([public-only connects](../../../../docs/spec/net.md#net-18-public-only-connects)).

`answer_name` answers one served Resolve or Handshake using the same
scripts as `serve`. It replies Denied to other operations.

## Changing partitions during a run

```hirsel
pub fn cut(n: Net, from: Ip, to: Ip, action: Partition) -> Result[(), NetError] waits
pub fn restore(n: Net, from: Ip, to: Ip) -> Result[(), NetError] waits
pub fn restore_at(tasks: Tasks, clock: Clock, n: Net, a: Ip, b: Ip, at: u64) -> Task[Result[(), NetError]]
```

`serve` holds the scripted `Network` for its life, but these calls change
its partitions through the served Net `n` while it runs. `cut` interrupts
the bytes sent from `from` to `to` from now on, and leaves the reverse
direction as it was. `restore` lifts every interruption of that direction,
scripted or not, and the server retries the calls it stalled. `restore_at`
starts a task that restores both directions between `a` and `b` when
`clock` reaches `at`; join it for the result. `heal` still clears every
partition before the run.
