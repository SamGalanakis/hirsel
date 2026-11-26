# Simulated UDP

Mount a Net with `provide.net` and give its server end to `serve`, or use
`start` below. Give the server exclusive control of the World for its
Clock. It advances virtual time to the earliest timer when the run is
idle. Other providers can share that Clock.

`defaults` sends immediately. Set the Gilbert–Elliott transition and loss
probabilities in parts per million. Each direction starts in the good
state and draws loss before transitioning. The server adds delay, jitter
and optional extra reordering delay before delivery. Jitter is the
integer average of the previous jitter and a new uniform draw from
zero through `jitter_ns`. A duplicate has the same arrival time, payload
and observed source as its original packet. Equal-time arrivals are
ordered by insertion. A plan fixes the network's choices for the same
sequence of sends and virtual times.

`rebind` is the probability of assigning a new external port for an
opened socket on each committed datagram. The old mapping stops accepting
traffic. `udp_local_addr` still reports the internal endpoint.

`captures` scripts records from external traffic at absolute virtual
times. A GRO record drops segments beyond the first incomplete one.
`UnknownBoundaries` and a zero GRO stride discard the whole record.
Full member queues and retired members discard new arrivals. The caller
runtime applies its ingress grant before returning received packets.
`steer_member` implements the host's Hash and QUIC member selection.

## Driven clocks

```hirsel
pub fn start(tasks: Tasks, config: Config, take plan: sim.world.Plan, take world: sim.world.World, clock: Clock, take captures: List[Record], queue_bytes: u64) -> (Net, Task[()])
```

`serve` advances its World itself, so it refuses a World that
[`sim.world.drive`](world.md#several-clocks) owns. `start` mounts its own Net
and serves it in a new task. It keeps a timer on `clock` for the next arrival,
so it works whether or not the World is driven. On an undriven World it
advances the clock as `serve` does. The task ends when the run is idle and
no clock has a timer.

## Directed links

```hirsel
pub type Impairment: send { loss: u64, delay_ns: u64, jitter_ns: u64, reorder: u64, reorder_ns: u64, duplicate: u64 }
pub type Link: send = Cut | Impaired(Impairment)
pub fn link(n: Net, from: Ip, to: Ip, l: Link) -> Result[(), NetError] waits
pub fn restore(n: Net, from: Ip, to: Ip) -> Result[(), NetError] waits
```

`link` sets the directed link from `from` to `to` on the provider serving
`n`, at any time during the run. The sender is its socket's bound IP, or the
configured source when the socket is bound to an unspecified address. A cut
drops every later datagram on that link. An impaired link replaces the
configured delay, jitter, reordering and duplication with its own: it loses
each datagram with probability `loss`, independently, and adds a uniform
jitter from zero through `jitter_ns`. Probabilities are in parts per million;
a value outside that range returns `Error(Denied)`. `restore` returns the link
to the configuration. A change affects datagrams sent after it; datagrams
already in flight still arrive. Each direction is set separately, so a
one-way cut is one call.
