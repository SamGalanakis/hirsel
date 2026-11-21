# Simulation filesystem

Mount an empty `new()` image with `provide.dir`. Move its served port to the
task that calls `serve`. When that task returns, its mount closes and the
image can be mounted on a fresh port.

`host.sync` makes the current file bytes and length durable. `host.sync_dir`
makes the direct children of that directory durable. `crash` recovers the
durable state and the prefix of later changes selected by the plan. Directory
entries point to inodes, so saving a name and saving the bytes it names are
independent.

A plan applies faults by the zero-based order in which the server takes
operations, excluding release notices. `Eio` refuses the operation.
`SyncUnknown` reports EIO and uses the plan to decide whether the barrier
took effect. Closing releases the file and its lock even when close reports EIO.
