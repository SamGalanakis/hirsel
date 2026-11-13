# versions: hand derivation

Build the seven-node tree with root key 3, children 1 and 5 and leaves
0,2,4,6. The pin and current initially share all seven nodes, values summing
0+1+2+3+4+5+6=21. Updating 0 to 10 copies nodes 0,1,3, leaf to root.
Updating 6 to 20 copies nodes 6,5,3. The intermediate copied root 3 then has
no holder and is released; the copied 0,1 and 6,5 subtrees remain current.
The union contains 7 original nodes plus copies at 0,1,3,5,6 = 12.
Current sum is 21+10+(20-6)=45. Unpin releases the five replaced originals,
leaving seven current nodes. Dropping current leaves zero. Rejecting draft.node3
releases both already-built copies before the old roots are inspected.

```text
updates=2 pinned=21 current=45 live=12
after-unpin=7
after-current=0
```

## Logical charges

| site | idiomatic | matched |
|---|---|---|
| draft.node3 | 64 for the third new counted Node | 0 unless a new slab storage block is needed |

The idiomatic Node is id(8)+key(8)+value(8)+2×Option[Arc](16)=56;
a counted allocation charges round8(56+8)=64. At the refusal, two draft
nodes hold 128 additional logical bytes, all refunded by unwinding the draft.
For 1023 keys the initial nodes hold 65,472. Sharing a pin admits nothing.
Matched Node is key(4)+value(8)+left(12)+right(12)+refs(8)=44;
Slot is 8+max(44,12)=52. Growing its List to capacity c charges
round8(52c), admitting the replacement block before refunding the old one.
Child indices are private, checked list indices; nodes are immutable after
publication except their refcounts. The free list is inline in vacant slots.
Every typed staging refusal releases new child edges before returning; no root
is published before success. Task failure cleans the entire owning slab.

All charges use MEM-24, charge-v1. Empty owners and static literals charge 0.
A growth block has capacity max(needed,4,2×old), and is admitted before the old
block is refunded. Named injections are synthetic source sites, not host-OOM
or allocator-call counters. Output and instrumentation charges are separate
from the payload charges above. These are hand derivations, not executed Hirsel
results.

## Source length

Physical lines, including comments and blanks: 138 idiomatic, 188 matched; Rust entry + shared program: 163 idiomatic and 185 matched. Shared support is counted separately in the lane report.
