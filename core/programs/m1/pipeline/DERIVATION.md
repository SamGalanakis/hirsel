# pipeline: hand derivation

The source publishes batch 1=[a,bb], batch 2=[ccc,d]. The transform
copies and uppercases them into [A,BB] and [CCC,D]. Sink totals are two
batches, four rows, seven bytes, and 65+132+201+68=466. Source reports two
sends; transform reports two actual forwarded batches. Before normal startup
with more than eight batches, q1 holds 1..8 and try_send of 9 returns Full.
On batch-2 error or cancellation, its owner releases before the sink starts,
then the sink releases batch 1. The surviving output is A,BB, 3 bytes, sum 197.
Cancel-receive publishes only 1. Cancel-send fills 1..8, holds rejected 9,
lets the transform drain 1..8, then delivers cancellation to the source's next
send; 9 releases last. Normal release ids are sorted; failure order stays real.

```text
sent=2 transformed=2 sink-batches=2 rows=4 bytes=7 sum=466
backpressure=false
released=1,2
```

## Logical charges

| site, second small batch [ccc,d] | idiomatic | matched |
|---|---|---|
| batch2.output | rows block 2×String-header(24)=48 plus copies round8(3)+round8(1)=16, total 64 | one packed 4-byte copy rounded to 8 |

Batch's inline resource handle is 8. Each eight-slot data channel therefore
charges 8×8+64=128, separately from each resource's admitted fields and cell.
An idiomatic Batch reserves id(8)+List-header(24)+Sender(8)+cause(32)=72;
matched Batch adds a String header and offsets-list header, totaling 96.
Release ids use a cap-B u64 channel, charge 8B+64. Unit gates charge 64 each;
the extra nonblocking gates deliberately prevent cancellation being observed
while synchronising the fixture. Three tasks each reserve 4,194,304 plus their
256-byte record, 32-byte cause cell and Result/Stats inline result size.
The main run uses a 64 MiB root limit. The progress channel counts actual
successful forwarding, including when join returns Cancelled. Full is a
SendError carrying the pending owner; it is never mapped to Capacity.
FOR SPEC: the cancel-send fixture's specified delivery gate observes cancellation
at send issue after drain, not proof of a previously blocked OS send. It preserves
the written release schedule, matching the Rust fixture's pending-owner gate.

All charges use MEM-24, charge-v1. Empty owners and static literals charge 0.
A growth block has capacity max(needed,4,2×old), and is admitted before the old
block is refunded. Named injections are synthetic source sites, not host-OOM
or allocator-call counters. Output and instrumentation charges are separate
from the payload charges above. These are hand derivations, not executed Hirsel
results.

## Source length

Physical lines, including comments and blanks: 375 idiomatic, 373 matched; Rust entry + shared program: 226 idiomatic and 234 matched. Shared support is counted separately in the lane report.
