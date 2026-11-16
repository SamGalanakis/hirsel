# Ordered persistent map

[API overview](../API.md).

`std.omap.OMap[K: ordered + shareable + send, V: shareable + send]` is a
persistent B+ tree with maximum fan-out 32 and minimum non-root occupancy 16.
`new` allocates nothing, `snapshot` shares the root in O(1) without allocation,
and `len` reads its entry count. `get`, `first` and `last` borrow one version.
An update returns a new version while every held version retains its contents.
Leaf keys and values occupy contiguous node lists. Each branch owns one
Arc[List[K]] separator block alongside its child-holder list. Sharing is the
client's choice of leaf payload type, for example Arc[Bytes]. A path copy shares
an unchanged separator block in O(1); changing a child minimum or boundary
creates a new flat block and clones its keys once. Branch comparisons borrow
that contiguous array, with one owner indirection per branch rather than per
comparison. There are no per-row Arcs or duplicated comparison prefixes.
Branch search uses an upper bound with one comparison per step; leaf search
uses a direct less-than comparison. Shared leaf insertion copies each entry
once directly into its final leaf, including both halves of a split. Splitting
a temporary owned leaf moves its entries without cloning their payloads. Resources fail the ordinary `shareable` bound.
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
`replaced_value(old) -> View[V] in old`. The opaque, sendable result moves the
old value out of a unique leaf; for a shared leaf it owns an Arc and slot.
Consequently it can retain the whole old leaf until dropped. It allocates no
extra box. Snapshots keep their previous values. Error results have no replaced
entry. A caller uninterested in the entry binds and drops the second tuple field.

| form | map input | success | Capacity | reason today |
|---|---|---|---|---|
| insert | borrowed | new map, optional old value holder | key/value; input remains valid | persistent update while retaining a version |
| insert_owned | taken | new map, optional old value holder | key/value; draft consumed | unique-node transfer through try_unwrap/new |
| insert_recover | taken | new map, optional old value holder | original map and key/value | strong recovery without fallible rollback |

The two consuming forms differ because checked mutation of a unique Arc payload
is unavailable. With that operation and a failure-atomic update algorithm,
`insert_recover` can take the unique path and `insert_owned` can disappear.
Borrowed `insert` continues to express a persistent update of a retained version.

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
leaves have no children. Node fields are stamp:u64, depth:u64, size:u64, three 24-byte List headers
and lows:Option[Arc[List[K]]]. Leaves have inline keys/values, no children and
None lows. Branches have empty inline keys/values, children and Some lows.
Logical node charge is 120 bytes plus the list capacities times their element
sizes: K/V at leaves, Arc[Node] children at branches. Full u64/u64 leaves charge
632 bytes. A separator block charges 32 bytes for its Arc and List header plus
capacity times K's size and owned payload allocations. Unchanged path copies
share that block; changed boundaries rebuild it. Consuming updates reserve spare
leaf slots explicitly; fan-out by count stays 32. Checkpoint node views still
expose one key per child and no representation-specific holder.

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
