# Ordered persistent map

[API overview](../API.md).

`std.omap.OMap[K: ordered + shareable + send, V: shareable + send]` is a
persistent B+ tree with maximum fan-out 32 and minimum non-root occupancy 16.
`new` allocates nothing, `snapshot` shares the root in O(1) without allocation,
and `len` reads its entry count. `get`, `first` and `last` borrow one version.
An update returns a new version while every held version retains its contents.
Each leaf holds a contiguous list of Arc[Entry[K,V]] rows. Path copying
shares each unchanged row without copying its key or value. Unique node
holders keep in-place editing, including the left half of a split. Branches
share an Arc of separator holders while boundaries stay unchanged. New
separators retain their minimum leaf row; decoded separators retain the
supplied key in an Arc, preserving the decoder's exact input. Rebuilding
separators shares these holders without copying payloads. Branch search uses
an upper bound and leaf search uses a lower bound, with a right-edge probe.
Splits and merges transfer or share row holders. Resources fail the ordinary `shareable` bound.
Versions are sendable; last-holder release frees unreachable nodes and payloads
through the existing constant-stack Arc worklist.

`insert(mut ids,m,k,v)` and `remove(mut ids,m,k)` path-copy, split, merge or
redistribute O(log n) nodes, at most 32 holders per node. `ids` is the writer's
last issued u64, shared across all branches. Every new node receives the next
id after its children; an exhausted counter raises checked Overflow. Capacity
preserves m; insertion returns its taken key/value. Failed attempts may consume
ids, which must never be reused. These APIs continue to borrow m. Successful inserts move their inputs into
one shared row. On Capacity, temporary path holders are dropped before the
row is unwrapped to recover the original inputs, without copying them.

`insert_owned(mut ids,take m,take k,take v)` is a separate consuming update.
At a unique Arc, `arc.with_unique` lends the payload for in-place edits.
An overwrite whose entire path and row are unique moves the raw inputs only
after acquiring those borrows, then completes without allocation. Other
updates first move the inputs into one row shared by the draft and recovery
holder.
The node stays allocated, including the left half of a split; a split allocates
only the new sibling. Shared nodes use path copying. Branches retain their shared
separator block while boundaries stay unchanged and rebuild it when they change.
Capacity consumes the draft m and returns k/v; keep an independent snapshot
for rollback. Every held version and its node ids stay unchanged.

`insert_recover(mut ids,take m,take k,take v)` returns
`Result[(OMap[K,V],Option[Replaced[K,V]]),(Capacity,(OMap[K,V],(K,V)))]`. On refusal it returns the
original map with unchanged contents and node ids, plus its key and value.
The original root stays alive until publication, so the implementation uses
path copying even when the caller owns the only version. No fallible rollback
allocation is needed. This is the transaction-friendly recovery contract;
`insert_owned` remains the explicit discard-on-Capacity alternative.

All insert forms return `Ok((map, Option[Replaced[K,V]]))`. New keys return
`None`. Replacements return the value that was stored before the update, from
the insertion descent without a second lookup or payload clone. Read it with
`replaced_value(old) -> View[V] in old`. The opaque, sendable result moves the old value out of a unique row or
retains just the old shared row through its Arc, independently of its leaf.
It allocates no extra box. Snapshots keep their previous values. Error results have no replaced
entry. A caller uninterested in the entry binds and drops the second tuple field.

| form | map input | success | Capacity | reason today |
|---|---|---|---|---|
| insert | borrowed | new map, optional old value holder | key/value; input remains valid | persistent update while retaining a version |
| insert_owned | taken | new map, optional old value holder | key/value; draft consumed | in-place edits through arc.with_unique |
| insert_recover | taken | new map, optional old value holder | original map and key/value | strong recovery without fallible rollback |

The consuming forms retain different recovery contracts. `insert_owned` edits unique
payloads in place and may discard its draft on Capacity. `insert_recover` retains the
original root; taking the unique path there requires a failure-atomic update algorithm.
Borrowed `insert` continues to express a persistent update of a retained version.

`range(m, lower, take upper)` accepts `Unbounded`, `Included(k)` or
`Excluded(k)` endpoints and returns an owned, sendable `Cursor`. It reserves one
Arc frame per level, seeks recursively in O(log n), and pins the version from
which it was made. `range_reverse(m, take lower, upper)` seeks from the upper
bound and walks the same interval in descending order. `next(mut cursor)`
returns `Option[(K, V)]`. It copies just that row. A raised Capacity leaves
the row unconsumed. Traversing k rows costs O(k + log n) node work and the cost
of copying their payloads. Inverted bounds yield an empty cursor. A cursor
can outlive its originating map and move to another task.

`count(m, lower, upper)` uses subtree sizes to return the number of keys in
the requested interval. It visits O(log n) nodes and allocates nothing.

Checkpoint writers use `root` for an Arc holder and `node_id`, `node_level`,
`node_count`, `node_len`, `node_key`, `node_value`, `node_child` for individual
nodes. Node level is zero at leaves. Leaf count is the number of key/value
pairs; branch count is the number of children and their minimum keys. Keys,
values and children outside their count return None. Branches have no values;
leaves have no children. A node has stamp, depth, size, a row-holder list, a child-holder list and an
optional shared separator list. Leaves have rows and no children or separators;
branches have children and separators and no rows. The logical node charge is
96 bytes, plus list backing capacities. Each row pays one counted allocation
for K and V and their owned payloads; shared holders never charge those payloads
again. A full u64/u64 leaf charges 1120 bytes: 96 for the node, 256 for its
32 holders and 32 times 24 for its rows. Sharing reduces path-copy work and
payload duplication at the cost of one allocation per stored row. Consuming
updates reserve spare leaf slots explicitly; fan-out by count stays 32.
Checkpoint views expose keys and values independently of their holders.

A checkpoint cutoff can skip a whole subtree whose id is already persisted.
`leaf(id,take keys,take values)` and `branch(id,take low_keys,take children)`
construct decoded nodes with recorded ids, without rechecking tree invariants.
The decoder must provide the documented shape. On Capacity they return their
input lists. `from_root(take optional_root)` publishes them in O(1). Loading is
eager and bottom-up, O(n) nodes plus payload bytes. There are no disk, codec or
unloaded-child operations in omap.

`from_sorted(mut ids,keys,values)` builds bottom-up in O(n) payload work without
path copies. Its borrowed input lists stay unchanged on Capacity. Like the
node constructors, callers supply the invariant: strictly increasing keys
and equally many values. It balances the last groups to retain minimum
occupancy 16, stamps children before parents, and preserves the same node views.
