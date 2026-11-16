# Ordered persistent map

[API overview](../API.md).

`std.omap.OMap[K: ordered + shareable + send, V: shareable + send]` is a
persistent B+ tree with maximum fan-out 32 and minimum non-root occupancy 16.
`new` allocates nothing, `snapshot` shares the root in O(1) without allocation,
and `len` reads its entry count. `get`, `first` and `last` borrow one version.
An update returns a new version while every held version retains its contents.
Keys and values occupy contiguous node lists. Path copies clone their payloads
and share untouched child subtrees. Resources fail the ordinary `shareable` bound.
Versions are sendable; last-holder release frees unreachable nodes and payloads
through the existing constant-stack Arc worklist.

`insert(mut ids,m,k,v)` and `remove(mut ids,m,k)` path-copy, split, merge or
redistribute O(log n) nodes, at most 32 holders per node. `ids` is the writer's
last issued u64, shared across all branches. Every new node receives the next
id after its children; an exhausted counter raises checked Overflow. Capacity
preserves m; insertion returns its taken key/value. Failed attempts may consume
ids, which must never be reused. These APIs continue to borrow m.

`insert_owned(mut ids,take m,take k,take v)` is a separate consuming update.
At a unique Arc, it transfers the owned node through `try_unwrap`, edits its
lists, then publishes it with a new id through `arc.new`; unchanged child
holders move without increments. Shared nodes use the ordinary path-copy.
Unwrap/new still frees and allocates one wrapper per visited unique node.
Capacity consumes the draft m and returns k/v; keep an independent snapshot
for rollback. Every held version and its node ids stay unchanged.

`range(m,take lower,take upper)` accepts `Unbounded`, `Included(k)` or
`Excluded(k)` endpoints and returns an owned, sendable `Cursor`. It reserves one
Arc frame per level, seeks recursively in O(log n), and pins the version from
which it was made. `next(mut cursor)` returns `Result[Option[(K,V)],Capacity]`.
It copies just that row, and Capacity leaves that row unconsumed. Traversing k
rows costs O(k + log n) node work and the cost of copying their payloads.
Reversed bounds yield an empty cursor. A cursor can outlive its originating map
and move to another task. User cursors are read with `while let`, not `for`.

Checkpoint writers use `root` for an Arc holder and `node_id`, `node_level`,
`node_count`, `node_len`, `node_key`, `node_value`, `node_child` for individual
nodes. Node level is zero at leaves. Leaf count is the number of key/value
pairs; branch count is the number of children and their minimum keys. Keys,
values and children outside their count return None. Branches have no values;
leaves have no children. Node fields are stamp:u64, depth:u64, size:u64 and
three 24-byte List headers. Logical node charge is 104 bytes plus the list
capacities times their element sizes: K/V at leaves, K/Arc at branches, plus
owned payload allocations. Full u64/u64 leaves charge 616 bytes. Consuming
updates reserve spare leaf slots explicitly; fan-out by count stays 32.

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
