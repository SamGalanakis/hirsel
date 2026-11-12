# Text pieces with middle edits and bounded undo

Store text as ordered slices of immutable counted chunks. An edit uses UTF-8 byte offsets, validates both boundaries, stages the new inserted chunk and piece list, then installs it while retaining the prior piece list as undo. Retain at most 64 undo snapshots; evict the oldest explicitly. UNDO replaces current with the most recent snapshot and releases the displaced root. Rendering visits pieces in sequence. This is a piece table with shared chunks, not a claim of logarithmic balanced-rope edits.

## Input and size

65,536 initial ASCII bytes, 256 middle replacements of three bytes with eight-byte text, then 16 undos. Idiomatic pieces hold Rc<Chunk> and usize ranges. Matched pieces hold u32 chunk ids and ranges in a reusing counted slab. Each snapshot copies descriptors and shares chunk bytes; it never copies the whole text.

The committed normal input is 69,851 bytes. `cases.json` gives every input path and variant; no input is generated inside the measured process.

## Oracles and failure variants

Python uses flat character sequences with origin labels and retained snapshots. The number of distinct live origins supplies a liveness oracle independent of piece splitting and counting. error exceeds the text range; utf8 splits lambda between its UTF-8 bytes; empty-undo has no snapshot. Capacity rejects edit2.piece before changing current or history. Failed edits retain the prior document. History releases before current at program end.

For every case C, `inputs/C.txt` comes from the construction just described and `EXPECTED/C.stdout`, `C.stderr` and `C.exit` come from the corresponding independent Python function in `core/tools/reference.py`. Stderr is empty; exit is 0, 2, 3 or 4 under `core/README.md`. The generator never invokes Rust or Hirsel. The values were fixed before Rust output comparison. Resource names in stdout identify lexical owners, rather than allocator blocks. Named admission failures are logical source sites, never native allocator call numbers. Cleanup does not perform another portable admission.

## Hand check

abcde with byte 2 replaced by XY becomes abXYde. Replacing bytes 1..3 with z gives azYde. UNDO restores abXYde. Its byte sum is 97+98+88+89+100+101 = 573. Only the initial chunk and XY survive; the z chunk releases on undo. Both remaining chunks release after current is dropped.

## Spec draft correspondence

See `core/SPEC-COMPARISON.md` for this workload's differences from the spec lane's informative example. Runtime expected values come from this contract and the independent references, never from executing that example.
