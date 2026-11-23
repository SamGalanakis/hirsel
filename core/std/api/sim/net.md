# Simulation network

Mount a network with `provide.net` and pass the server end to `serve`.
The network uses a bounded pipe in each direction. A seeded plan chooses
the prefix committed by each send and receive. A positive latency delays
transmission on the simulated link using its clock. Run the world clock
driver alongside the server.

`interrupt_at` scripts a partition from a virtual instant. A stalled
established connection waits for its call's deadline or cancellation.
A broken link reports Reset. A listener can accept only queued connections,
and closing a write half lets its peer drain the pipe before EOF.

`resolve_as` and `handshake_as` configure name-specific replies. The server
counts successful opaque secret sends without reading their bytes. Virtual
TLS supplies no trust authority for entry-granted secrets.

`answer_name` answers one served Resolve or Handshake using the same
scripts as `serve`. It replies Denied to other operations.
