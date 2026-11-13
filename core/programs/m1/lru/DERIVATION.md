# lru: hand derivation

Put `a=one`: head and tail are a, and its generation-bearing handle is saved.
Put `b=two`: link b before a. Get a: copy `one`, unlink a from the tail, relink
it before b. Put c: link c before a. Getting absent changes no links. The chain
is c,a,b, one hit has byte sum 111+110+101=322 and no eviction has occurred.
On churn, removing and reusing the saved first slot changes its generation;
probing the saved handle returns Removed, reported as StaleHandle@first-entry.

```text
hits=1 sum=322 evictions=0
order=c,a,b
acquire=cache
release=cache
```

## Logical charges

| site | idiomatic | matched |
|---|---|---|
| put2.payload, `two` | `string.from`: round8(3)=8 | 0 dynamic bytes; the staged inline payload is a local |

The named logical injection is kept in both variants, even though the matched
payload has no admission. The idiomatic Entry has two String headers (48) and
two Option[Id] values (32 each): size 112. A pool slot is 8+max(8+112,16)=128;
64 preallocated slots charge 8192. The matched Slot is 32+4+128+4+4+4+8=184;
64 slots charge 11776 once. Its links are u32, payloads are bounded inline
arrays, and the private saved handle is (index,generation). Keys in the index
remain independently owned. A get always copies the payload before relinking.
Injection before put 2 leaves a as the sole entry and releases the cache once.

Charges use MEM-24, charge-v1, independently of native layout. The corpus injects a
logical refusal before a named source operation; it does not set an ambient quota.
`requested` records the stated charge and `available` is 0 for this synthetic event.
Instrumentation is separate: String trace items count 24 bytes; a channel charges
24×event-capacity+64. Each Marker reserves 24+24+32+256=336, plus its release
string storage; its 256-byte finalizer grant covers one release-line copy.
These are source derivations, not executed Hirsel results.

Id[T] is key whatever T is (4eeee159: a parameter no field mentions adds no
condition), so recency handles compare and Entry declares no bound.

## Source length

Physical lines, including comments and blanks: 174 idiomatic and 195 matched;
Rust entry plus shared program.rs: 148 idiomatic and 175 matched.
Shared support is counted separately in REPORT.md.
