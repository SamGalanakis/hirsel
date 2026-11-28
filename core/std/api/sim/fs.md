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

A write on a `Durable` or `Direct` handle has two events (docs/spec/core.md,
Host files). Its bytes are readable once the server takes it. They are
durable once its success is delivered, and a crash then keeps the whole
write. When the write is refused with `Eio`, or its success is never
delivered, the bytes stay readable and the image records the write as
pending. A `sync` of the file or a later successful write on a durable
handle of it makes a pending write durable. `crash` tears a pending write
instead of cutting it whole: after its cut pick it calls `sim.fs_disk.tear`
for each pending write, in inode order and then issue order, which keeps a
drawn subset of the write's 512-byte sectors over the bytes the cut kept.
An image holds at most `sim.fs_disk.LIMIT` pending writes and fails on one
more. With no pending write a crash draws exactly what it drew before
pending writes existed.

`torn_writes` returns one `Torn` receipt for each write the last `crash`
tore. Each carries the serve and site of the write, its range, and a
`sim.world.Tear` naming the candidate extents and the extents kept.

`serve_tapped` serves like `serve` and also sends a `sim.world.Receipt` on
its tap for each fault it delivers and each damaged read it answers, before
the reply. A receipt names the serve (`mount`, counted per image), the fault
site, the operation, the inode and its path, the requested range and, for a
write, where its bytes landed, plus the image's operation count and crash
count for ordering. `Refused` is an Eio refusal, `Barrier(advanced)` a failed
sync, `Acknowledged` a damaging write, and `DamagedRead` a read or stat whose
answer differs from the intended bytes, reported once per inode version per
serve. When a reply that would show a fault is not delivered, an
`Undelivered` receipt for the same site follows. `Persisted` follows an
`Acknowledged` write on a `Durable` or `Direct` handle, and a sync that
advances a file holding write damage: that reply made the damage durable.
A `Refused` receipt for a write on a durable handle carries the range the
refused write left readable. Receipts are for test
oracles; the server never reads them back. `receipts_dropped` counts
receipts the server could not send because the tap was full or closed.
