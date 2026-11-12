# Two files and an OS file lock with prefix failures

Create a private temporary directory, open source, create destination, acquire a real exclusive OS file lock on a third file, and copy in 4,096-byte buffers. Automatic success and error cleanup releases lock, destination, source. Probe the lock using a fresh independently opened file description after cleanup. Report destination bytes and checksum, so partial external effects remain visible. File and directory setup are included in whole-process timing. The mutex-poisoning subcase is excluded from M1.

## Input and size

12,000 lines of 24 bytes, 288,000 source bytes. Both Rust twins use the same three owned OS handles and byte buffer, since there is no meaningful alternative representation for them. File::try_lock encounters a real held lock in error-lock. No scripted-only successful lock can satisfy this workload.

The committed normal input is 288,000 bytes. `cases.json` gives every input path and variant; no input is generated inside the measured process.

## Oracles and failure variants

Python derives file contents and effect prefixes directly. error-first opens a nonexistent source; error-second uses create_new against an existing destination; error-lock holds a conflicting real lock; error-work writes exactly 17 bytes and then reports Io@copy. Capacity at copy.buffer occurs after the three acquisitions and before any copy. error-release-* injects a fallible cleanup report after performing the real release. error-cleanup-all reports Io@copy as primary and lock,destination,source cleanup errors as suppressed in that order. Every actual OS release still runs. Probe success proves the lock is available after all variants; the destination prefix is not rolled back.

For every case C, `inputs/C.txt` comes from the construction just described and `EXPECTED/C.stdout`, `C.stderr` and `C.exit` come from the corresponding independent Python function in `core/tools/reference.py`. Stderr is empty; exit is 0, 2, 3 or 4 under `core/README.md`. The generator never invokes Rust or Hirsel. The values were fixed before Rust output comparison. Resource names in stdout identify lexical owners, rather than allocator blocks. Named admission failures are logical source sites, never native allocator call numbers. Cleanup does not perform another portable admission.

## Hand check

hello world plus newline is 12 bytes. hello sums to 532; space is 32; world sums to 552; newline is 10. Total is 1126. A failed second acquisition closes only source. A failed lock closes destination then source. A work error releases lock, destination, source and leaves the 17-byte prefix.

## Spec draft correspondence

See `core/SPEC-COMPARISON.md` for this workload's differences from the spec lane's informative example. Runtime expected values come from this contract and the independent references, never from executing that example.
