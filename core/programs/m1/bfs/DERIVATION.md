# bfs: hand derivation

Validation reads all seven edges before search storage. Enqueue 0 and mark its
parent as 0. Processing 0 enqueues 1 then 2; processing 1 enqueues 3; processing 2
finds 3 already marked and enqueues 4; processing 3 enqueues 5. Processing 4 sees
5 marked. Processing 5 adds nothing. Queue is `[0,1,2,3,4,5]`. Following parents
from 5 gives `[5,3,1,0]`; reverse gives `[0,1,3,5]`. The whole reachable component
has been exhausted before the owned path is produced.

```text
path=0,1,3,5
visited=6
acquire=graph
acquire=parent
acquire=queue
release=queue
release=parent
release=graph
```

## Logical charges

| site, six-vertex fixture | charge in either source |
|---|---|
| search.parent | exactly 6 × u64 = 48 |
| search.queue | exactly 6 × u64 = 48; parent still holds 48 |
| result.path | exactly 4 × u64 = 32; parent + queue still hold 96 |

Normal fixtures use V=16,384, so each search buffer charges 131,072. At a named
refusal nothing is admitted. The parent refusal has only the graph owner; the
queue refusal cleans parent then graph; the path refusal cleans queue, parent,
graph. Path reconstruction counts its length first and reserves exactly once.
Idiomatic graph headers charge V×24 plus each adjacency list's GROWTH block.
Matched adjacency reserves (V+1)×4 rounded to 8 for offsets, V×4 for insertion
positions and E×4 for neighbors; input order is preserved by the positions pass.

Charges use MEM-24, charge-v1, independently of native layout. The corpus injects a
logical refusal before a named source operation; it does not set an ambient quota.
`requested` records the stated charge and `available` is 0 for this synthetic event.
Instrumentation is separate: String trace items count 24 bytes; a channel charges
24×event-capacity+64. Each Marker reserves 24+8+32+256=320, plus its release
string storage; its 256-byte finalizer grant covers one release-line copy.
These are source derivations, not executed Hirsel results.
