# generics: hand derivation

The input lines are `a`, `alpha`, `a`, `λ`. The scoped callback increments its
position before each call, makes an independent owned copy, and appends `!`.
Lengths are 2, 6, 2, 3 UTF-8 bytes. Their sum is 13 and only `alpha!` reaches 6.
Insertion encounters `a!`, `alpha!`, repeats `a!`, then encounters `λ!`, so the
ordered counts are 2, 1, 1. The callback completes before the map is acquired.
The map, mapped list and original list are then cleaned in that order.

```text
long=1 bytes=13
a!=2
alpha!=1
λ!=1
acquire=list
acquire=mapped
acquire=map
release=map
release=mapped
release=list
```

## Logical charges

| site | idiomatic charge | matched charge |
|---|---|---|
| callback2.string, capacity input `b` | `string.from`: round8(1)=8; append `!` grows capacity to 4, admitting 8 before refunding 8 | same |
| tally2.key, capacity input `b!` | key copy round8(2)=8; the existing 4-entry map needs no growth | append 2 bytes to the key arena; growth is max(needed,4,2c), rounded to 8 |

For the small input, `alpha` copies 5 bytes with charge 8; appending `!` grows
capacity from 5 to 10, admitting 16 while the old 8 remains. The generic output
list reserves exactly 4 × String-header(24)=96. The ordered built-in map starts
with 4 × (24+8+16)=192. The matched tally uses 16 × u64=128 for its initial buckets,
4 × Count(16)=64 for its entries and one growing UTF-8 arena. Hashing alone wraps.
The named refusal happens before either the copy or the append. Earlier callback
results belong to `ys` and are cleaned even though no map exists yet.

Charges use MEM-24, charge-v1, independently of native layout. The corpus injects a
logical refusal before a named source operation; it does not set an ambient quota.
`requested` records the stated charge and `available` is 0 for this synthetic event.
Instrumentation is separate: String trace items count 24 bytes; a channel charges
24×event-capacity+64. Each Marker reserves 24+8+32+256=320, plus its release
string storage; its 256-byte finalizer grant covers one release-line copy.
These are source derivations, not executed Hirsel results.

## Source length

Physical lines, including comments and blanks: 78 idiomatic, 133 matched; Rust entry + shared program: 72 idiomatic and 109 matched. Shared support is counted separately in the lane report.
