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

`Corrupt`, `Misdirect` and `LostWrite` acknowledge a nonempty write.
`Corrupt` flips one bit in the written range. `Misdirect` leaves the
intended range alone and writes the original bytes to an adjacent range in
the same file. `LostWrite` leaves the old bytes and length in place.
Reads and stats see the damaged version. The image keeps the intended
bytes separately, and sync and crash retain the damage with its version.
Later successful writes update both versions.

`has_write_damage` reports whether a current inode version has an overlay.

`serve_tapped` serves like `serve` and also sends a `sim.world.Receipt` on
its tap for each fault it delivers and each damaged read it answers, before
the reply. A receipt names the serve (`mount`, counted per image), the fault
site, the operation, the inode and its path, the requested range and, for a
write, where its bytes landed, plus the image's operation count and crash
count for ordering. `Refused` is an Eio refusal, `Barrier(advanced)` a failed
sync, `Acknowledged` a damaging write, and `DamagedRead` a read or stat whose
answer differs from the intended bytes, reported once per inode version per
serve. When a reply that would show a fault is not delivered, an
`Undelivered` receipt for the same site follows. Receipts are for test
oracles; the server never reads them back. `receipts_dropped` counts
receipts the server could not send because the tap was full or closed.
