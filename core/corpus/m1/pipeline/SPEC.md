# Three-task pipeline with bounded backpressure

Spawn exactly three OS workers: source, transform and sink. Two sync channels each hold eight batches. Move owned batches from source to transform, allocate uppercase ASCII output, then move to sink. The sink counts rows/bytes and sums bytes. EOF closes q1 then q2, and the supervisor joins all workers. No shared mutable payload or shared budget exists. A test-only gate holds the transform until q1 is demonstrably full. Pure transformation has no implicit cancellation polling.

## Input and size

8,192 rows of 74 bytes, 32 rows per batch, 256 batches, Q=8 on both queues. Idiomatic batches contain Vec<String>; matched batches contain contiguous bytes and u32 row offsets. The normal case confirms try_send returns Full before releasing the consumer. The small case has two two-row batches.

The committed normal input is 614,403 bytes. `cases.json` gives every input path and variant; no input is generated inside the measured process.

## Oracles and failure variants

All input rows and transformed byte sums are computed directly in Python. Failure variants publish exactly two batches before starting transform; the sink waits until transform joins. error, Capacity at batch2.output and cancel at transform.check destroy batch 2 before the queued batch 1 reaches sink, giving release-order 2,1. cancel-receive publishes only batch 1, then the scripted cancellation wakes the next receive through endpoint closure. cancel-send fills q1 with batches 1..8, observes Full for pending batch 9 and parks on the cancellation delivery gate; the supervisor drains queued owners 1..8, then releases the pending sender owner 9. The cancellation schedule adds six literal cancel rows for queued batches 3..8 and one for pending batch 9. Normal release order between concurrent workers is intentionally unobservable; normal output lists released ids sorted. Failure output includes the exact actual release order.

For every case C, `inputs/C.txt` comes from the construction just described and `EXPECTED/C.stdout`, `C.stderr` and `C.exit` come from the corresponding independent Python function in `core/tools/reference.py`. Stderr is empty; exit is 0, 2, 3 or 4 under `core/README.md`. The generator never invokes Rust or Hirsel. The values were fixed before Rust output comparison. Resource names in stdout identify lexical owners, rather than allocator blocks. Named admission failures are logical source sites, never native allocator call numbers. Cleanup does not perform another portable admission.

## Hand check

Rows a,bb,ccc,d become A,BB,CCC,D. Four rows contain 7 bytes with sum 65 + 2*66 + 3*67 + 68 = 466. At transform failure only A and BB reach sink, giving 3 bytes and sum 197. Batch 2 releases before batch 1. Full is backpressure, never Capacity.

## Spec draft correspondence

See `core/SPEC-COMPARISON.md` for this workload's differences from the spec lane's informative example. Runtime expected values come from this contract and the independent references, never from executing that example.
