# Simulated nodes

[Simulated S3 nodes](s3_node.md). [Simulated UDP](udp.md). [Simulation network](net.md).

A `Node` records the provider links one simulated node uses and the tasks
it runs, so that a test can crash the node while its peers keep running.

```hirsel
pub opaque type Node
pub fn node() -> Node
pub fn reaches_s3(mut machine: Node, n: Net, address: Addr)
pub fn sends_udp(mut machine: Node, n: Net, own: Ip, peer: Ip)
pub fn connects_tcp(mut machine: Node, n: Net, own: Ip, peer: Ip)
pub fn runs(mut machine: Node, take task: Task[()])
pub fn crash(take machine: Node) -> Result[(), NetError] waits
```

`reaches_s3` records that the node reaches the `sim.s3` service on `n`
through `address`, which should already have been joined with
[`sim.s3_node.join`](s3_node.md). `sends_udp` records a `sim.udp` link
between the node's `own` IP and `peer`, and `connects_tcp` a `sim.net`
link. `runs` hands the node a task.

`crash` first cuts every recorded link in both directions, in the order the
links were added: the bucket address with `sim.s3_node.cut`, a UDP link with
`sim.udp.link(.., Cut)` and a TCP link with `sim.net.cut(.., Break)`. A
request still in flight on the node is therefore never applied, and nothing
its tasks do while stopping reaches a peer. It then cancels every task and
joins them all, which drops the node's volatile state. Peer nodes and the
services keep running. `crash` returns the first error a provider gave for a
cut; the tasks are stopped either way.
