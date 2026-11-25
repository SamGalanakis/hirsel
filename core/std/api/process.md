# Process

[API overview](../API.md). [Core provider signatures](../../core/host.hirsel).

`host.stopped(p: Process) waits` returns once the process has received its
first SIGTERM or SIGINT. If the signal arrived before the call, the call
returns with that same stop. The wait observes task cancellation. Pass the
capability explicitly through `main` to handle a stop.

A second SIGTERM or SIGINT terminates the live process immediately. The stop's
delivery passes [the replay point](../../../docs/spec/capabilities.md#cap-8-the-replay-point).

[Capability contract](../../../docs/spec/capabilities.md#cap-5-provider-operations).
