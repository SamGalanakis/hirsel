# rope: hand derivation

Start with chunk 0=`abcde`, root [0:0..5]. Replace byte range 2..3 by
chunk 1=`XY`: [0:0..2,1:0..2,0:3..5] renders abXYde. Retain the prior root
in history. Replace range 1..3 by chunk 2=`z`: [0:0..1,2:0..1,1:1..2,0:3..5]
renders azYde and retains the abXYde root. UNDO releases the displaced root,
including the last holder of z, and restores abXYde. Its bytes sum to
97+98+88+89+100+101=573. History now holds the initial root. Releasing history
leaves chunks 0 and 1; releasing current leaves none. A failed second edit
leaves the same current text and one history entry, but only one completed edit.

```text
edits=2 undo=1 bytes=6 sum=573 live=2
after-history=2
after-text=0
```

## Logical charges

| site | idiomatic | matched |
|---|---|---|
| edit2.piece, after first edit | (3+2)×Piece(24)=120 | round8((3+2)×Piece(12))=64 |

The edit reserves an upper bound of old-piece-count+2 before installing a root.
The inserted idiomatic chunk copies z with charge 8 and creates a counted
Chunk of id(8)+String-header(24)+count(8)=40. Matched chunks have one String
and a private refcount; strings admit round8(length), and slab slots grow as a
List. A matched Slot is 8+max(String-header(24)+refs(8),Option[u32](12))=40.
An Rc holder and a retained slab edge admit nothing. Moving the previous own
piece list into undo avoids a descriptor copy; it leaves all externally
observable sharing and release boundaries unchanged. Matched staging admits
all fallible blocks before adding retained edges; private indices never escape
the Store and a free-list slot is reused only when its edge count reaches zero.
Both boundaries are checked before staging, including the interior of lambda.

All charges use MEM-24, charge-v1. Empty owners and static literals charge 0.
A growth block has capacity max(needed,4,2×old), and is admitted before the old
block is refunded. Named injections are synthetic source sites, not host-OOM
or allocator-call counters. Output and instrumentation charges are separate
from the payload charges above. These are hand derivations, not executed Hirsel
results.

## Source length

Physical lines, including comments and blanks: 157 idiomatic and 214 matched;
Rust entry plus shared program.rs: 144 idiomatic and 164 matched.
Shared support is counted separately in REPORT.md.
