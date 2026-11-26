# Simulated S3 nodes

[Simulated S3](s3.md). [Simulated nodes](node.md).

Several simulated nodes can share one bucket. Each node dials the service
at its own address on the service's `Net`, and the service tells nodes apart
by that address.

```hirsel
pub fn join(n: Net, address: Addr) -> Result[(), NetError] waits
pub fn cut(n: Net, address: Addr) -> Result[(), NetError] waits
pub fn restore(n: Net, address: Addr) -> Result[(), NetError] waits
```

`join` makes `address` accepted by the service on `n` as well as its
configured address. A node's connections draw faults the same way as
connections to the configured address.

`cut` cuts the node that dials `address`, so other nodes keep reaching the
bucket. Its new connections are reset, and its open connections are reset
as if each were dropped by a `Drop` hold. Each request a hold has parked for
it is discarded without being applied, even if its release condition is met
later. The configured address can be cut as well. `restore` lets new
connections through `address` in again. Connections the cut reset stay
reset.

Each call returns once the service has applied the change. An address that
is not an IPv4 or IPv6 socket address returns `Error(Denied)`.

`nodes`, `joined`, `severed`, `admit` and `command` are the hooks
`sim.s3.serve` uses to keep this table; tests call the three functions above.
