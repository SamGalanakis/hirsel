# Simulation filesystem disk model

The inode version model of `sim.fs` and the power-cut rule for a pending
durable write. `sim.fs` is the only intended caller; tests read `SECTOR`
and `LIMIT`.

A `Version` holds a file's intended bytes and, while it carries write
damage, the bytes the disk presents. `write` applies one write and its
fault to both. `visible` is what reads see.

`tear` decides what a power cut keeps of one `Pending` write. Its
candidates are the write's `SECTOR`-byte sectors that differ from the bytes
before the write or lie past their end. With no candidate it draws nothing.
Otherwise it draws `pick(3)`: 0 keeps no candidate, 1 keeps every candidate,
and 2 draws one `pick(2)` per candidate in ascending order and keeps a
sector on 1. A kept sector past the old end extends the file, and a hole
before it reads as zeros. The returned `sim.world.Tear` lists the candidate
and kept extents as ascending `(offset, length)` runs.
