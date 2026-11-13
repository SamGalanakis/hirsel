# request-scratch: hand derivation

Request 1 selects /alpha, reads X-Id=1 and Content=abc and renders
`welcome:1:abc`: 13 bytes, byte sum 1207. Its scratch and socket end before
request 2 begins. Request 2 selects /beta and renders `ready:2:xy`: 10 bytes,
sum 940. Commit the scripted write only after the reply is fully prepared.
Totals are 2 requests, 23 bytes, sum 2147. A malformed second header or either
second-request refusal retains only request 1's delivered prefix. The config
is immutable Arc data; each request has an explicit shared holder.

```text
requests=2 bytes=23 sum=2147
acquire=socket#1
acquire=scratch#1
release=scratch#1
release=socket#1
acquire=socket#2
acquire=scratch#2
release=scratch#2
release=socket#2
```

## Logical charges

| site | idiomatic | matched |
|---|---|---|
| request2.header2 | 0; headers were reserved once before walking them | 0 for the reserved descriptor; growing the byte arena can separately admit GROWTH |
| request2.reply | round8(10)=16 | same |

Idiomatic Header is four u64 offsets, size 32. Two headers reserve
round8(8+2×32)=72 in the request region. Matched Header is four u32 offsets,
size 16; the same reservation is round8(8+2×16)=40, plus one growing String
arena containing concatenated names and values. `span.push` within the reserved
capacity admits nothing. The reply reserves exactly its full byte length,
so subsequent appends admit nothing. The injection named header2 is a logical
source failure, not a claim that the second span push allocates.
The route map starts at 4×(24+24+16)=256, and Arc[Config] adds
round8(Map-header(24)+8)=32, excluding key/value blocks. Shared holders admit 0.

All charges use MEM-24, charge-v1. Empty owners and static literals charge 0.
A growth block has capacity max(needed,4,2×old), and is admitted before the old
block is refunded. Named injections are synthetic source sites, not host-OOM
or allocator-call counters. Output and instrumentation charges are separate
from the payload charges above. These are hand derivations, not executed Hirsel
results.

## Source length

Physical lines, including comments and blanks: 134 idiomatic, 140 matched; Rust entry + shared program: 72 idiomatic and 80 matched. Shared support is counted separately in the lane report.
