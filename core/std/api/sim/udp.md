# Simulated UDP

Mount a Net with `provide.net` and give its server end to `serve`. Give
the server exclusive control of the World for its Clock. It advances
virtual time to the earliest timer when the run is idle. Other providers
can share that Clock.

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
